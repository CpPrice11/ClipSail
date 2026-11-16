use std::cmp::Reverse;

use reqwest::Url;
use serde::Deserialize;

use crate::{
    discovery::{self, Kind, Media, Quality},
    yt_dlp,
};

fn post_url(mut url: Url) -> Option<Url> {
    if url.scheme() != "https"
        || url.port().is_some()
        || !matches!(
            url.host_str()?,
            "x.com"
                | "www.x.com"
                | "mobile.x.com"
                | "twitter.com"
                | "www.twitter.com"
                | "mobile.twitter.com"
        )
    {
        return None;
    }

    let parts: Vec<_> = url
        .path()
        .trim_end_matches('/')
        .split('/')
        .skip(1)
        .collect();
    let tail = match parts.as_slice() {
        ["i", "web", "status", tail @ ..] | ["statuses", tail @ ..] => tail,
        [handle, "status", tail @ ..]
            if (1..=15).contains(&handle.len())
                && handle
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_') =>
        {
            tail
        }
        _ => return None,
    };
    let numeric = |value: &str| {
        !value.is_empty() && value.len() <= 20 && value.bytes().all(|c| c.is_ascii_digit())
    };
    match tail {
        [id] if numeric(id) => {}
        [id, "video", index]
            if numeric(id) && index.parse::<u16>().is_ok_and(|index| index > 0) => {}
        _ => return None,
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

#[derive(Deserialize)]
struct Entry {
    title: String,
    duration: Option<f64>,
    thumbnail: Option<String>,
    formats: Vec<Format>,
}

#[derive(Deserialize)]
struct Format {
    url: String,
    protocol: String,
    ext: String,
    width: Option<u32>,
    height: Option<u32>,
    filesize: Option<u64>,
    has_drm: Option<serde_json::Value>,
}

fn cdn_url(address: &str, host: &str) -> Option<Url> {
    let url = discovery::url(address).ok()?;
    (url.scheme() == "https" && url.host_str() == Some(host) && url.port().is_none()).then_some(url)
}

fn mp4(format: Format) -> Option<Quality> {
    if format.protocol != "https"
        || format.ext != "mp4"
        || format
            .has_drm
            .as_ref()
            .is_some_and(|drm| drm.as_bool() != Some(false))
    {
        return None;
    }
    let url = cdn_url(&format.url, "video.twimg.com")?;
    if discovery::file_format(url.path()) != Some(("mp4", Kind::Video)) {
        return None;
    }
    Some(Quality {
        url: Some(url.to_string()),
        source_origin: None,
        format_id: None,
        extension: format.ext,
        width: format.width,
        height: format.height,
        size: format.filesize,
    })
}

fn media(entry: Entry, post: &Url) -> Option<Media> {
    let mut qualities: Vec<_> = entry.formats.into_iter().rev().filter_map(mp4).collect();
    qualities.sort_by_key(|quality| Reverse(quality.resolution()));
    qualities.dedup_by_key(|quality| quality.resolution());
    qualities.truncate(16);
    let best = qualities.first()?;
    let url = best.url.clone()?;
    let preview = entry
        .thumbnail
        .and_then(|address| cdn_url(&address, "pbs.twimg.com"))
        .map(|address| address.to_string());
    Some(Media {
        url,
        source_origin: Some(post.origin().ascii_serialization()),
        name: entry.title.chars().take(120).collect(),
        photo_title: None,
        kind: Kind::Video,
        size: best.size,
        width: best.width,
        height: best.height,
        duration: entry.duration,
        preview,
        preview_only: false,
        page_asset: false,
        qualities,
    })
}

pub async fn find(url: Url) -> Option<Vec<Media>> {
    let post = post_url(url)?;
    let mut command = yt_dlp::command().ok()?;
    command.args(["--ies", "^Twitter$", "--playlist-end", "16"]);
    let output = yt_dlp::metadata(
        command,
        post.as_str(),
        "%(.{title,duration,thumbnail,formats})j",
    )
    .await
    .ok()?;
    let items: Vec<_> = output
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .filter_map(|entry| media(entry, &post))
        .take(16)
        .collect();
    (!items.is_empty()).then_some(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_x_post_and_video_index() {
        let post = "https://x.com/test/status/1234567890123456789/video/1";
        assert_eq!(
            post_url(Url::parse(&format!("{post}?s=20")).unwrap())
                .unwrap()
                .as_str(),
            post
        );
        for address in [
            "https://x.com/test",
            "https://x.com/test/status/123/photo/1",
            "https://x.com/test/status/123/video/1/extra",
            "https://x.com.evil.example/test/status/123",
        ] {
            assert!(post_url(Url::parse(address).unwrap()).is_none());
        }
    }

    #[test]
    fn exposes_only_direct_mp4_from_x_cdn_at_each_resolution() {
        let entry = serde_json::from_value(serde_json::json!({
            "title": "Portrait video",
            "duration": 27.353,
            "thumbnail": "https://pbs.twimg.com/video_thumb.jpg",
            "formats": [
                {"url":"https://video.twimg.com/low.mp4","protocol":"https","ext":"mp4","width":320,"height":568},
                {"url":"https://video.twimg.com/high.mp4","protocol":"https","ext":"mp4","width":720,"height":1280},
                {"url":"https://video.twimg.com/high.mp4","protocol":"https","ext":"mp4","width":720,"height":1280},
                {"url":"https://video.twimg.com/drm.mp4","protocol":"https","ext":"mp4","width":1080,"height":1920,"has_drm":true},
                {"url":"https://video.twimg.com/video.m3u8","protocol":"m3u8_native","ext":"mp4"},
                {"url":"https://video.twimg.com.evil.example/high.mp4","protocol":"https","ext":"mp4","width":2160,"height":3840}
            ]
        })).unwrap();
        let post = Url::parse("https://x.com/test/status/123").unwrap();
        let result = media(entry, &post).unwrap();
        assert_eq!(result.url, "https://video.twimg.com/high.mp4");
        assert_eq!((result.width, result.height), (Some(720), Some(1280)));
        assert_eq!(result.duration, Some(27.353));
        assert_eq!(
            result
                .qualities
                .iter()
                .map(Quality::resolution)
                .collect::<Vec<_>>(),
            [720, 320]
        );
    }
}
