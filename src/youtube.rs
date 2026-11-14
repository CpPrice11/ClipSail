use std::{process::Stdio, time::Duration};

use reqwest::Url;
use serde::Deserialize;

use crate::discovery::{self, AppResult, Kind, Media, Quality};

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
    let tools = crate::paths::app_folder()
        .ok_or("youtubeTools")?
        .join("tools");
    let runtime = tools.join("qjs.exe");
    if !runtime.is_file() {
        return Err("youtubeTools");
    }
    let mut js_runtime = std::ffi::OsString::from("quickjs:");
    js_runtime.push(runtime);
    let mut command = tokio::process::Command::new(tools.join("yt-dlp.exe"));
    command
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .args([
            "--ignore-config",
            "--no-plugin-dirs",
            "--no-cache-dir",
            "--no-playlist",
            "--no-js-runtimes",
            "--js-runtimes",
        ])
        .arg(js_runtime)
        .args([
            "--socket-timeout",
            "10",
            "--retries",
            "1",
            "--extractor-retries",
            "1",
            "--print",
            "%(.{title,duration,formats})j",
            "--",
        ])
        .arg(watch_url(id));
    let child = command.spawn().map_err(|_| "youtubeTools")?;
    let output = tokio::time::timeout(Duration::from_secs(45), child.wait_with_output())
        .await
        .map_err(|_| "youtubeUnavailable")?
        .map_err(|_| "youtubeUnavailable")?;
    if !output.status.success() {
        return Err("youtubeUnavailable");
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "youtubeUnavailable")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_single_youtube_videos() {
        let id = "abOW-r-NSKs";
        assert_eq!(
            video_id(&Url::parse(&watch_url(id)).unwrap()).as_deref(),
            Some(id)
        );
        assert_eq!(
            video_id(&Url::parse("https://youtu.be/abOW-r-NSKs?t=5").unwrap()).as_deref(),
            Some(id)
        );
        for input in [
            "https://youtube.com.evil.example/watch?v=abOW-r-NSKs",
            "https://www.youtube.com/playlist?list=abOW-r-NSKs",
            "https://www.youtube.com/watch?v=abOW-r-NSK%2F",
        ] {
            assert!(video_id(&Url::parse(input).unwrap()).is_none());
        }
    }

    #[test]
    fn ranks_only_clear_mp4_with_audio_from_youtube_cdn() {
        let good = serde_json::json!({
            "url":"https://r1.googlevideo.com/videoplayback", "has_drm":false,
            "ext":"mp4", "protocol":"https", "format_id":"18", "acodec":"aac",
            "vcodec":"avc1", "height":360
        });
        let mut formats = vec![good.clone()];
        let mut high = good.clone();
        high["height"] = 720.into();
        high["format_id"] = "22".into();
        formats.push(high.clone());
        let mut separate = high.clone();
        separate["acodec"] = "none".into();
        separate["format_id"] = "137".into();
        separate["height"] = 1080.into();
        let mut audio = good.clone();
        audio["vcodec"] = "none".into();
        audio["acodec"] = "mp4a.40.2".into();
        audio["ext"] = "m4a".into();
        audio["format_id"] = "140".into();
        formats.extend([separate, audio]);
        for (field, value) in [
            ("has_drm", true.into()),
            ("has_drm", serde_json::Value::Null),
            ("url", "https://googlevideo.com.evil.example/file".into()),
        ] {
            let mut rejected = good.clone();
            rejected[field] = value;
            rejected["height"] = 2160.into();
            formats.push(rejected);
        }
        let data = serde_json::from_value(serde_json::json!({
            "title": "Video", "formats": formats
        }))
        .unwrap();
        let qualities: Vec<_> = variants(&data, false)
            .into_iter()
            .map(|(_, quality, _)| quality.format_id.unwrap())
            .collect();
        assert_eq!(qualities, ["22", "18"]);
        let merged = variants(&data, true);
        assert_eq!(merged[0].1.format_id.as_deref(), Some("137+140"));
        assert!(merged[0].0.audio.is_some());
        assert_eq!(merged[0].1.resolution(), 1080);
    }
}
