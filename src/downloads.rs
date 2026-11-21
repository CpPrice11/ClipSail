use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{Client, Response, StatusCode, header::*};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};
use uuid::Uuid;

use crate::{
    dash, direct,
    discovery::{self, AppResult, Kind},
    extractor, hls, youtube,
};

const MIB: u64 = 1024 * 1024;
const QUEUE_LIMIT: usize = 1000;

#[derive(Deserialize)]
pub struct Selection {
    pub url: String,
    #[serde(default)]
    pub name: String,
    pub size: Option<u64>,
    #[serde(default)]
    pub preview_only: bool,
    pub format_id: Option<String>,
    pub source_origin: Option<String>,
    #[serde(default)]
    pub direct: bool,
}

pub fn prepare_selection(
    items: Vec<Selection>,
    folder: &Path,
    existing: &[Job],
) -> AppResult<Vec<Job>> {
    if items.is_empty() || items.len() > discovery::GALLERY_LIMIT {
        return Err("selection");
    }
    if !folder.is_dir() {
        return Err("disk");
    }
    let mut urls: HashSet<_> = existing
        .iter()
        .filter(|job| job.folder == folder && job.status != Status::Completed)
        .map(|job| {
            (
                job.url.clone(),
                job.format_id.clone(),
                job.source_origin.clone(),
            )
        })
        .collect();
    let mut pending = Vec::new();
    for item in items {
        if item.preview_only {
            return Err("unsupported");
        }
        let url = discovery::url(&item.url)?;
        let source_origin = item
            .source_origin
            .as_deref()
            .map(discovery::url)
            .transpose()?
            .and_then(|source| discovery::media_referer(&source, &url));
        if let Some(id) = &item.format_id
            && (item.direct
                || !(extractor::valid_format_id(id)
                    || hls::valid_format_id(id)
                    || dash::valid_format_id(id)
                    || youtube::video_id(&url).is_some() && youtube::valid_format_id(id)))
        {
            return Err("qualityUnavailable");
        }
        let address = url.to_string();
        if !urls.insert((
            address.clone(),
            item.format_id.clone(),
            source_origin.clone(),
        )) {
            continue;
        }
        let name: String = item
            .name
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect();
        pending.push(Job {
            id: Uuid::new_v4(),
            url: address,
            name: if name.is_empty() {
                "media".to_owned()
            } else {
                name
            },
            folder: folder.to_owned(),
            file: None,
            done: 0,
            total: item.size,
            status: Status::Queued,
            error: None,
            archived: false,
            format_id: item.format_id,
            browser_origin: None,
            source_origin,
            direct: item.direct,
        });
    }
    if pending.is_empty() {
        return Err("alreadyQueued");
    }
    if existing.iter().filter(|job| !job.archived).count() + pending.len() > QUEUE_LIMIT {
        return Err("queueFull");
    }
    Ok(pending)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub url: String,
    pub name: String,
    pub folder: PathBuf,
    pub file: Option<String>,
    pub done: u64,
    pub total: Option<u64>,
    pub status: Status,
    pub error: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_origin: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Deferred,
    Running,
    Pausing,
    Paused,
    Completed,
    Failed,
}

#[derive(Serialize, Deserialize, PartialEq)]
struct Plan {
    url: String,
    name: String,
    etag: Option<String>,
    total: Option<u64>,
    segments: Vec<(u64, u64)>,
}

pub enum Update {
    Progress(u64, Option<u64>),
    Completed(String, u64),
}

pub fn write_json(path: &Path, value: &impl Serialize) -> AppResult<()> {
    use std::io::Write;
    let mut temp =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("disk")?).map_err(|_| "disk")?;
    serde_json::to_writer(temp.as_file_mut(), value).map_err(|_| "disk")?;
    temp.flush().map_err(|_| "disk")?;
    temp.as_file().sync_all().map_err(|_| "disk")?;
    temp.persist(path).map_err(|_| "disk")?;
    Ok(())
}

async fn wait<T, E>(
    stop: &mut watch::Receiver<bool>,
    future: impl Future<Output = Result<T, E>>,
    failure: &'static str,
) -> AppResult<T> {
    if *stop.borrow() {
        return Err("paused");
    }
    tokio::select! {
        biased;
        _ = stop.changed() => Err("paused"),
        result = future => result.map_err(|_| failure),
    }
}

fn etag(response: &Response) -> Option<String> {
    let value = response.headers().get(ETAG)?.to_str().ok()?;
    (value.starts_with('"') && value.ends_with('"') && value.len() >= 2).then(|| value.to_owned())
}

pub(crate) fn content_range(response: &Response) -> Option<(u64, u64, u64)> {
    let value = response
        .headers()
        .get(CONTENT_RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes ")?;
    let (range, length) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, length.parse().ok()?))
}

fn part(dir: &Path, number: usize) -> PathBuf {
    dir.join(format!("{number}.part"))
}

fn status_ok(response: &Response) -> AppResult<()> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err("httpError")
    }
}

#[derive(Serialize)]
pub struct DirectInfo {
    name: String,
    size: Option<u64>,
}

fn direct_status(response: &Response) -> AppResult<()> {
    match response.status() {
        StatusCode::UNAUTHORIZED => Err("pageUnauthorized"),
        StatusCode::FORBIDDEN => Err("pageForbidden"),
        _ => status_ok(response),
    }
}

pub async fn inspect_direct(client: &Client, url: &str) -> AppResult<DirectInfo> {
    let url = discovery::url(url)?;
    let mut response = client
        .head(url.clone())
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|_| "network")?;
    if matches!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED | StatusCode::FORBIDDEN
    ) || (response.status().is_success()
        && !response.headers().contains_key(CONTENT_TYPE)
        && !response.headers().contains_key(CONTENT_DISPOSITION))
    {
        response = client
            .get(url.clone())
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, "bytes=0-0")
            .send()
            .await
            .map_err(|_| "network")?;
        if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
            response = client
                .get(url)
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(|_| "network")?;
        }
    }
    direct_status(&response)?;
    let size = if response.status() == StatusCode::PARTIAL_CONTENT {
        content_range(&response).map(|(_, _, size)| size)
    } else {
        response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse().ok())
    };
    Ok(DirectInfo {
        name: direct::name(&response)?,
        size,
    })
}

fn media_download_name(
    response: &Response,
    job: &Job,
    is_youtube: bool,
    is_extracted: bool,
) -> AppResult<(String, &'static str)> {
    let (extension, kind) = discovery::response_format(response).ok_or("unsupported")?;
    let name = if is_youtube {
        if (extension, kind) != ("mp4", Kind::Video) {
            return Err("unsupported");
        }
        let name = if job.browser_origin.is_some() {
            job.name.strip_suffix(".mp4").unwrap_or(&job.name)
        } else {
            &job.name
        };
        discovery::safe_media_name(name, extension)
    } else if is_extracted {
        if kind != Kind::Video {
            return Err("unsupported");
        }
        let stem = job
            .name
            .strip_suffix(&format!(".{extension}"))
            .unwrap_or(&job.name);
        discovery::safe_media_name(stem, extension)
    } else if job.browser_origin.is_some() {
        if discovery::file_format(&job.name).map(|(_, declared)| declared) != Some(kind) {
            return Err("unsupported");
        }
        let stem = job
            .name
            .rsplit_once('.')
            .map_or(job.name.as_str(), |(stem, _)| stem);
        discovery::safe_media_name(stem, extension)
    } else {
        discovery::media_name(response.url(), extension)
    };
    Ok((name, extension))
}

async fn save_youtube_track(
    client: &Client,
    url: &reqwest::Url,
    path: &Path,
    total: u64,
    stop: &mut watch::Receiver<bool>,
    progress: impl Fn(u64),
) -> AppResult<()> {
    if total == 0
        || url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|host| host.ends_with(".googlevideo.com"))
    {
        return Err("changed");
    }
    File::create(path).await.map_err(|_| "disk")?;
    let mut start = 0;
    while start < total {
        let end = start.saturating_add(4 * MIB).min(total) - 1;
        let request = client
            .get(url.clone())
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, format!("bytes={start}-{end}"));
        let response = wait(
            stop,
            request.timeout(Duration::from_secs(30)).send(),
            "network",
        )
        .await?;
        status_ok(&response)?;
        if response.url().scheme() != "https"
            || !response
                .url()
                .host_str()
                .is_some_and(|host| host.ends_with(".googlevideo.com"))
            || response.status() != StatusCode::PARTIAL_CONTENT
            || content_range(&response) != Some((start, end, total))
            || response
                .headers()
                .get(CONTENT_ENCODING)
                .is_some_and(|v| v != "identity")
        {
            return Err("changed");
        }
        save(response, path, start, Some(end + 1), stop, &progress).await?;
        start = end + 1;
    }
    Ok(())
}

pub async fn run(
    client: &Client,
    job: &Job,
    mut stop: watch::Receiver<bool>,
    report: impl Fn(Update) + Send + Sync,
) -> AppResult<()> {
    let url = discovery::url(&job.url)?;
    let saved_format = job.format_id.as_deref();
    let extracted =
        match saved_format.filter(|value| !job.direct && extractor::valid_format_id(value)) {
            Some(id) => Some(tokio::select! {
                biased;
                _ = stop.changed() => return Err("paused"),
                result = extractor::resolve(&url, id) => result?,
            }),
            None => None,
        };
    let format_id = extracted
        .as_ref()
        .map_or(saved_format, |source| source.stream_id.as_deref());
    let url = extracted.as_ref().map_or(url, |source| source.url.clone());
    let referer = extracted.as_ref().map_or_else(
        || {
            job.browser_origin
                .as_deref()
                .or(job.source_origin.as_deref())
        },
        |source| source.referer.as_deref(),
    );
    let specialized_client = extracted
        .as_ref()
        .and_then(|source| source.user_agent.as_deref())
        .map(discovery::client_with_user_agent)
        .transpose()?;
    let client = specialized_client.as_ref().unwrap_or(client);
    let dash_stream = dash::is_stream(&url, format_id);
    let stream = !job.direct
        && if extracted.is_some() {
            format_id.is_some()
        } else {
            dash_stream || hls::is_playlist(&url) || format_id.is_some_and(hls::valid_format_id)
        };
    let youtube_id = (!job.direct && !stream && extracted.is_none())
        .then(|| youtube::video_id(&url))
        .flatten();
    let (url, audio) = match &youtube_id {
        Some(id) => {
            tokio::select! {
                biased;
                _ = stop.changed() => return Err("paused"),
                result = youtube::resolve(id, job.format_id.as_deref()) => {
                    let source = result?;
                    (source.video, source.audio)
                },
            }
        }
        None => (url, None),
    };
    let dir = job.folder.join(format!(".media-{}", job.id));
    fs::create_dir_all(&dir).map_err(|_| "disk")?;
    if stream {
        let download = async {
            let progress = |bytes| report(Update::Progress(bytes, None));
            if dash_stream {
                dash::save(client, url, format_id, referer, &dir, progress).await
            } else {
                hls::save(client, url, format_id, referer, &dir, progress).await
            }
        };
        let result = tokio::select! {
            biased;
            _ = stop.changed() => Err("paused"),
            result = download => result,
        };
        let final_result = result.and_then(|bytes| {
            if *stop.borrow() {
                return Err("paused");
            }
            let stem = job
                .name
                .rsplit_once('.')
                .filter(|(_, extension)| extension.eq_ignore_ascii_case("mp4"))
                .map_or(job.name.as_str(), |(stem, _)| stem);
            let name = discovery::safe_media_name(stem, "mp4");
            let name = publish_download(&dir.join("stream.mp4"), &job.folder, &name)?;
            report(Update::Completed(name, bytes));
            Ok(())
        });
        if final_result.is_ok() {
            let _ = fs::remove_dir_all(&dir);
        }
        return final_result;
    }
    let manifest = dir.join("parts.json");
    let old: Option<Plan> = fs::read(&manifest)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let get = |url: reqwest::Url| {
        let request = client.get(url);
        match referer {
            Some(origin) => request.header(REFERER, origin),
            None => request,
        }
    };
    let request = get(url.clone())
        .header(ACCEPT_ENCODING, "identity")
        .header(RANGE, "bytes=0-0");
    let mut response = wait(&mut stop, request.send(), "network").await?;
    if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
        response = wait(
            &mut stop,
            get(url).header(ACCEPT_ENCODING, "identity").send(),
            "network",
        )
        .await?;
    }
    let (name, media_extension) = if job.direct {
        direct_status(&response)?;
        (direct::name(&response)?, None)
    } else {
        status_ok(&response)?;
        let (name, extension) =
            media_download_name(&response, job, youtube_id.is_some(), extracted.is_some())?;
        (name, Some(extension))
    };
    if response.headers().contains_key(CONTENT_ENCODING)
        && response.headers()[CONTENT_ENCODING] != "identity"
    {
        return Err("unsupported");
    }

    let ranged = response.status() == StatusCode::PARTIAL_CONTENT;
    let total = if ranged {
        let (0, 0, total) = content_range(&response).ok_or("changed")? else {
            return Err("changed");
        };
        if total == 0 {
            return Err("changed");
        }
        Some(total)
    } else {
        response.content_length()
    };
    let validator = etag(&response);
    let resumable = ranged && validator.is_some();
    let segments = if let Some(total) = total.filter(|_| resumable) {
        let count = if total >= 8 * MIB { 4 } else { 1 };
        let span = total.div_ceil(count);
        (0..total)
            .step_by(span as usize)
            .map(|start| (start, start.saturating_add(span).min(total) - 1))
            .collect()
    } else {
        Vec::new()
    };
    let plan = Plan {
        url: response.url().to_string(),
        name,
        etag: validator,
        total,
        segments,
    };

    // Only the same validated representation can reuse bytes from a previous run.
    if !resumable || old.as_ref() != Some(&plan) {
        for n in 0..4 {
            match fs::remove_file(part(&dir, n)) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err("disk"),
            }
        }
    }
    write_json(&manifest, &plan)?;
    let lengths: Vec<u64> = plan
        .segments
        .iter()
        .enumerate()
        .map(|(n, _)| fs::metadata(part(&dir, n)).map(|m| m.len()).unwrap_or(0))
        .collect();
    let counters = lengths
        .iter()
        .copied()
        .map(std::sync::atomic::AtomicU64::new)
        .collect::<Vec<_>>();
    use std::sync::atomic::Ordering::Relaxed;
    let progress = |n: usize, bytes: u64| {
        counters[n].store(bytes, Relaxed);
        report(Update::Progress(
            counters.iter().map(|value| value.load(Relaxed)).sum(),
            total,
        ));
    };
    report(Update::Progress(lengths.iter().sum(), total));

    if resumable {
        let jobs = plan
            .segments
            .iter()
            .copied()
            .enumerate()
            .map(|(n, (start, end))| {
                let mut stop = stop.clone();
                let path = part(&dir, n);
                let existing = lengths[n];
                let plan = &plan;
                let progress = &progress;
                async move {
                    let size = end - start + 1;
                    if existing == size {
                        return Ok(());
                    }
                    if existing > size {
                        return Err("changed");
                    }
                    let request = get(discovery::url(&plan.url)?)
                        .header(ACCEPT_ENCODING, "identity")
                        .header(RANGE, format!("bytes={}-{}", start + existing, end))
                        .header(
                            IF_RANGE,
                            plan.etag.as_ref().expect("validated representation"),
                        );
                    let response = wait(&mut stop, request.send(), "network").await?;
                    status_ok(&response)?;
                    if response.url().as_str() != plan.url
                        || response.status() != StatusCode::PARTIAL_CONTENT
                        || etag(&response) != plan.etag
                        || content_range(&response)
                            != Some((start + existing, end, plan.total.expect("known length")))
                        || response
                            .headers()
                            .get(CONTENT_ENCODING)
                            .is_some_and(|v| v != "identity")
                    {
                        return Err("changed");
                    }
                    save(response, &path, existing, Some(size), &mut stop, |bytes| {
                        progress(n, bytes)
                    })
                    .await?;
                    Ok(())
                }
            });
        // Wait for every writer to stop before allowing another attempt on these files.
        let results = futures_util::future::join_all(jobs).await;
        for result in results {
            result?;
        }
    } else if ranged && youtube_id.is_some() {
        // YouTube may stall an unbounded request for a large track.
        drop(response);
        let total = plan.total.ok_or("changed")?;
        let url = discovery::url(&plan.url)?;
        save_youtube_track(client, &url, &part(&dir, 0), total, &mut stop, |done| {
            report(Update::Progress(done, Some(total)))
        })
        .await?;
    } else {
        if ranged {
            response = wait(
                &mut stop,
                get(discovery::url(&plan.url)?)
                    .header(ACCEPT_ENCODING, "identity")
                    .send(),
                "network",
            )
            .await?;
            status_ok(&response)?;
        }
        let same_file = if let Some(extension) = media_extension {
            discovery::response_format(&response).map(|(ext, _)| ext) == Some(extension)
        } else {
            direct::name(&response)? == plan.name
        };
        if response.status() != StatusCode::OK
            || !same_file
            || response
                .headers()
                .get(CONTENT_ENCODING)
                .is_some_and(|v| v != "identity")
        {
            return Err("changed");
        }
        let size = response.content_length();
        save(response, &part(&dir, 0), 0, size, &mut stop, |bytes| {
            report(Update::Progress(bytes, size))
        })
        .await?;
    }

    let count = plan.segments.len().max(1);
    let mut output = dir.join("complete.tmp");
    let mut target = File::create(&output).await.map_err(|_| "disk")?;
    let mut bytes = 0;
    for n in 0..count {
        let mut source = File::open(part(&dir, n)).await.map_err(|_| "disk")?;
        let mut buffer = vec![0; MIB as usize];
        loop {
            let count = wait(&mut stop, source.read(&mut buffer), "disk").await?;
            if count == 0 {
                break;
            }
            target
                .write_all(&buffer[..count])
                .await
                .map_err(|_| "disk")?;
            bytes += count as u64;
        }
    }
    target.sync_all().await.map_err(|_| "disk")?;
    drop(target);
    if let Some(audio) = audio {
        let audio_path = dir.join("audio.tmp");
        File::create(&audio_path).await.map_err(|_| "disk")?;
        let response = wait(
            &mut stop,
            get(audio)
                .header(ACCEPT_ENCODING, "identity")
                .header(RANGE, "bytes=0-0")
                .send(),
            "network",
        )
        .await?;
        status_ok(&response)?;
        if !matches!(
            discovery::response_format(&response),
            Some(("m4a", Kind::Audio))
        ) || response
            .headers()
            .get(CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err("unsupported");
        }
        if response.status() == StatusCode::PARTIAL_CONTENT {
            let (0, 0, audio_size) = content_range(&response).ok_or("changed")? else {
                return Err("changed");
            };
            let url = response.url().clone();
            drop(response);
            save_youtube_track(client, &url, &audio_path, audio_size, &mut stop, |done| {
                report(Update::Progress(
                    bytes + done,
                    bytes.checked_add(audio_size),
                ))
            })
            .await?;
        } else {
            let audio_size = response.content_length();
            save(response, &audio_path, 0, audio_size, &mut stop, |done| {
                report(Update::Progress(
                    bytes + done,
                    audio_size.and_then(|size| bytes.checked_add(size)),
                ))
            })
            .await?;
        }
        let merged = dir.join("merged.tmp");
        let mut command = tokio::process::Command::new(youtube::ffmpeg()?);
        command
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .args([
                "-nostdin",
                "-v",
                "error",
                "-y",
                "-protocol_whitelist",
                "file",
                "-f",
                "mov",
                "-enable_drefs",
                "0",
                "-i",
            ])
            .arg(&output)
            .args([
                "-protocol_whitelist",
                "file",
                "-f",
                "mov",
                "-enable_drefs",
                "0",
                "-i",
            ])
            .arg(&audio_path)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c",
                "copy",
                "-movflags",
                "+faststart",
                "-f",
                "mp4",
            ])
            .arg(&merged);
        let status = wait(&mut stop, command.status(), "mergeFailed").await?;
        if !status.success() {
            return Err("mergeFailed");
        }
        bytes = fs::metadata(&merged).map_err(|_| "disk")?.len();
        if bytes == 0 {
            return Err("mergeFailed");
        }
        output = merged;
    }
    if *stop.borrow() {
        return Err("paused");
    }
    let final_name = publish_download(&output, &job.folder, &plan.name)?;
    report(Update::Completed(final_name, bytes));
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

fn publish_download(output: &Path, folder: &Path, file_name: &str) -> AppResult<String> {
    let (stem, ext) = file_name
        .rsplit_once('.')
        .map_or((file_name, None), |(stem, ext)| (stem, Some(ext)));
    (0..1000)
        .find_map(|number| {
            let name = if number == 0 {
                file_name.to_owned()
            } else {
                let suffix = format!("{stem} ({number})");
                ext.map_or_else(|| suffix.clone(), |ext| format!("{suffix}.{ext}"))
            };
            let target = folder.join(&name);
            let published = fs::hard_link(output, &target).or_else(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    Err(error)
                } else {
                    publish_copy(output, &target)
                }
            });
            match published {
                Ok(()) => Some(Ok(name)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(_) => Some(Err("disk")),
            }
        })
        .ok_or("disk")?
}

fn publish_copy(source: &Path, target: &Path) -> std::io::Result<()> {
    // Persist a complete temporary file without overwriting an existing download.
    let mut temp =
        tempfile::NamedTempFile::new_in(target.parent().ok_or(std::io::ErrorKind::InvalidInput)?)?;
    std::io::copy(&mut fs::File::open(source)?, temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(target)
        .map_err(|failure| failure.error)?;
    Ok(())
}

async fn save(
    mut response: Response,
    path: &Path,
    existing: u64,
    expected: Option<u64>,
    stop: &mut watch::Receiver<bool>,
    report: impl Fn(u64),
) -> AppResult<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|_| "disk")?;
    let mut done = existing;
    let mut last_update = std::time::Instant::now() - Duration::from_secs(1);
    while let Some(chunk) = wait(stop, response.chunk(), "network").await? {
        if expected.is_some_and(|n| done + chunk.len() as u64 > n) {
            return Err("changed");
        }
        file.write_all(&chunk).await.map_err(|_| "disk")?;
        done += chunk.len() as u64;
        if last_update.elapsed() >= Duration::from_millis(125) {
            report(done);
            last_update = std::time::Instant::now();
        }
    }
    if expected.is_some_and(|n| n != done) {
        return Err("network");
    }
    file.flush().await.map_err(|_| "disk")?;
    file.sync_all().await.map_err(|_| "disk")?;
    report(done);
    Ok(())
}
