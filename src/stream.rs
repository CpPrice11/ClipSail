use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};

use reqwest::{
    Client, StatusCode, Url,
    header::{ACCEPT_ENCODING, CONTENT_ENCODING, RANGE, REFERER},
};

use crate::{
    discovery::{self, AppResult},
    downloads, youtube,
};

pub const SEGMENT_LIMIT: usize = 32 * 1024 * 1024;

pub fn relative(base: &Url, value: &str) -> AppResult<Url> {
    let address = base.join(value).map_err(|_| "unsupported")?;
    let address = discovery::url(address.as_str())?;
    if base.scheme() == "https" && address.scheme() != "https" {
        return Err("unsupported");
    }
    Ok(address)
}

pub struct Segment {
    pub url: Url,
    pub range: Option<(u64, u64)>,
}

pub struct Track {
    pub initialization: Option<Segment>,
    pub segments: Vec<Segment>,
    pub sequence: u64,
    pub seconds: Option<f64>,
}

pub async fn get(
    client: &Client,
    url: Url,
    referer: Option<&str>,
    timeout: Duration,
    range: Option<(u64, u64)>,
) -> AppResult<reqwest::Response> {
    let mut request = client
        .get(url)
        .timeout(timeout)
        .header(ACCEPT_ENCODING, "identity");
    if let Some(value) = referer {
        request = request.header(REFERER, value);
    }
    if let Some((start, end)) = range {
        request = request.header(RANGE, format!("bytes={start}-{end}"));
    }
    let response = request.send().await.map_err(|_| "network")?;
    let status = response.status();
    if matches!(status, StatusCode::FORBIDDEN | StatusCode::GONE) {
        return Err("changed");
    }
    if status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
    {
        return Err("network");
    }
    let response = response.error_for_status().map_err(|_| "httpError")?;
    let matches_range = match range {
        Some((start, end)) => {
            status == StatusCode::PARTIAL_CONTENT
                && downloads::content_range(&response).is_some_and(|(from, through, len)| {
                    (start, end) == (from, through) && through < len
                })
        }
        None => status == StatusCode::OK,
    };
    if !matches_range
        || response
            .headers()
            .get(CONTENT_ENCODING)
            .is_some_and(|value| value != "identity")
    {
        return Err("changed");
    }
    Ok(response)
}

pub fn clear_mp4_bytes(mut data: &[u8], initialization: bool) -> bool {
    let (metadata, payload): (&[u8; 4], &[u8; 4]) = if initialization {
        (b"moov", b"ftyp")
    } else {
        (b"moof", b"mdat")
    };
    let mut found_metadata = false;
    let mut found_payload = false;
    while let Some(header) = data.first_chunk::<8>() {
        let mut size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        if size == 1 {
            let Some(extended) = data.get(8..16) else {
                return false;
            };
            let Ok(extended_size) =
                usize::try_from(u64::from_be_bytes(extended.try_into().unwrap()))
            else {
                return false;
            };
            if extended_size < 16 {
                return false;
            }
            size = extended_size;
        } else if size == 0 {
            size = data.len();
        }
        if size < 8 || size > data.len() {
            return false;
        }
        let kind = &header[4..];
        if kind == b"pssh" {
            return false;
        }
        if kind == metadata {
            if data[8..size]
                .windows(4)
                .any(|box_type| matches!(box_type, b"pssh" | b"sinf" | b"encv" | b"enca" | b"senc"))
            {
                return false;
            }
            found_metadata = true;
        }
        found_payload |= kind == payload;
        data = &data[size..];
    }
    data.is_empty() && found_metadata && found_payload
}

impl Track {
    fn parts(&self) -> impl Iterator<Item = &Segment> {
        self.initialization.iter().chain(&self.segments)
    }

    fn identity(&self) -> serde_json::Value {
        let parts: Vec<_> = self
            .parts()
            .map(|part| (part.url.as_str(), part.range))
            .collect();
        serde_json::json!({
            "parts": parts,
            "initialization": self.initialization.is_some(),
            "sequence": self.sequence,
            "seconds": self.seconds,
        })
    }

    fn input_format(&self) -> &'static str {
        if self.initialization.is_some() {
            "mov"
        } else {
            "mpegts"
        }
    }
}

async fn fetch_part(client: &Client, part: &Segment, referer: Option<&str>) -> AppResult<Vec<u8>> {
    for delay in [Some(300), Some(900), None] {
        let result = async {
            let mut response = get(
                client,
                part.url.clone(),
                referer,
                Duration::from_secs(60),
                part.range,
            )
            .await?;
            let expected = part
                .range
                .map(|(start, end)| end - start + 1)
                .or_else(|| response.content_length());
            let bytes =
                discovery::read_limited(&mut response, SEGMENT_LIMIT, "unsupported").await?;
            if expected.is_some_and(|len| len != bytes.len() as u64) {
                return Err("network");
            }
            Ok(bytes)
        }
        .await;
        if let (Err("network"), Some(delay)) = (&result, delay) {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        } else {
            return result;
        }
    }
    Err("network")
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Checkpoint {
    segments: usize,
    bytes: u64,
}

async fn save_track(
    client: &Client,
    track: &Track,
    referer: Option<&str>,
    dir: &Path,
    name: &str,
    previous_bytes: u64,
    progress: &impl Fn(u64),
) -> AppResult<u64> {
    let input = dir.join(format!("{name}.track"));
    let state = dir.join(format!("{name}.json"));
    let mut checkpoint = std::fs::read(&state)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Checkpoint>(&bytes).ok())
        .filter(|checkpoint| {
            checkpoint.segments <= track.parts().count()
                && std::fs::metadata(&input).is_ok_and(|file| file.len() >= checkpoint.bytes)
        })
        .unwrap_or_default();
    let mut target = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&input)
        .map_err(|_| "disk")?;
    // Bytes written just before an interruption are committed only with the checkpoint.
    target.set_len(checkpoint.bytes).map_err(|_| "disk")?;
    target
        .seek(SeekFrom::Start(checkpoint.bytes))
        .map_err(|_| "disk")?;
    progress(previous_bytes + checkpoint.bytes);
    for (index, part) in track.parts().enumerate().skip(checkpoint.segments) {
        let data = fetch_part(client, part, referer).await?;
        let valid = if track.initialization.is_some() {
            clear_mp4_bytes(&data, index == 0)
        } else {
            data.len() >= 188 && data[0] == 0x47 && data.len() % 188 == 0
        };
        if !valid {
            return Err("unsupported");
        }
        target.write_all(&data).map_err(|_| "disk")?;
        target.sync_all().map_err(|_| "disk")?;
        checkpoint.segments += 1;
        checkpoint.bytes += data.len() as u64;
        downloads::write_json(&state, &checkpoint)?;
        progress(previous_bytes + checkpoint.bytes);
    }
    Ok(checkpoint.bytes)
}

pub async fn save(
    client: &Client,
    video: &Track,
    audio: Option<&Track>,
    referer: Option<&str>,
    cache: &Path,
    progress: impl Fn(u64),
) -> AppResult<u64> {
    let ffmpeg = youtube::ffmpeg()?;
    let dir = cache.parent().ok_or("disk")?;
    let manifest = cache.join("parts.json");
    let plan = serde_json::json!({"video": video.identity(), "audio": audio.map(Track::identity)});
    let previous = std::fs::read(&manifest)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if previous.as_ref() != Some(&plan) {
        if cache.exists() {
            std::fs::remove_dir_all(cache).map_err(|_| "disk")?;
        }
        std::fs::create_dir(cache).map_err(|_| "disk")?;
        downloads::write_json(&manifest, &plan)?;
    }
    let video_bytes = save_track(client, video, referer, cache, "video", 0, &progress).await?;
    if let Some(audio) = audio {
        save_track(
            client,
            audio,
            referer,
            cache,
            "audio",
            video_bytes,
            &progress,
        )
        .await?;
    }
    let output = dir.join("stream.mp4");
    let mut command = tokio::process::Command::new(ffmpeg);
    command
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .args(["-nostdin", "-y", "-xerror"]);
    for (name, track) in [Some(("video", video)), audio.map(|track| ("audio", track))]
        .into_iter()
        .flatten()
    {
        command.args(["-protocol_whitelist", "file", "-f", track.input_format()]);
        if track.initialization.is_some() {
            command.args(["-enable_drefs", "0"]);
        }
        command.arg("-i").arg(cache.join(format!("{name}.track")));
    }
    let audio_stream = if audio.is_some() { "1:a:0" } else { "0:a:0?" };
    let status = command
        .args([
            "-map",
            "0:v:0",
            "-map",
            audio_stream,
            "-c",
            "copy",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ])
        .arg(&output)
        .status()
        .await
        .map_err(|_| "mergeFailed")?;
    let bytes = tokio::fs::metadata(output)
        .await
        .map_err(|_| "mergeFailed")?
        .len();
    if !status.success() || bytes == 0 {
        return Err("mergeFailed");
    }
    Ok(bytes)
}
