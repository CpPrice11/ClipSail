use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Url, header::HeaderValue};
use serde::{Deserialize, Serialize};

use crate::{
    dash,
    discovery::{self, AppResult, Kind, Media, Quality},
    hls, yt_dlp,
};

#[derive(Deserialize)]
struct Entry {
    id: String,
    extractor_key: String,
    title: Option<String>,
    thumbnail: Option<String>,
    duration: Option<f64>,
    is_live: Option<bool>,
    live_status: Option<String>,
    has_drm: Option<serde_json::Value>,
    formats: Vec<Format>,
}

#[derive(Deserialize)]
struct Format {
    format_id: Option<String>,
    protocol: Option<String>,
    ext: Option<String>,
    url: Option<String>,
    manifest_url: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    has_drm: Option<serde_json::Value>,
    width: Option<u32>,
    height: Option<u32>,
    filesize: Option<u64>,
    tbr: Option<f64>,
    #[serde(default)]
    http_headers: HashMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct Selector {
    extractor: String,
    item: String,
    format: String,
    stream: Option<String>,
}

const PREFIX: &str = "extractor:";

fn component(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn clean_drm(value: Option<&serde_json::Value>) -> bool {
    value.is_none_or(|value| value.is_null() || value.as_bool() == Some(false))
}

impl Selector {
    fn read(value: &str) -> Option<Self> {
        let encoded = value
            .strip_prefix(PREFIX)
            .filter(|value| value.len() <= 1024)?;
        let selector =
            serde_json::from_slice::<Self>(&URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()?;
        (component(&selector.extractor)
            && component(&selector.item)
            && component(&selector.format)
            && selector
                .stream
                .as_deref()
                .is_none_or(|id| hls::valid_format_id(id) || dash::valid_format_id(id)))
        .then_some(selector)
    }

    fn encode(&self) -> Option<String> {
        let json = serde_json::to_vec(self).ok()?;
        let encoded = URL_SAFE_NO_PAD.encode(json);
        (encoded.len() <= 1024).then(|| format!("{PREFIX}{encoded}"))
    }
}

pub fn valid_format_id(value: &str) -> bool {
    Selector::read(value).is_some()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Transfer {
    File,
    Hls,
    Dash,
}

pub struct Source {
    pub url: Url,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    pub stream_id: Option<String>,
}

impl Format {
    fn source(&self, page: &Url) -> Option<(Transfer, Source)> {
        if !clean_drm(self.has_drm.as_ref()) || self.vcodec.as_deref() == Some("none") {
            return None;
        }
        let header = |name: &str| {
            self.http_headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        if ["cookie", "authorization", "proxy-authorization"]
            .iter()
            .any(|name| header(name).is_some_and(|value| !value.is_empty()))
        {
            return None;
        }
        let ext = self.ext.as_deref()?;
        let (transfer, address) = match self.protocol.as_deref()? {
            "https" | "http"
                if discovery::video_extension(ext).is_some()
                    && self.acodec.as_deref() != Some("none") =>
            {
                (Transfer::File, self.url.as_deref()?)
            }
            "m3u8" | "m3u8_native" if ext == "mp4" => {
                let address = self.manifest_url.as_deref().or_else(|| {
                    (self.acodec.as_deref() != Some("none"))
                        .then_some(self.url.as_deref())
                        .flatten()
                })?;
                (Transfer::Hls, address)
            }
            "http_dash_segments" | "dash" if ext == "mp4" => {
                (Transfer::Dash, self.manifest_url.as_deref()?)
            }
            _ => return None,
        };
        let url = discovery::url(address).ok()?;
        if page.scheme() == "https" && url.scheme() != "https" {
            return None;
        }
        let referer_page = header("referer").and_then(|value| discovery::url(value).ok());
        let user_agent = header("user-agent")
            .filter(|value| value.len() <= 512 && HeaderValue::from_str(value).is_ok())
            .map(str::to_owned);
        Some((
            transfer,
            Source {
                referer: discovery::media_referer(referer_page.as_ref().unwrap_or(page), &url),
                url,
                user_agent,
                stream_id: None,
            },
        ))
    }
}

async fn extract(page: &Url) -> AppResult<Entry> {
    let mut command = yt_dlp::command().map_err(|_| "unsupported")?;
    command.args(["--simulate", "--playlist-end", "2"]);
    let bytes = yt_dlp::metadata(
        command,
        page.as_str(),
        "%(.{id,extractor_key,title,thumbnail,duration,is_live,live_status,has_drm,formats})j",
    )
    .await
    .map_err(|_| "unsupported")?;
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace));
    let entry: Entry =
        serde_json::from_slice(lines.next().ok_or("unsupported")?).map_err(|_| "unsupported")?;
    if lines.next().is_some()
        || !component(&entry.id)
        || !component(&entry.extractor_key)
        || entry.is_live == Some(true)
        || matches!(
            entry.live_status.as_deref(),
            Some("is_live" | "is_upcoming" | "post_live")
        )
        || !clean_drm(entry.has_drm.as_ref())
    {
        return Err("unsupported");
    }
    Ok(entry)
}

fn media(page: &Url, entry: &Entry) -> Media {
    let preview = entry
        .thumbnail
        .as_deref()
        .and_then(|value| discovery::url(value).ok())
        .map(|url| url.to_string());
    let name = entry
        .title
        .as_deref()
        .unwrap_or(&entry.id)
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    Media {
        url: page.to_string(),
        source_origin: discovery::media_referer(page, page),
        name,
        photo_title: None,
        kind: Kind::Video,
        size: None,
        width: None,
        height: None,
        duration: entry
            .duration
            .filter(|value| value.is_finite() && *value > 0.0),
        preview,
        preview_only: false,
        page_asset: false,
        qualities: Vec::new(),
    }
}

pub async fn find(client: &Client, page: &Url) -> Option<Media> {
    tokio::time::timeout(Duration::from_secs(25), find_video(client, page))
        .await
        .ok()
        .flatten()
}

async fn find_video(client: &Client, page: &Url) -> Option<Media> {
    let entry = extract(page).await.ok()?;
    let mut item = media(page, &entry);
    let mut choices = Vec::new();
    let mut manifests = HashSet::new();
    for format in entry.formats.iter().rev().take(600) {
        let Some(format_id) = format.format_id.as_deref().filter(|value| component(value)) else {
            continue;
        };
        let Some((transfer, source)) = format.source(page) else {
            continue;
        };
        let direct = transfer == Transfer::File;
        let quality = Quality {
            url: None,
            source_origin: None,
            format_id: None,
            extension: format.ext.clone()?,
            width: format.width,
            height: format.height,
            size: direct.then_some(format.filesize).flatten(),
        };
        let candidates = if direct {
            vec![quality]
        } else {
            // Use the original manifest so separate audio and the chosen rendition stay linked.
            if manifests.len() >= 4 || !manifests.insert((transfer, source.url.clone())) {
                continue;
            }
            let specialized = source
                .user_agent
                .as_deref()
                .and_then(|value| discovery::client_with_user_agent(value).ok());
            let stream_client = specialized.as_ref().unwrap_or(client);
            let mut manifest = media(page, &entry);
            manifest.source_origin = source.referer;
            manifest.qualities.push(Quality {
                url: Some(source.url.to_string()),
                format_id: Some(
                    match transfer {
                        Transfer::Hls => "hls:default",
                        Transfer::Dash => "dash:default",
                        Transfer::File => unreachable!(),
                    }
                    .to_owned(),
                ),
                ..quality
            });
            match transfer {
                Transfer::Hls => {
                    hls::retain_available(stream_client, &mut manifest).await;
                }
                Transfer::Dash => {
                    dash::retain_available(stream_client, &mut manifest).await;
                }
                Transfer::File => unreachable!(),
            }
            item.duration = item.duration.or(manifest.duration);
            manifest.qualities
        };
        for mut quality in candidates {
            let selector = Selector {
                extractor: entry.extractor_key.clone(),
                item: entry.id.clone(),
                format: format_id.to_owned(),
                stream: quality.format_id.take(),
            };
            let Some(key) = selector.encode() else {
                continue;
            };
            quality.url = None;
            quality.format_id = Some(key);
            choices.push((quality, direct, format.tbr.unwrap_or_default()));
        }
    }
    choices.sort_by(|(a, a_direct, a_rate), (b, b_direct, b_rate)| {
        Reverse(a.resolution())
            .cmp(&Reverse(b.resolution()))
            .then_with(|| b_direct.cmp(a_direct))
            .then_with(|| b_rate.total_cmp(a_rate))
    });
    let mut seen = HashSet::new();
    item.qualities = choices
        .into_iter()
        .map(|(quality, _, _)| quality)
        .filter(|quality| seen.insert((quality.resolution(), quality.extension.clone())))
        .take(16)
        .collect();
    let best = item.qualities.first()?;
    item.width = best.width;
    item.height = best.height;
    item.size = best.size;
    Some(item)
}

fn selected_source(page: &Url, selected: Selector, entry: &Entry) -> AppResult<Source> {
    if entry.id != selected.item || entry.extractor_key != selected.extractor {
        return Err("qualityUnavailable");
    }
    let format = entry
        .formats
        .iter()
        .find(|format| format.format_id.as_deref() == Some(selected.format.as_str()))
        .ok_or("qualityUnavailable")?;
    let (transfer, mut source) = format.source(page).ok_or("qualityUnavailable")?;
    match (transfer, selected.stream.as_deref()) {
        (Transfer::File, None) => {}
        (Transfer::Hls, Some(id)) if hls::valid_format_id(id) => {}
        (Transfer::Dash, Some(id)) if dash::valid_format_id(id) => {}
        _ => return Err("qualityUnavailable"),
    }
    source.stream_id = selected.stream;
    Ok(source)
}

pub async fn resolve(page: &Url, format_id: &str) -> AppResult<Source> {
    let selected = Selector::read(format_id).ok_or("qualityUnavailable")?;
    let entry = extract(page).await.map_err(|_| "qualityUnavailable")?;
    selected_source(page, selected, &entry)
}
