use std::{path::Path, process::Stdio, time::Duration};

use futures_util::{StreamExt, stream};
use reqwest::{
    Client, StatusCode, Url,
    header::{RANGE, REFERER},
};
use tokio::io::AsyncWriteExt;

use crate::{
    discovery::{self, AppResult},
    downloads,
    stream::{SEGMENT_LIMIT, Segment, Track as MediaPlaylist, clear_mp4_bytes, get, relative},
    youtube,
};

pub fn is_playlist(url: &Url) -> bool {
    discovery::is_hls(url)
}

enum Playlist {
    Master(Vec<Variant>),
    Media(MediaPlaylist),
}

struct Variant {
    address: Url,
    audio: Option<Url>,
    dimensions: Option<(u32, u32)>,
    bandwidth: u64,
}

impl Variant {
    fn id(&self) -> String {
        let (width, height) = self.dimensions.unwrap_or_default();
        format!("hls:{}:{width}:{height}", self.bandwidth)
    }
}

pub fn valid_format_id(value: &str) -> bool {
    if value == "hls:default" {
        return true;
    }
    let Some(value) = value.strip_prefix("hls:").filter(|value| value.len() <= 42) else {
        return false;
    };
    let mut parts = value.split(':');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(rate), Some(width), Some(height), None)
            if rate.parse::<u64>().is_ok_and(|rate| rate > 0)
                && width.parse::<u32>().is_ok() && height.parse::<u32>().is_ok()
    )
}

fn attributes(mut line: &str) -> AppResult<std::collections::HashMap<&str, &str>> {
    let mut out = std::collections::HashMap::new();
    while !line.is_empty() {
        let (key, value) = line.split_once('=').ok_or("unsupported")?;
        let (value, rest) = if let Some(quoted) = value.strip_prefix('"') {
            quoted.split_once('"').ok_or("unsupported")?
        } else if let Some((value, _)) = value.split_once(',') {
            (value, &line[key.len() + 1 + value.len()..])
        } else {
            (value, "")
        };
        if key.is_empty() || value.is_empty() || out.insert(key, value).is_some() {
            return Err("unsupported");
        }
        line = if rest.is_empty() {
            ""
        } else {
            rest.strip_prefix(',').ok_or("unsupported")?
        };
    }
    Ok(out)
}

fn dimensions(value: &str) -> Option<(u32, u32)> {
    let (w, h) = value.split_once('x')?;
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

fn parse_master(base: &Url, lines: impl Iterator<Item = impl AsRef<str>>) -> AppResult<Playlist> {
    let mut audio = std::collections::HashMap::new();
    let mut variants = Vec::new();
    let mut next = None;
    for value in lines {
        let line = value.as_ref();
        if let Some(tag) = line.strip_prefix("#EXT-X-MEDIA:") {
            let fields = attributes(tag)?;
            if fields.get("TYPE") == Some(&"AUDIO") {
                let group = *fields.get("GROUP-ID").ok_or("unsupported")?;
                let address = fields
                    .get("URI")
                    .map(|uri| relative(base, uri))
                    .transpose()?;
                let priority = if fields.get("DEFAULT") == Some(&"YES") {
                    2
                } else {
                    u8::from(fields.get("AUTOSELECT") == Some(&"YES"))
                };
                match audio.entry(group.to_owned()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert((priority, address));
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry)
                        if priority > entry.get().0 =>
                    {
                        entry.insert((priority, address));
                    }
                    _ => {}
                }
            }
        } else if let Some(tag) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if next.is_some() || variants.len() >= 32 {
                return Err("unsupported");
            }
            let fields = attributes(tag)?;
            let bandwidth = fields
                .get("BANDWIDTH")
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or("unsupported")?;
            next = Some((
                fields.get("RESOLUTION").and_then(|value| dimensions(value)),
                bandwidth,
                fields.get("AUDIO").map(|group| (*group).to_owned()),
            ));
        } else if line.starts_with('#') {
            if line.starts_with("#EXT-X-SESSION-KEY") || line.starts_with("#EXT-X-KEY") {
                return Err("unsupported");
            }
        } else {
            let (dimensions, bandwidth, group) = next.take().ok_or("unsupported")?;
            variants.push((relative(base, line)?, dimensions, bandwidth, group));
        }
    }
    if next.is_some() || variants.is_empty() {
        return Err("unsupported");
    }
    let mut variants: Vec<_> = variants
        .into_iter()
        .filter_map(|(address, dimensions, bandwidth, group)| {
            let audio = match group {
                Some(group) => audio.get(&group)?.1.clone(),
                None => None,
            };
            Some(Variant {
                address,
                audio,
                dimensions,
                bandwidth,
            })
        })
        .collect();
    if variants.is_empty() {
        return Err("unsupported");
    }
    variants.sort_by_key(|variant| {
        let (w, h) = variant.dimensions.unwrap_or_default();
        std::cmp::Reverse((w.min(h), variant.bandwidth))
    });
    let mut seen = std::collections::HashSet::new();
    variants.retain(|variant| seen.insert(variant.id()));
    Ok(Playlist::Master(variants))
}

fn byte_range(value: &str) -> AppResult<(u64, Option<u64>)> {
    let (len, start) = match value.split_once('@') {
        Some((len, start)) => (len, Some(start.parse().map_err(|_| "unsupported")?)),
        None => (value, None),
    };
    let len = len.parse::<u64>().map_err(|_| "unsupported")?;
    if len == 0 || len > SEGMENT_LIMIT as u64 {
        return Err("unsupported");
    }
    Ok((len, start))
}

fn segment(
    base: &Url,
    location: &str,
    range: Option<(u64, Option<u64>)>,
    previous: Option<&Segment>,
) -> AppResult<Segment> {
    let url = relative(base, location)?;
    let range = match range {
        Some((len, start)) => {
            let start = start
                .or_else(|| {
                    let previous = previous.filter(|previous| previous.url == url)?;
                    previous.range?.1.checked_add(1)
                })
                .ok_or("unsupported")?;
            Some((start, start.checked_add(len - 1).ok_or("unsupported")?))
        }
        None => None,
    };
    Ok(Segment { url, range })
}

fn parse(base: &Url, text: &str) -> AppResult<Playlist> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err("unsupported");
    }
    if lines
        .clone()
        .any(|line| line.starts_with("#EXT-X-STREAM-INF:"))
    {
        return parse_master(base, lines);
    }
    let mut complete = false;
    let mut duration = false;
    let mut seconds = 0.0;
    let mut sequence = 0;
    let mut segments = Vec::new();
    let mut initialization = None;
    let mut range = None;
    for line in lines {
        let (key, value) = line.split_once(':').unwrap_or((line, ""));
        match key {
            "#EXT-X-MAP" if segments.is_empty() && !complete && initialization.is_none() => {
                let fields = attributes(value)?;
                let uri = fields.get("URI").ok_or("unsupported")?;
                let range = fields
                    .get("BYTERANGE")
                    .map(|range| byte_range(range))
                    .transpose()?;
                initialization = Some(segment(base, uri, range, None)?);
            }
            "#EXT-X-BYTERANGE" if range.is_none() && !complete => {
                range = Some(byte_range(value)?);
            }
            "#EXT-X-ENDLIST" => complete = true,
            "#EXTINF" if !duration && !complete => {
                duration = true;
                let segment_seconds = value
                    .split(',')
                    .next()
                    .and_then(|value| value.trim().parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .ok_or("unsupported")?;
                seconds += segment_seconds;
            }
            "#EXT-X-MEDIA-SEQUENCE" if segments.is_empty() => {
                sequence = value.parse().map_err(|_| "unsupported")?;
            }
            "#EXT-X-KEY" if attributes(value)?.get("METHOD") == Some(&"NONE") => {}
            "#EXT-X-TARGETDURATION"
            | "#EXT-X-ALLOW-CACHE"
            | "#EXT-X-PLAYLIST-TYPE"
            | "#EXT-X-VERSION"
            | "#EXT-X-PROGRAM-DATE-TIME"
            | "#EXT-X-INDEPENDENT-SEGMENTS" => {}
            _ if line.starts_with("#EXT") => return Err("unsupported"),
            _ if line.starts_with('#') => {}
            _ if duration && !complete && segments.len() < 12_000 => {
                segments.push(segment(base, line, range.take(), segments.last())?);
                duration = false;
            }
            _ => return Err("unsupported"),
        }
    }
    if !complete || duration || range.is_some() || segments.is_empty() {
        return Err("unsupported");
    }
    Ok(Playlist::Media(MediaPlaylist {
        initialization,
        segments,
        sequence,
        seconds: (seconds.is_finite() && seconds > 0.0).then_some(seconds),
    }))
}

async fn playlist(client: &Client, url: Url, referer: Option<&str>) -> AppResult<Playlist> {
    let mut response = get(client, url, referer, Duration::from_secs(15), None).await?;
    let address = response.url().clone();
    let bytes = discovery::read_limited(&mut response, 2 * 1024 * 1024, "unsupported").await?;
    parse(
        &address,
        std::str::from_utf8(&bytes).map_err(|_| "unsupported")?,
    )
}

async fn media_playlist(
    client: &Client,
    url: Url,
    referer: Option<&str>,
) -> AppResult<MediaPlaylist> {
    match playlist(client, url, referer).await? {
        Playlist::Media(media) => Ok(media),
        Playlist::Master(_) => Err("unsupported"),
    }
}

async fn selected_media(
    client: &Client,
    url: Url,
    format_id: Option<&str>,
    referer: Option<&str>,
) -> AppResult<(MediaPlaylist, Option<MediaPlaylist>, Option<(u32, u32)>)> {
    let requested = format_id.filter(|id| *id != "hls:default");
    match playlist(client, url, referer).await? {
        Playlist::Master(variants) => {
            let variant = variants
                .into_iter()
                .find(|variant| requested.is_none_or(|id| id == variant.id()))
                .ok_or("qualityUnavailable")?;
            let (video, audio) = variant_media(client, &variant, referer).await?;
            Ok((video, audio, variant.dimensions))
        }
        Playlist::Media(media) if requested.is_none() => Ok((media, None, None)),
        Playlist::Media(_) => Err("qualityUnavailable"),
    }
}

async fn variant_media(
    client: &Client,
    variant: &Variant,
    referer: Option<&str>,
) -> AppResult<(MediaPlaylist, Option<MediaPlaylist>)> {
    tokio::try_join!(
        media_playlist(client, variant.address.clone(), referer),
        async {
            match &variant.audio {
                Some(audio) => media_playlist(client, audio.clone(), referer)
                    .await
                    .map(Some),
                None => Ok(None),
            }
        }
    )
}

pub fn is_stream_quality(quality: &discovery::Quality) -> bool {
    quality.format_id.as_deref().map_or_else(
        || {
            quality
                .url
                .as_deref()
                .and_then(|url| discovery::url(url).ok())
                .is_some_and(|url| is_playlist(&url))
        },
        valid_format_id,
    )
}

pub async fn retain_available(client: &Client, item: &mut discovery::Media) -> bool {
    if !item.qualities.iter().any(is_stream_quality) {
        return false;
    }
    let has_ffmpeg = youtube::ffmpeg().is_ok();
    let referer = item.source_origin.as_deref();
    let mut changed = false;
    let mut qualities = Vec::new();
    for quality in std::mem::take(&mut item.qualities) {
        if !is_stream_quality(&quality) {
            qualities.push(quality);
            continue;
        }
        let Some(url) = quality
            .url
            .as_deref()
            .and_then(|url| discovery::url(url).ok())
        else {
            continue;
        };
        if !has_ffmpeg {
            continue;
        }
        match playlist(client, url, referer).await {
            Ok(Playlist::Media(media)) => {
                item.duration = item.duration.or(media.seconds);
                qualities.push(quality);
            }
            Ok(Playlist::Master(variants)) => {
                let available =
                    stream::iter(variants.into_iter().take(16).map(|variant| async move {
                        let result = variant_media(client, &variant, referer).await;
                        (variant, result)
                    }))
                    .buffered(4)
                    .collect::<Vec<_>>()
                    .await;
                for (variant, result) in available {
                    match result {
                        Ok((video, _)) => {
                            item.duration = item.duration.or(video.seconds);
                            let (width, height) = variant
                                .dimensions
                                .map_or((None, quality.height), |(w, h)| (Some(w), Some(h)));
                            qualities.push(discovery::Quality {
                                url: quality.url.clone(),
                                format_id: Some(variant.id()),
                                width,
                                height,
                                ..quality.clone()
                            });
                        }
                        Err("changed") => changed = true,
                        Err(_) => {}
                    }
                }
            }
            Err("changed") => changed = true,
            Err(_) => {}
        }
    }
    qualities.sort_by_key(|quality| std::cmp::Reverse(quality.resolution()));
    let mut seen = std::collections::HashSet::new();
    qualities.retain(|quality| seen.insert((quality.url.clone(), quality.format_id.clone())));
    qualities.truncate(16);
    item.qualities = qualities;
    changed
}

pub async fn inspect_resolution(
    client: &Client,
    url: Url,
    format_id: Option<&str>,
    referer: Option<&str>,
) -> AppResult<(u32, u32)> {
    const SAMPLE_LIMIT: usize = 512 * 1024;
    static PROBES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    if !is_playlist(&url) && !format_id.is_some_and(valid_format_id) {
        return Err("unsupported");
    }
    let _permit = PROBES.try_acquire().map_err(|_| "unknown")?;
    let (media, _, dimensions) = selected_media(client, url, format_id, referer).await?;
    if let Some(dimensions) = dimensions {
        return Ok(dimensions);
    }
    let fragmented = media.initialization.is_some();
    let segment = media
        .initialization
        .or_else(|| media.segments.into_iter().next())
        .ok_or("unsupported")?;
    let (start, end) = segment
        .range
        .map_or((0, SAMPLE_LIMIT as u64 - 1), |(start, end)| {
            (
                start,
                end.min(start.saturating_add(SAMPLE_LIMIT as u64 - 1)),
            )
        });
    let mut request = client
        .get(segment.url)
        .header(RANGE, format!("bytes={start}-{end}"))
        .timeout(Duration::from_secs(8));
    if let Some(referer) = referer {
        request = request.header(REFERER, referer);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "network")?
        .error_for_status()
        .map_err(|_| "httpError")?;
    if segment.range.is_some()
        && (response.status() != StatusCode::PARTIAL_CONTENT
            || !downloads::content_range(&response).is_some_and(|(from, through, len)| {
                (from, through) == (start, end) && through < len
            }))
    {
        return Err("changed");
    }
    let mut bytes = Vec::new();
    while bytes.len() < SAMPLE_LIMIT {
        let Some(chunk) = response.chunk().await.map_err(|_| "network")? else {
            break;
        };
        bytes.extend_from_slice(&chunk[..chunk.len().min(SAMPLE_LIMIT - bytes.len())]);
    }
    if (fragmented && !clear_mp4_bytes(&bytes, true))
        || (!fragmented && bytes.first() != Some(&0x47))
    {
        return Err("unsupported");
    }

    let probe = youtube::ffmpeg()?.with_file_name("ffprobe.exe");
    let mut command = tokio::process::Command::new(probe);
    command
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "pipe",
            "-f",
            if fragmented { "mov" } else { "mpegts" },
        ]);
    if fragmented {
        command.args(["-enable_drefs", "0"]);
    }
    let mut child = command
        .args([
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "json",
            "pipe:0",
        ])
        .spawn()
        .map_err(|_| "unsupported")?;
    let mut input = child.stdin.take().ok_or("unsupported")?;
    let output = tokio::time::timeout(Duration::from_secs(5), async move {
        let (_, output) = tokio::join!(
            async move { input.write_all(&bytes).await },
            child.wait_with_output()
        );
        output
    })
    .await
    .map_err(|_| "unsupported")?
    .map_err(|_| "unsupported")?;
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "unsupported")?;
    let stream = &json["streams"][0];
    let dimension = |key: &str| {
        stream[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|n| *n > 0)
    };
    Ok((
        dimension("width").ok_or("unsupported")?,
        dimension("height").ok_or("unsupported")?,
    ))
}

pub async fn save(
    client: &Client,
    url: Url,
    format_id: Option<&str>,
    referer: Option<&str>,
    dir: &Path,
    progress: impl Fn(u64),
) -> AppResult<u64> {
    let (video, audio, _) = selected_media(client, url, format_id, referer).await?;
    crate::stream::save(
        client,
        &video,
        audio.as_ref(),
        referer,
        &dir.join("hls"),
        progress,
    )
    .await
}
