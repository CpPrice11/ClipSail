use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_BYTES: usize = 64 * 1024;
pub const ADDON_ID: &str = "{86208952-d93f-483a-bace-e85ac03124dc}";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoRequest {
    pub url: String,
    pub title: String,
    pub origin: String,
    pub quality: u16,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_id: Option<String>,
}

impl VideoRequest {
    pub fn validate(&self) -> Option<reqwest::Url> {
        let media = reqwest::Url::parse(&self.url).ok()?;
        let page = reqwest::Url::parse(&self.origin).ok()?;
        let youtube = is_youtube_video(&media) && self.origin == "https://www.youtube.com";
        if self.url.len() > 8192
            || self.origin.len() > 512
            || self.title.chars().count() > 180
            || self.title.chars().any(char::is_control)
            || self.quality > 8640
            || page.origin().ascii_serialization() != self.origin
            || ![&media, &page].iter().all(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
            })
            || self.format_id.as_ref().is_some_and(|id| {
                !youtube || !self.auto_start || !crate::youtube::valid_format_id(id)
            })
            || !(youtube
                || matches!(
                    media
                        .path()
                        .rsplit_once('.')
                        .map(|(_, ext)| ext.to_ascii_lowercase())
                        .as_deref(),
                    Some("mp4" | "webm")
                )
                || self.auto_start
                    && matches!(
                        media
                            .path()
                            .rsplit_once('.')
                            .map(|(_, ext)| ext.to_ascii_lowercase())
                            .as_deref(),
                        Some(
                            "jpg"
                                | "jpeg"
                                | "png"
                                | "gif"
                                | "webp"
                                | "avif"
                                | "mp3"
                                | "m4a"
                                | "ogg"
                                | "oga"
                                | "wav"
                        )
                    ))
        {
            return None;
        }
        Some(media)
    }
}

pub fn is_youtube_video(url: &reqwest::Url) -> bool {
    url.origin().ascii_serialization() == "https://www.youtube.com"
        && url.path() == "/watch"
        && url
            .query()
            .and_then(|query| query.strip_prefix("v="))
            .is_some_and(|id| {
                id.len() == 11
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            })
}

pub fn incoming_directory() -> Option<PathBuf> {
    Some(directory()?.join("incoming"))
}

pub fn directory() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("MEDIA_DOWNLOADER_TEST_DATA") {
        return Some(PathBuf::from(path).join("firefox"));
    }
    Some(
        dirs::data_local_dir()?
            .join("local.media.downloader")
            .join("firefox"),
    )
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub profile: Uuid,
    pub jobs: Vec<BrowserJob>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserJob {
    pub id: u32,
    pub name: String,
    pub status: BrowserStatus,
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum BrowserStatus {
    Running,
    Paused,
    Completed,
    Failed,
    FirefoxOffline,
}
