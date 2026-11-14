use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use html5ever::{
    tendril::StrTendril,
    tokenizer::states::{Rawtext, Rcdata, ScriptData},
    tokenizer::{
        BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    },
};
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{CONTENT_TYPE, HeaderMap},
};
use serde::{Deserialize, Serialize};

pub type AppResult<T> = Result<T, &'static str>;

const MAIN_MEDIA_LIMIT: usize = 300;
const PAGE_ASSET_LIMIT: usize = 100;
pub const GALLERY_LIMIT: usize = MAIN_MEDIA_LIMIT + PAGE_ASSET_LIMIT;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Media {
    pub url: String,
    pub name: String,
    pub kind: Kind,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    pub preview: Option<String>,
    #[serde(default)]
    pub preview_only: bool,
    #[serde(default)]
    pub page_asset: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualities: Vec<Quality>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Quality {
    pub url: Option<String>,
    pub format_id: Option<String>,
    pub extension: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: Option<u64>,
}

impl Quality {
    pub fn resolution(&self) -> u32 {
        match (self.width, self.height) {
            (Some(w), Some(h)) => w.min(h),
            (_, height) => height.unwrap_or_default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Gif,
    Video,
    Audio,
}

pub fn client() -> AppResult<Client> {
    Client::builder()
        .user_agent("MediaDownloader/0.1")
        .connect_timeout(Duration::from_secs(8))
        .read_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 8
                || !allowed(attempt.url())
                || attempt
                    .previous()
                    .last()
                    .is_some_and(|old| old.scheme() == "https" && attempt.url().scheme() == "http")
            {
                attempt.error("Invalid redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| "network")
}

fn allowed(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

pub fn url(value: &str) -> AppResult<Url> {
    if value.len() > 8192 {
        return Err("invalidUrl");
    }
    let mut url = Url::parse(value.trim()).map_err(|_| "invalidUrl")?;
    if !allowed(&url) {
        return Err("invalidUrl");
    }
    url.set_fragment(None);
    Ok(url)
}

pub fn mime(headers: &HeaderMap) -> &str {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
}

fn mime_format(mime: &str) -> Option<(&'static str, Kind)> {
    Some(match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" => ("jpg", Kind::Image),
        "image/png" => ("png", Kind::Image),
        "image/gif" => ("gif", Kind::Gif),
        "image/svg+xml" => ("svg", Kind::Image),
        "image/x-icon" | "image/vnd.microsoft.icon" => ("ico", Kind::Image),
        "image/webp" => ("webp", Kind::Image),
        "image/avif" => ("avif", Kind::Image),
        "video/mp4" => ("mp4", Kind::Video),
        "video/webm" => ("webm", Kind::Video),
        "audio/mpeg" => ("mp3", Kind::Audio),
        "audio/mp4" | "audio/x-m4a" => ("m4a", Kind::Audio),
        "audio/wav" | "audio/x-wav" => ("wav", Kind::Audio),
        "audio/ogg" | "application/ogg" => ("ogg", Kind::Audio),
        "audio/webm" => ("webm", Kind::Audio),
        _ => return None,
    })
}

fn path_format(url: &Url) -> Option<(&'static str, Kind)> {
    file_format(url.path())
}

pub fn file_format(name: &str) -> Option<(&'static str, Kind)> {
    Some(
        match name.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => ("jpg", Kind::Image),
            "png" => ("png", Kind::Image),
            "gif" => ("gif", Kind::Gif),
            "svg" => ("svg", Kind::Image),
            "ico" => ("ico", Kind::Image),
            "webp" => ("webp", Kind::Image),
            "avif" => ("avif", Kind::Image),
            "mp4" | "m4v" => ("mp4", Kind::Video),
            "webm" => ("webm", Kind::Video),
            "mp3" => ("mp3", Kind::Audio),
            "m4a" => ("m4a", Kind::Audio),
            "wav" => ("wav", Kind::Audio),
            "ogg" | "oga" => ("ogg", Kind::Audio),
            _ => return None,
        },
    )
}

pub fn response_format(response: &Response) -> Option<(&'static str, Kind)> {
    match mime(response.headers()).to_ascii_lowercase().as_str() {
        "" | "application/octet-stream" | "binary/octet-stream" => path_format(response.url()),
        value => mime_format(value),
    }
}

fn asset_label(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "icon"
            | "icons"
            | "favicon"
            | "favicons"
            | "site-icon"
            | "logo"
            | "site-logo"
            | "avatar"
            | "avatars"
            | "emoji"
            | "emojis"
            | "sprite"
            | "sprites"
            | "spinner"
            | "tracking-pixel"
    )
}

fn asset_class(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    asset_label(&value)
        || value.starts_with("emoji-")
        || value.starts_with("icon-")
        || value.ends_with("-icon")
        || value.ends_with("-emoji")
}

fn asset_path(url: &Url) -> bool {
    let decoded = percent_encoding::percent_decode_str(url.path()).decode_utf8_lossy();
    let (directory, file) = decoded.rsplit_once('/').unwrap_or(("", &decoded));
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    directory.split('/').any(asset_label) || asset_label(stem)
}

pub fn media_name(url: &Url, extension: &str) -> String {
    let segment = url.path().rsplit('/').next().unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(segment).decode_utf8_lossy();
    let stem = decoded
        .rsplit_once('.')
        .map_or(decoded.as_ref(), |(stem, _)| stem);
    safe_media_name(stem, extension)
}

pub fn safe_media_name(stem: &str, extension: &str) -> String {
    let clean: String = stem
        .chars()
        .take(100)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = clean.trim_matches([' ', '.']);
    let base = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (base.starts_with("COM") || base.starts_with("LPT"))
        && base.chars().count() == 4
        && base.ends_with(|c: char| c.is_ascii_digit() || "¹²³".contains(c));
    if name.is_empty() || reserved {
        format!("media.{extension}")
    } else {
        format!("{name}.{extension}")
    }
}

#[derive(Default)]
struct Html {
    base: RefCell<Option<String>>,
    items: RefCell<Vec<Candidate>>,
    player: Cell<Option<Kind>>,
    video_group: Cell<usize>,
    player_asset: Cell<bool>,
    page_title: RefCell<Option<String>>,
    page_image: RefCell<Option<String>>,
    last_meta_video: Cell<Option<usize>>,
    poster: RefCell<Option<String>>,
    collect_links: bool,
    anchor: RefCell<Option<String>>,
    posts: RefCell<Vec<String>>,
    next: RefCell<Vec<String>>,
    json_ld: RefCell<Vec<String>>,
    in_json_ld: Cell<bool>,
}

struct Candidate {
    link: String,
    format: Option<(&'static str, Kind)>,
    hint: Option<Kind>,
    width: Option<u32>,
    height: Option<u32>,
    poster: Option<String>,
    embedded: bool,
    page_asset: bool,
    video_group: Option<usize>,
    quality_height: Option<u32>,
}

fn declared_quality(value: &str) -> Option<u32> {
    value
        .trim()
        .trim_end_matches(['p', 'P'])
        .parse::<u32>()
        .ok()
        .filter(|height| (100..=8640).contains(height))
}

impl TokenSink for Html {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let tag = match token {
            Token::TagToken(tag) => tag,
            Token::CharacterTokens(text) if self.in_json_ld.get() => {
                if let Some(json) = self.json_ld.borrow_mut().last_mut() {
                    json.push_str(&text);
                }
                return TokenSinkResult::Continue;
            }
            _ => return TokenSinkResult::Continue,
        };
        let name = tag.name.as_ref();
        if tag.kind == TagKind::EndTag {
            if name == "script" {
                self.in_json_ld.set(false);
            }
            if name == "a" {
                self.anchor.replace(None);
            }
            if matches!(name, "video" | "audio") {
                self.player.set(None);
                self.player_asset.set(false);
                self.poster.replace(None);
            }
            return TokenSinkResult::Continue;
        }
        let attr = |key: &str| {
            tag.attrs
                .iter()
                .find(|a| a.name.local.as_ref() == key)
                .map(|a| a.value.as_ref())
                .filter(|v| !v.is_empty())
        };
        let width = attr("width").and_then(|v| v.parse::<u32>().ok());
        let height = attr("height").and_then(|v| v.parse::<u32>().ok());
        let explicitly_small = width
            .zip(height)
            .is_some_and(|(w, h)| (1..=96).contains(&w) && (1..=96).contains(&h));
        let asset = attr("class")
            .is_some_and(|classes| classes.split_ascii_whitespace().any(asset_class))
            || attr("id").is_some_and(asset_class)
            || explicitly_small;
        if self.collect_links {
            let href = attr("href").filter(|link| link.len() <= 8192);
            let rel = |value| {
                attr("rel").is_some_and(|tokens| {
                    tokens
                        .split_ascii_whitespace()
                        .any(|token| token.eq_ignore_ascii_case(value))
                })
            };
            if matches!(name, "a" | "link") && rel("next") {
                if let Some(link) = href {
                    self.next.borrow_mut().push(link.to_owned());
                }
            }
            if name == "a" {
                self.anchor.replace(href.map(str::to_owned));
                if rel("bookmark") {
                    self.record_post();
                }
            }
            if matches!(name, "img" | "video")
                && !asset
                && attr("data-src").or_else(|| attr("src")).is_some()
            {
                self.record_post();
            }
        }
        let push = |link: &str, hint: Option<Kind>, poster: Option<String>, page_asset: bool| {
            let mut items = self.items.borrow_mut();
            if items.len() >= 3000 || link.len() > 8192 {
                return;
            }
            if hint.is_none()
                && !link
                    .split(['?', '#'])
                    .next()
                    .unwrap_or("")
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .contains('.')
            {
                return;
            }
            items.push(Candidate {
                link: link.to_owned(),
                format: attr("type").and_then(mime_format),
                hint,
                width,
                height,
                poster,
                embedded: false,
                page_asset,
                video_group: (hint == Some(Kind::Video) && matches!(name, "video" | "source"))
                    .then(|| self.video_group.get()),
                quality_height: [
                    "data-height",
                    "data-quality",
                    "data-res",
                    "res",
                    "size",
                    "label",
                ]
                .into_iter()
                .filter_map(attr)
                .find_map(declared_quality)
                .or_else(|| (name == "source").then_some(height).flatten()),
            });
        };
        match name {
            "script" => {
                if self.collect_links && attr("type") == Some("application/ld+json") {
                    self.json_ld.borrow_mut().push(String::new());
                    self.in_json_ld.set(true);
                }
                return TokenSinkResult::RawData(ScriptData);
            }
            "style" | "iframe" | "xmp" => return TokenSinkResult::RawData(Rawtext),
            "title" | "textarea" => return TokenSinkResult::RawData(Rcdata),
            "base" if self.base.borrow().is_none() => {
                self.base.replace(attr("href").map(str::to_owned));
            }
            "link"
                if attr("rel").is_some_and(|rel| {
                    rel.split_ascii_whitespace().any(|value| {
                        [
                            "icon",
                            "apple-touch-icon",
                            "apple-touch-icon-precomposed",
                            "mask-icon",
                        ]
                        .iter()
                        .any(|kind| value.eq_ignore_ascii_case(kind))
                    })
                }) =>
            {
                if let Some(link) = attr("href") {
                    push(link, Some(Kind::Image), None, true);
                }
            }
            "video" | "audio" => {
                let kind = if name == "video" {
                    Kind::Video
                } else {
                    Kind::Audio
                };
                let poster = attr("poster").map(str::to_owned);
                if kind == Kind::Video {
                    self.video_group.set(self.video_group.get() + 1);
                }
                self.player.set(Some(kind));
                self.player_asset.set(kind == Kind::Video && asset);
                self.poster.replace(poster.clone());
                if let Some(link) = attr("data-src").or_else(|| attr("src")) {
                    push(link, Some(kind), poster, self.player_asset.get());
                }
            }
            "source" | "img" => {
                let hint = if name == "img" {
                    Some(Kind::Image)
                } else {
                    self.player.get()
                };
                let page_asset = asset || (name == "source" && self.player_asset.get());
                if let Some(link) = attr("data-src").or_else(|| attr("src")) {
                    push(link, hint, self.poster.borrow().clone(), page_asset);
                }
                if let Some(srcset) = attr("data-srcset").or_else(|| attr("srcset")) {
                    if !srcset.trim_start().starts_with("data:") {
                        for link in srcset
                            .split(',')
                            .filter_map(|s| s.split_whitespace().next())
                        {
                            push(link, Some(Kind::Image), None, page_asset);
                        }
                    }
                }
            }
            "a" => {
                if let Some(link) = attr("href") {
                    push(link, None, None, false);
                }
            }
            "meta" => {
                let property = attr("property").or_else(|| attr("name"));
                match (property, attr("content")) {
                    (Some("og:title"), Some(value)) if self.page_title.borrow().is_none() => {
                        self.page_title
                            .replace(Some(value.chars().take(180).collect()));
                    }
                    (Some("og:image" | "og:image:url" | "og:image:secure_url"), Some(value))
                        if value.len() <= 8192 && self.page_image.borrow().is_none() =>
                    {
                        self.page_image.replace(Some(value.to_owned()));
                    }
                    (Some("og:video:type"), Some(value))
                        if value
                            .split(';')
                            .next()
                            .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/html")) =>
                    {
                        if let Some(index) = self.last_meta_video.get() {
                            self.items.borrow_mut()[index].embedded = true;
                        }
                    }
                    _ => {}
                }
                let hint = match property {
                    Some("og:image" | "og:image:secure_url" | "twitter:image") => Some(Kind::Image),
                    Some("og:video" | "og:video:url" | "og:video:secure_url") => Some(Kind::Video),
                    Some("og:audio" | "og:audio:url" | "og:audio:secure_url") => Some(Kind::Audio),
                    _ => None,
                };
                if let (Some(link), Some(hint)) = (attr("content"), hint) {
                    let index = self.items.borrow().len();
                    push(link, Some(hint), None, false);
                    if hint == Kind::Video {
                        self.last_meta_video
                            .set((self.items.borrow().len() > index).then_some(index));
                    }
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

impl Html {
    fn record_post(&self) {
        if let Some(link) = self.anchor.borrow_mut().take() {
            self.posts.borrow_mut().push(link);
        }
    }
}

fn tokenize(html: &str, collect_links: bool) -> Html {
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from(html));
    let parser = Tokenizer::new(
        Html {
            collect_links,
            ..Html::default()
        },
        TokenizerOpts::default(),
    );
    let _ = parser.feed(&queue);
    parser.end();
    parser.sink
}

fn parse(html: &str, source: &Url) -> Vec<Media> {
    parse_media(tokenize(html, false), source)
}

fn parse_media(page: Html, source: &Url) -> Vec<Media> {
    let base = page
        .base
        .into_inner()
        .and_then(|v| source.join(&v).ok())
        .filter(allowed)
        .unwrap_or_else(|| source.clone());
    let title = page.page_title.into_inner();
    let page_image = page.page_image.into_inner();
    let candidates = page.items.into_inner();
    let embedded: HashSet<_> = candidates
        .iter()
        .filter(|item| item.embedded)
        .filter_map(|item| base.join(&item.link).ok())
        .map(|mut url| {
            url.set_fragment(None);
            url
        })
        .collect();
    let mut seen = HashMap::new();
    let mut results: Vec<Media> = Vec::new();
    let mut groups = HashMap::<usize, Vec<(Media, Quality)>>::new();
    let mut entries = Vec::new();
    for (media, group) in candidates.into_iter().filter_map(|item| {
        let mut url = base.join(&item.link).ok()?;
        url.set_fragment(None);
        if !allowed(&url)
            || url.path().to_ascii_lowercase().ends_with(".m3u8")
            || url.path().to_ascii_lowercase().ends_with(".mpd")
        {
            return None;
        }
        let (extension, kind) = item
            .format
            .or_else(|| path_format(&url))
            .map(|(ext, kind)| {
                if kind == Kind::Gif {
                    (ext, kind)
                } else {
                    (ext, item.hint.unwrap_or(kind))
                }
            })
            .or_else(|| item.hint.map(|kind| ("", kind)))?;
        let preview_only = kind == Kind::Video && embedded.contains(&url);
        let preview = if matches!(kind, Kind::Image | Kind::Gif) {
            Some(url.to_string())
        } else {
            item.poster
                .as_deref()
                .or_else(|| {
                    if preview_only {
                        page_image.as_deref()
                    } else {
                        None
                    }
                })
                .and_then(|v| base.join(v).ok())
                .filter(allowed)
                .map(|v| v.to_string())
        };
        let name = if let Some(title) = title.as_ref().filter(|_| preview_only) {
            title.clone()
        } else if extension.is_empty() {
            let segment = url
                .path()
                .rsplit('/')
                .next()
                .filter(|v| !v.is_empty())
                .unwrap_or("media");
            percent_encoding::percent_decode_str(segment)
                .decode_utf8_lossy()
                .chars()
                .take(100)
                .collect()
        } else {
            media_name(&url, extension)
        };
        let group = item
            .video_group
            .filter(|_| kind == Kind::Video && !preview_only && matches!(extension, "mp4" | "webm"))
            .map(|id| {
                (
                    id,
                    Quality {
                        url: Some(url.to_string()),
                        format_id: None,
                        extension: extension.to_owned(),
                        width: None,
                        height: item.quality_height.or_else(|| {
                            url.path()
                                .split(|c: char| !c.is_ascii_alphanumeric())
                                .find_map(|part| {
                                    if part.ends_with(['p', 'P']) {
                                        declared_quality(part)
                                    } else {
                                        None
                                    }
                                })
                        }),
                        size: None,
                    },
                )
            });
        Some((
            Media {
                url: url.to_string(),
                name,
                kind,
                size: None,
                width: item.width,
                height: item.height,
                duration: None,
                preview,
                preview_only,
                page_asset: kind != Kind::Audio
                    && !preview_only
                    && (item.page_asset || extension == "ico" || asset_path(&url)),
                qualities: Vec::new(),
            },
            group,
        ))
    }) {
        if let Some((id, quality)) = group.filter(|_| !media.page_asset) {
            let group = groups.entry(id).or_default();
            if group.is_empty() {
                entries.push(Err(id));
            }
            if !group.iter().any(|(entry, _)| entry.url == media.url) {
                group.push((media, quality));
            }
        } else {
            entries.push(Ok(media));
        }
    }
    let mut alternative_urls = HashSet::new();
    let mut flattened = Vec::new();
    for entry in entries {
        let id = match entry {
            Ok(media) => {
                flattened.push(media);
                continue;
            }
            Err(id) => id,
        };
        let mut group = groups.remove(&id).unwrap_or_default();
        if group.len() > 1 && group.iter().any(|(_, quality)| quality.resolution() > 0) {
            group.sort_by_key(|(_, quality)| std::cmp::Reverse(quality.resolution()));
            let mut qualities = Vec::with_capacity(group.len().min(16));
            for (index, (entry, quality)) in group.iter().enumerate() {
                if index != 0 {
                    alternative_urls.insert(entry.url.clone());
                }
                if index < 16 {
                    qualities.push(quality.clone());
                }
            }
            let (mut best, _) = group.remove(0);
            best.height = qualities[0].height;
            best.qualities = qualities;
            flattened.push(best);
        } else {
            flattened.extend(group.into_iter().map(|(media, _)| media));
        }
    }
    for media in flattened {
        if media.qualities.is_empty() && alternative_urls.contains(&media.url) {
            continue;
        }
        if let Some(&index) = seen.get(&media.url) {
            let existing: &mut Media = &mut results[index];
            existing.page_asset =
                !existing.preview_only && (existing.page_asset || media.page_asset);
            existing.width = existing.width.or(media.width);
            existing.height = existing.height.or(media.height);
            existing.preview = existing.preview.take().or(media.preview);
            if existing.qualities.is_empty() {
                existing.qualities = media.qualities;
            }
        } else {
            seen.insert(media.url.clone(), results.len());
            results.push(media);
        }
    }
    let (assets, media): (Vec<_>, Vec<_>) = results.into_iter().partition(|item| item.page_asset);
    media
        .into_iter()
        .take(MAIN_MEDIA_LIMIT)
        .chain(assets.into_iter().take(PAGE_ASSET_LIMIT))
        .collect()
}

pub async fn find(client: &Client, value: &str) -> AppResult<Vec<Media>> {
    let mut response = client
        .get(url(value)?)
        .send()
        .await
        .map_err(|_| "network")?
        .error_for_status()
        .map_err(page_error)?;
    let base = response.url().clone();
    if let Some((extension, kind)) = response_format(&response) {
        return Ok(vec![Media {
            url: base.to_string(),
            name: media_name(&base, extension),
            kind,
            size: response.content_length(),
            width: None,
            height: None,
            duration: None,
            preview: matches!(kind, Kind::Image | Kind::Gif).then(|| base.to_string()),
            preview_only: false,
            page_asset: false,
            qualities: Vec::new(),
        }]);
    }
    Ok(parse(&read_html(&mut response).await?, &base))
}

const MAX_COLLECTION_PAGES: usize = 10;
const MAX_POSTS: usize = 120;
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(45);

fn page_error(error: reqwest::Error) -> &'static str {
    match error.status() {
        Some(StatusCode::UNAUTHORIZED) => "pageUnauthorized",
        Some(StatusCode::FORBIDDEN) => "pageForbidden",
        _ => "httpError",
    }
}

#[derive(Serialize)]
pub struct Collection {
    items: Vec<Media>,
    partial: bool,
}

fn structured_links(
    value: &serde_json::Value,
    videos: &mut Vec<Media>,
    posts: &mut Vec<String>,
    base: &Url,
    in_list: bool,
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                structured_links(value, videos, posts, base, in_list);
            }
        }
        serde_json::Value::Object(object) => {
            let field = |key| object.get(key).and_then(serde_json::Value::as_str);
            let is_type = |name: &str| {
                let matches = |value: &str| value.rsplit('/').next() == Some(name);
                field("@type").is_some_and(matches)
                    || object
                        .get("@type")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|types| {
                            types
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .any(matches)
                        })
            };
            if is_type("VideoObject")
                && let Some(mut link) = field("contentUrl").and_then(|link| base.join(link).ok())
                && allowed(&link)
                && link.as_str().len() <= 8192
                && let Some((extension, Kind::Video)) = path_format(&link)
            {
                link.set_fragment(None);
                let thumbnail = field("thumbnailUrl")
                    .or_else(|| object.get("thumbnailUrl")?.as_array()?.first()?.as_str());
                videos.push(Media {
                    url: link.to_string(),
                    name: field("name")
                        .filter(|name| !name.trim().is_empty())
                        .map(|name| name.chars().take(120).collect())
                        .unwrap_or_else(|| media_name(&link, extension)),
                    kind: Kind::Video,
                    size: None,
                    width: None,
                    height: None,
                    duration: None,
                    preview: thumbnail
                        .and_then(|value| base.join(value).ok())
                        .filter(allowed)
                        .map(|url| url.to_string()),
                    preview_only: false,
                    page_asset: false,
                    qualities: Vec::new(),
                });
            }
            if in_list && is_type("ListItem") {
                if let Some(link) =
                    field("url").or_else(|| object.get("item")?.get("url")?.as_str())
                {
                    posts.push(link.to_owned());
                }
            }
            for (key, child) in object {
                structured_links(
                    child,
                    videos,
                    posts,
                    base,
                    key == "itemListElement" && is_type("ItemList") && !is_type("BreadcrumbList"),
                );
            }
        }
        _ => {}
    }
}

fn collection_link(base: &Url, origin: &Url, raw: &str) -> Option<Url> {
    let mut link = base.join(raw).ok()?;
    link.set_fragment(None);
    (allowed(&link)
        && link.origin() == origin.origin()
        && link.as_str().len() <= 8192
        && path_format(&link).is_none()
        && !link.path().rsplit('/').next().is_some_and(|last| {
            let lower = last.to_ascii_lowercase();
            lower.ends_with(".m3u8") || lower.ends_with(".mpd")
        }))
    .then_some(link)
}

fn add_collection_videos(items: &mut Vec<Media>, seen: &mut HashSet<String>, videos: Vec<Media>) {
    for video in videos {
        if items.len() >= MAIN_MEDIA_LIMIT {
            break;
        }
        if video.kind == Kind::Video
            && !video.page_asset
            && !video.preview_only
            && seen.insert(video.url.clone())
        {
            items.push(video);
        }
    }
}

async fn collection_page(client: &Client, address: &Url) -> AppResult<(Url, Html)> {
    let mut response = client
        .get(address.clone())
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|_| "network")?
        .error_for_status()
        .map_err(page_error)?;
    let base = response.url().clone();
    Ok((base, tokenize(&read_html(&mut response).await?, true)))
}

pub async fn find_collection(client: &Client, value: &str) -> AppResult<Collection> {
    use futures_util::{StreamExt, stream};

    let address = url(value)?;
    let deadline = tokio::time::Instant::now() + COLLECTION_TIMEOUT;
    let (origin, first) = collection_page(client, &address).await?;
    let allowed_origin = origin.origin();
    let client = Client::builder()
        .user_agent("MediaDownloader/0.1")
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() < 8
                && allowed(attempt.url())
                && attempt.url().origin() == allowed_origin
            {
                attempt.follow()
            } else {
                attempt.error("Invalid collection redirect")
            }
        }))
        .build()
        .map_err(|_| "network")?;
    let mut first = Some((origin.clone(), first));
    let mut items = Vec::new();
    let mut videos_seen = HashSet::new();
    let mut pages = std::collections::VecDeque::from([origin.clone()]);
    let mut visited = HashSet::from([address, origin.clone()]);
    let mut posts = Vec::new();
    let mut partial = false;

    for _ in 0..MAX_COLLECTION_PAGES {
        let Some(address) = pages.pop_front() else {
            break;
        };
        let (base, page) = match first.take() {
            Some(page) => page,
            None => {
                match tokio::time::timeout_at(deadline, collection_page(&client, &address)).await {
                    Ok(Ok(page)) => page,
                    Ok(Err(_)) => {
                        partial = true;
                        continue;
                    }
                    Err(_) => {
                        partial = true;
                        break;
                    }
                }
            }
        };
        visited.insert(base.clone());
        let mut links = page.posts.take();
        let mut structured = Vec::new();
        for script in page.json_ld.take() {
            if let Ok(json) = serde_json::from_str(&script) {
                structured_links(&json, &mut structured, &mut links, &base, false);
            }
        }
        let next = page.next.take();
        add_collection_videos(&mut items, &mut videos_seen, parse_media(page, &base));
        add_collection_videos(&mut items, &mut videos_seen, structured);
        for raw in next {
            if let Some(link) = collection_link(&base, &origin, &raw) {
                if visited.insert(link.clone()) {
                    pages.push_back(link);
                }
            }
        }
        posts.extend(
            links
                .into_iter()
                .filter_map(|raw| collection_link(&base, &origin, &raw)),
        );
    }
    partial |= !pages.is_empty();
    posts.retain(|post| visited.insert(post.clone()));
    partial |= posts.len() > MAX_POSTS;
    let mut pending = stream::iter(posts.into_iter().take(MAX_POSTS).map(|post| {
        let client = &client;
        async move {
            let (base, page) = collection_page(client, &post).await?;
            let mut videos = Vec::new();
            for script in page.json_ld.take() {
                if let Ok(json) = serde_json::from_str(&script) {
                    structured_links(&json, &mut videos, &mut Vec::new(), &base, false);
                }
            }
            videos.extend(parse_media(page, &base));
            Ok::<_, &'static str>(videos)
        }
    }))
    .buffered(4);
    loop {
        let result = match tokio::time::timeout_at(deadline, pending.next()).await {
            Ok(Some(result)) => result,
            Ok(None) => break,
            Err(_) => {
                partial = true;
                break;
            }
        };
        match result {
            Ok(videos) => add_collection_videos(&mut items, &mut videos_seen, videos),
            Err(_) => partial = true,
        }
        if items.len() >= MAIN_MEDIA_LIMIT {
            partial = true;
            break;
        }
    }
    Ok(Collection { items, partial })
}

pub async fn read_html(response: &mut Response) -> AppResult<String> {
    if !matches!(
        mime(response.headers()).to_ascii_lowercase().as_str(),
        "text/html" | "application/xhtml+xml" | ""
    ) {
        return Err("unsupported");
    }
    let charset = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';').skip(1).find_map(|p| {
                p.trim()
                    .split_once('=')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("charset"))
                    .map(|(_, v)| v.trim_matches([' ', '"', '\'']))
            })
        })
        .and_then(|v| encoding_rs::Encoding::for_label(v.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let data = read_limited(response, 4 * 1024 * 1024, "pageTooLarge").await?;
    Ok(charset.decode(&data).0.into_owned())
}

pub async fn inspect(client: &Client, value: &str) -> AppResult<Option<u64>> {
    let response = client
        .head(url(value)?)
        .send()
        .await
        .map_err(|_| "network")?
        .error_for_status()
        .map_err(|_| "httpError")?;
    response_format(&response).ok_or("unsupported")?;
    Ok(response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok()))
}

pub async fn preview(client: &Client, value: &str) -> AppResult<String> {
    let mut response = client
        .get(url(value)?)
        .send()
        .await
        .map_err(|_| "network")?
        .error_for_status()
        .map_err(|_| "httpError")?;
    let (ext, kind) = response_format(&response).ok_or("unsupported")?;
    if !matches!(kind, Kind::Image | Kind::Gif) {
        return Err("unsupported");
    }
    let data = read_limited(&mut response, 6 * 1024 * 1024, "previewTooLarge").await?;
    let mime = match ext {
        "jpg" => "jpeg",
        "svg" => "svg+xml",
        "ico" => "x-icon",
        _ => ext,
    };
    Ok(format!(
        "data:image/{mime};base64,{}",
        STANDARD.encode(data)
    ))
}

async fn read_limited(
    response: &mut Response,
    limit: usize,
    error: &'static str,
) -> AppResult<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "network")? {
        if bytes.len() + chunk.len() > limit {
            return Err(error);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_sources_of_one_player_in_quality_order() {
        let source = Url::parse("https://example.org/page").unwrap();
        let items = parse(
            r#"
            <video height="300"><source src="/clip-low.mp4" data-quality="360p">
              <source src="/clip-1080p.mp4" type="video/mp4">
              <source src="/clip-med.webm" label="720p"></video>
            <video src="/another.mp4"></video>
        "#,
            &source,
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "clip-1080p.mp4");
        let heights: Vec<_> = items[0].qualities.iter().map(Quality::resolution).collect();
        assert_eq!(heights, [1080, 720, 360]);
        assert!(items[1].qualities.is_empty());
    }

    #[test]
    fn discovers_relative_sources_posters_and_entities() {
        let page = Url::parse("https://example.org/articles/").unwrap();
        let result = parse(
            r#"<base href="/media/"><script>const x = '<img src="bogus.jpg">'</script>
            <img src="p.jpg?v=1&amp;k=2"><video poster="p.jpg"><source src="movie" type="video/mp4"></video>
            <a href="p.jpg?v=1&amp;k=2#dup">repeat</a><audio src="a.mp3"></audio>"#,
            &page,
        );
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].url, "https://example.org/media/p.jpg?v=1&k=2");
        assert_eq!(
            result[1].preview.as_deref(),
            Some("https://example.org/media/p.jpg")
        );
        assert_eq!(result[2].kind, Kind::Audio);
    }

    #[test]
    fn pairs_embedded_video_with_page_cover() {
        let page = Url::parse("https://example.org/watch?v=123").unwrap();
        let result = parse(
            r#"<meta property="og:title" content="Demo &amp; title">
               <meta property="og:image" content="/cover.jpg">
               <meta property="og:video:url" content="/embed/123">
               <meta property="og:video:secure_url" content="/embed/123">
               <meta property="og:video:type" content="text/html">
               <video src="/direct.mp4"></video>"#,
            &page,
        );
        assert_eq!(result.len(), 3);
        assert_eq!(result[1].name, "Demo & title");
        assert_eq!(
            result[1].preview.as_deref(),
            Some("https://example.org/cover.jpg")
        );
        assert!(result[1].preview_only);
        assert!(!result[0].preview_only && !result[2].preview_only);
    }

    #[test]
    fn separates_page_assets_from_photo_video_and_gif() {
        let page = Url::parse("https://example.org/").unwrap();
        let result = parse(
            r#"<link rel="shortcut icon" href="/mark">
               <a href="/still.png">Image</a><img class="site-logo" src="/still.png">
               <img src="/photo.jpg">
               <img src="/art.svg">
               <img src="/animation.GIF?v=1">
               <img src="/spinner.gif">
               <img class="emoji-img" src="/faces/01.png">
               <img class="crown-icon crown-emoji" src="/crown.webp">
               <video width="64" height="64"><source src="/decoration.mp4"></video>
               <video data-src="/feature.mp4"></video>
               <img src="/tall.png" width="40" height="800">"#,
            &page,
        );
        let entries: Vec<_> = result
            .iter()
            .map(|item| (item.name.as_str(), item.kind, item.page_asset))
            .collect();
        assert_eq!(
            entries,
            [
                ("photo.jpg", Kind::Image, false),
                ("art.svg", Kind::Image, false),
                ("animation.gif", Kind::Gif, false),
                ("feature.mp4", Kind::Video, false),
                ("tall.png", Kind::Image, false),
                ("mark", Kind::Image, true),
                ("still.png", Kind::Image, true),
                ("spinner.gif", Kind::Gif, true),
                ("01.png", Kind::Image, true),
                ("crown.webp", Kind::Image, true),
                ("decoration.mp4", Kind::Video, true),
            ]
        );
        assert_eq!(mime_format("image/gif"), Some(("gif", Kind::Gif)));
        assert!(result[2].preview.is_some());
        assert!(result[7].preview.is_some());
    }

    #[test]
    fn validates_urls_and_windows_names() {
        assert!(url("file:///C:/bad").is_err());
        assert!(url("https://u:p@example.org/image.png").is_err());
        assert_eq!(
            media_name(&url("https://example.org/CON.png").unwrap(), "png"),
            "media.png"
        );
    }
}
