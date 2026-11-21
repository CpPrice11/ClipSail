use reqwest::Url;
use serde::Deserialize;

use crate::discovery::{self, AppResult, Kind, Media, Quality};
use crate::yt_dlp::{self, MetadataError};

pub fn video_id(url: &Url) -> Option<String> {
    if url.scheme() != "https" || url.port().is_some() {
        return None;
    }
    let id = match (url.host_str()?, url.path()) {
        ("youtube.com" | "www.youtube.com" | "m.youtube.com", "/watch") => url
            .query_pairs()
            .find_map(|(key, value)| (key == "v").then(|| value.into_owned()))?,
        ("youtube.com" | "www.youtube.com" | "m.youtube.com", path) => {
            path.strip_prefix("/shorts/")?.to_owned()
        }
        ("youtu.be", path) => path.strip_prefix('/')?.to_owned(),
        _ => return None,
    };
    (id.len() == 11
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)))
    .then_some(id)
}

pub fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

#[derive(Deserialize)]
struct Extracted {
    title: String,
    duration: Option<f64>,
    formats: Vec<ExtractedFormat>,
}

#[derive(Deserialize)]
struct ExtractedFormat {
    url: String,
    format_id: String,
    ext: String,
    protocol: String,
    vcodec: Option<String>,
    acodec: Option<String>,
    has_drm: Option<serde_json::Value>,
    filesize: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    tbr: Option<f64>,
}

pub fn valid_format_id(id: &str) -> bool {
    fn valid_part(part: &str) -> bool {
        !part.is_empty()
            && part.len() <= 48
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    }
    id.split_once('+').map_or_else(
        || valid_part(id),
        |(video, audio)| valid_part(video) && valid_part(audio),
    )
}

fn clear_track(format: &ExtractedFormat) -> Option<Url> {
    let url = discovery::url(&format.url).ok()?;
    if format.has_drm != Some(serde_json::Value::Bool(false))
        || format.protocol != "https"
        || !valid_format_id(&format.format_id)
        || format.format_id.contains('+')
        || url.scheme() != "https"
        || url.port().is_some()
        || !url
            .host_str()
            .is_some_and(|host| host.ends_with(".googlevideo.com"))
    {
        return None;
    }
    Some(url)
}

pub struct Source {
    pub video: Url,
    pub audio: Option<Url>,
}

fn variants(data: &Extracted, merge: bool) -> Vec<(Source, Quality, f64)> {
    let tracks: Vec<_> = data
        .formats
        .iter()
        .filter_map(|format| Some((format, clear_track(format)?)))
        .collect();
    let audio = merge
        .then(|| {
            tracks
                .iter()
                .filter(|(format, _)| {
                    format.ext == "m4a"
                        && format.vcodec.as_deref() == Some("none")
                        && format
                            .acodec
                            .as_deref()
                            .is_some_and(|codec| codec.starts_with("mp4a."))
                })
                .max_by(|(a, _), (b, _)| {
                    a.tbr
                        .unwrap_or_default()
                        .total_cmp(&b.tbr.unwrap_or_default())
                        .then_with(|| {
                            b.format_id
                                .ends_with("-drc")
                                .cmp(&a.format_id.ends_with("-drc"))
                        })
                })
        })
        .flatten();
    let mut formats: Vec<_> = tracks
        .iter()
        .filter_map(|(format, url)| {
            if format.ext != "mp4"
                || !format
                    .vcodec
                    .as_deref()
                    .is_some_and(|codec| !codec.is_empty() && codec != "none")
            {
                return None;
            }
            let (audio_url, format_id, size) = match format.acodec.as_deref() {
                Some("none") => {
                    let (audio, audio_url) = audio?;
                    (
                        Some(audio_url.clone()),
                        format!("{}+{}", format.format_id, audio.format_id),
                        format
                            .filesize
                            .zip(audio.filesize)
                            .and_then(|(a, b)| a.checked_add(b)),
                    )
                }
                Some(codec) if !codec.is_empty() => {
                    (None, format.format_id.clone(), format.filesize)
                }
                _ => return None,
            };
            Some((
                Source {
                    video: url.clone(),
                    audio: audio_url,
                },
                Quality {
                    url: None,
                    source_origin: None,
                    format_id: Some(format_id),
                    extension: "mp4".to_owned(),
                    size,
                    width: format.width,
                    height: format.height,
                },
                format.tbr.unwrap_or_default(),
            ))
        })
        .collect();
    formats.sort_by(|(a_source, a, a_rate), (b_source, b, b_rate)| {
        b.resolution()
            .cmp(&a.resolution())
            .then_with(|| a_source.audio.is_some().cmp(&b_source.audio.is_some()))
            .then_with(|| b_rate.total_cmp(a_rate))
    });
    formats
}

pub async fn resolve(id: &str, format_id: Option<&str>) -> AppResult<Source> {
    let data = extract(id).await?;
    let can_merge = ffmpeg().is_ok();
    if let Some((video, audio)) = format_id.and_then(|id| id.split_once('+')) {
        ffmpeg()?;
        let mut chosen = data;
        chosen
            .formats
            .retain(|format| format.format_id == video || format.format_id == audio);
        return variants(&chosen, true)
            .into_iter()
            .find(|(_, quality, _)| quality.format_id.as_deref() == format_id)
            .map(|(source, _, _)| source)
            .ok_or("qualityUnavailable");
    }
    variants(&data, can_merge)
        .into_iter()
        .find(|(_, quality, _)| format_id.is_none() || quality.format_id.as_deref() == format_id)
        .map(|(url, _, _)| url)
        .ok_or(if format_id.is_some() {
            "qualityUnavailable"
        } else {
            "youtubeUnavailable"
        })
}

pub fn ffmpeg() -> AppResult<std::path::PathBuf> {
    let path = crate::paths::app_folder()
        .ok_or("youtubeTools")?
        .join("tools/ffmpeg/ffmpeg.exe");
    path.is_file().then_some(path).ok_or("youtubeTools")
}

async fn extract(id: &str) -> AppResult<Extracted> {
    let runtime = crate::paths::app_folder()
        .ok_or("youtubeTools")?
        .join("tools/qjs.exe");
    if !runtime.is_file() {
        return Err("youtubeTools");
    }
    let mut js_runtime = std::ffi::OsString::from("quickjs:");
    js_runtime.push(runtime);
    let mut command = yt_dlp::command().map_err(|_| "youtubeTools")?;
    command.arg("--js-runtimes").arg(js_runtime);
    let output = yt_dlp::metadata(command, &watch_url(id), "%(.{title,duration,formats})j")
        .await
        .map_err(|error| match error {
            MetadataError::Tools => "youtubeTools",
            MetadataError::Unavailable => "youtubeUnavailable",
        })?;
    serde_json::from_slice(&output).map_err(|_| "youtubeUnavailable")
}

fn unique_qualities(data: &Extracted) -> Vec<Quality> {
    let mut seen = std::collections::HashSet::new();
    variants(data, ffmpeg().is_ok())
        .into_iter()
        .map(|(_, quality, _)| quality)
        .filter(|quality| seen.insert(quality.resolution()))
        .take(16)
        .collect()
}

pub async fn qualities(id: &str) -> AppResult<Vec<Quality>> {
    let data = extract(id).await?;
    let qualities = unique_qualities(&data);
    if qualities.is_empty() {
        return Err("youtubeUnavailable");
    }
    Ok(qualities)
}

pub async fn find(id: &str) -> AppResult<Vec<Media>> {
    let data = extract(id).await?;
    let qualities = unique_qualities(&data);
    let best = qualities.first().ok_or("youtubeUnavailable")?;
    let poster = format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg");
    Ok(vec![
        Media {
            url: watch_url(id),
            source_origin: None,
            name: data.title.chars().take(120).collect(),
            photo_title: None,
            kind: Kind::Video,
            size: best.size,
            width: best.width,
            height: best.height,
            duration: data.duration,
            preview: Some(poster.clone()),
            preview_only: false,
            page_asset: false,
            qualities,
        },
        Media {
            url: poster.clone(),
            source_origin: None,
            name: "hqdefault.jpg".to_owned(),
            photo_title: None,
            kind: Kind::Image,
            size: None,
            width: None,
            height: None,
            duration: None,
            preview: Some(poster),
            preview_only: false,
            page_asset: false,
            qualities: Vec::new(),
        },
    ])
}
