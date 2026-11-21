use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream};
use html5ever::{
    tendril::StrTendril,
    tokenizer::states::{Rawtext, Rcdata, ScriptData},
    tokenizer::{
        BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    },
};
use regex::Regex;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{CACHE_CONTROL, CONTENT_TYPE, HeaderMap},
};
use serde::{Deserialize, Serialize};

pub type AppResult<T> = Result<T, &'static str>;

const MAIN_MEDIA_LIMIT: usize = 300;
const PAGE_ASSET_LIMIT: usize = 100;
pub const GALLERY_LIMIT: usize = MAIN_MEDIA_LIMIT + PAGE_ASSET_LIMIT;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Media {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_origin: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo_title: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_origin: Option<String>,
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
    client_with_user_agent("MediaDownloader/0.1")
}

pub fn client_with_user_agent(user_agent: &str) -> AppResult<Client> {
    Client::builder()
        .user_agent(user_agent)
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

pub fn media_referer(source: &Url, target: &Url) -> Option<String> {
    (source.scheme() != "https" || target.scheme() == "https")
        .then(|| format!("{}/", source.origin().ascii_serialization()))
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

pub fn is_hls(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with(".m3u8")
}

pub fn is_dash(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with(".mpd")
}

pub(crate) fn stream_mime(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "application/vnd.apple.mpegurl"
        | "application/x-mpegurl"
        | "audio/mpegurl"
        | "audio/x-mpegurl" => Some("m3u8"),
        "application/dash+xml" => Some("mpd"),
        _ => None,
    }
}

pub(crate) fn stream_id(url: &Url, extension: Option<&str>) -> Option<&'static str> {
    match extension {
        Some("mpd") => Some("dash:default"),
        Some("m3u8") => Some("hls:default"),
        _ if is_dash(url) => Some("dash:default"),
        _ if is_hls(url) => Some("hls:default"),
        _ => None,
    }
}

fn mime_format(mime: &str) -> Option<(&'static str, Kind)> {
    Some(match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" => ("jpg", Kind::Image),
        "image/png" => ("png", Kind::Image),
        "image/apng" => ("apng", Kind::Image),
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

pub(crate) fn path_format(url: &Url) -> Option<(&'static str, Kind)> {
    file_format(url.path().trim_end_matches('/'))
}

fn same_media_address(address: &Url, variant: &Url) -> bool {
    address == variant
        || address.query().is_none()
            && variant.origin() == address.origin()
            && variant.path() == address.path()
}

pub fn file_format(name: &str) -> Option<(&'static str, Kind)> {
    Some(
        match name.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => ("jpg", Kind::Image),
            "png" => ("png", Kind::Image),
            "apng" => ("apng", Kind::Image),
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
        || value.split(['-', '_']).any(|part| part == "logo")
        || value.starts_with("emoji-")
        || value.starts_with("icon-")
        || value.ends_with("-icon")
        || value.ends_with("-emoji")
}

fn asset_path(url: &Url) -> bool {
    let decoded = percent_encoding::percent_decode_str(url.path()).decode_utf8_lossy();
    let (directory, file) = decoded.rsplit_once('/').unwrap_or(("", &decoded));
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    directory.split('/').any(asset_label) || asset_class(stem)
}

fn image_key(mut url: Url) -> Url {
    url.set_fragment(None);
    if path_format(&url).is_some_and(|(_, kind)| matches!(kind, Kind::Image | Kind::Gif)) {
        let query: Vec<_> = url
            .query_pairs()
            .into_owned()
            .filter(|(key, _)| {
                !matches!(key.as_str(), "w" | "width" | "h" | "height" | "dpr" | "fit")
            })
            .collect();
        url.set_query(None);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
    }
    url
}

fn image_fragment(url: &Url) -> bool {
    url.fragment()
        .is_some_and(|fragment| fragment.starts_with("xywh="))
        || url
            .path_segments()
            .and_then(Iterator::last)
            .is_some_and(|segment| {
                percent_encoding::percent_decode_str(segment)
                    .decode_utf8_lossy()
                    .strip_prefix("vts:")
                    .is_some_and(|index| {
                        !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                    })
            })
}

pub fn media_name(url: &Url, extension: &str) -> String {
    let segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(segment).decode_utf8_lossy();
    let stem = decoded
        .rsplit_once('.')
        .map_or(decoded.as_ref(), |(stem, _)| stem);
    safe_media_name(stem, extension)
}

pub fn clean_file_component(value: &str, limit: usize) -> String {
    value
        .chars()
        .take(limit)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

pub fn safe_file_stem(stem: &str) -> Option<String> {
    let clean = clean_file_component(stem, 100);
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
    (!name.is_empty() && !reserved).then(|| name.to_owned())
}

pub fn safe_media_name(stem: &str, extension: &str) -> String {
    let name = safe_file_stem(stem).unwrap_or_else(|| "media".to_owned());
    format!("{name}.{extension}")
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
    asset_anchor: Cell<bool>,
    picture: Cell<Option<usize>>,
    picture_count: Cell<usize>,
    posts: RefCell<Vec<String>>,
    next: RefCell<Vec<String>>,
    json_ld: RefCell<Vec<String>>,
    in_json_ld: Cell<bool>,
    script: RefCell<Option<String>>,
    players: RefCell<Vec<Player>>,
    inline_players: RefCell<Vec<InlinePlayer>>,
}

struct InlinePlayer {
    poster: Option<String>,
    sources: Vec<(String, Option<u32>)>,
}

#[derive(Deserialize)]
struct Player {
    image_url: String,
    #[serde(rename = "mediaDefinitions")]
    sources: Vec<PlayerSource>,
}

#[derive(Deserialize)]
struct PlayerSource {
    format: String,
    quality: Option<serde_json::Value>,
    #[serde(rename = "videoUrl")]
    url: String,
}

#[derive(Default)]
struct Candidate {
    link: String,
    photo_title: Option<String>,
    format: Option<(&'static str, Kind)>,
    hint: Option<Kind>,
    width: Option<u32>,
    height: Option<u32>,
    poster: Option<String>,
    embedded: bool,
    page_asset: bool,
    video_group: Option<usize>,
    quality_height: Option<u32>,
    meta_video: bool,
    picture: Option<usize>,
}

fn image_format(format: &(&str, Kind)) -> bool {
    matches!(format.1, Kind::Image | Kind::Gif)
}

fn image_srcset(mut input: &str) -> Vec<(&str, Option<u32>)> {
    let mut entries = Vec::new();
    while entries.len() < 3000 {
        input = input.trim_start_matches(|c: char| c == ',' || c.is_ascii_whitespace());
        if input.is_empty() {
            break;
        }
        let end = input
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(input.len());
        let (address, rest) = input.split_at(end);
        let (descriptor, next) = if address.ends_with(',') {
            ("", rest)
        } else {
            rest.split_once(',').unwrap_or((rest, ""))
        };
        input = next;
        let address = address.trim_end_matches(',');
        if address.starts_with("data:") {
            continue;
        }
        let descriptor = descriptor.trim();
        let width = descriptor
            .strip_suffix('w')
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value != 0);
        let density = descriptor
            .strip_suffix('x')
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0);
        if descriptor.is_empty() || width.is_some() || density.is_some() {
            entries.push((address, width, density.unwrap_or(1.0)));
        }
    }
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.total_cmp(&a.2)));
    entries
        .into_iter()
        .map(|(address, width, _)| (address, width))
        .collect()
}

fn technical_photo_name(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("rs:fit:")
        || lower.starts_with("vts:")
        || lower.contains("(m=")
        || lower.contains("(mh=")
}

fn meaningful_photo_title(value: &str) -> bool {
    value.chars().any(char::is_alphabetic)
        && !technical_photo_name(value)
        && !matches!(
            value.to_lowercase().as_str(),
            "photo"
                | "image"
                | "picture"
                | "thumbnail"
                | "video thumbnail"
                | "preview"
                | "фото"
                | "зображення"
                | "мініатюра"
                | "прев’ю"
                | "превью"
                | "изображение"
                | "картинка"
                | "миниатюра"
        )
}

fn declared_quality(value: &str) -> Option<u32> {
    value
        .trim()
        .trim_end_matches(['p', 'P'])
        .parse::<u32>()
        .ok()
        .filter(|height| (100..=8640).contains(height))
}

fn inline_players(script: &str) -> impl Iterator<Item = InlinePlayer> + '_ {
    static OBJECT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)\b(?:var|let|const)\s+[\w$]+\s*=\s*\{(.*?)\}\s*;").unwrap()
    });
    OBJECT
        .captures_iter(script)
        .filter_map(|object| object.get(1))
        .filter_map(|body| inline_player(body.as_str()))
}

fn inline_player(body: &str) -> Option<InlinePlayer> {
    static FIELD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?:^|,)\s*['"]?(preview_url|video_(?:url|alt_url[0-9]*)(?:_text)?)['"]?\s*:\s*(?:'([^'\r\n]*)'|"([^"\r\n]*)")"#,
        )
        .unwrap()
    });
    let fields: HashMap<_, _> = FIELD
        .captures_iter(body)
        .map(|field| {
            (
                field.get(1).unwrap().as_str(),
                field.get(2).or_else(|| field.get(3)).unwrap().as_str(),
            )
        })
        .collect();
    let sources: Vec<_> = std::iter::once("video_url".to_owned())
        .chain(std::iter::once("video_alt_url".to_owned()))
        .chain((1..=16).map(|index| format!("video_alt_url{index}")))
        .filter_map(|key| {
            let address = fields.get(key.as_str())?.replace("\\/", "/");
            let height = fields
                .get(format!("{key}_text").as_str())
                .and_then(|value| declared_quality(value));
            Some((address, height))
        })
        .collect();
    (!sources.is_empty()).then(|| InlinePlayer {
        poster: fields
            .get("preview_url")
            .map(|value| value.replace("\\/", "/")),
        sources,
    })
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
            Token::CharacterTokens(text) => {
                if let Some(script) = self.script.borrow_mut().as_mut() {
                    script.push_str(&text);
                }
                return TokenSinkResult::Continue;
            }
            _ => return TokenSinkResult::Continue,
        };
        let name = tag.name.as_ref();
        if tag.kind == TagKind::EndTag {
            if name == "script" {
                self.in_json_ld.set(false);
                if let Some(script) = self.script.borrow_mut().take() {
                    if script.contains("video_url") || script.contains("video_alt_url") {
                        self.inline_players
                            .borrow_mut()
                            .extend(inline_players(&script));
                    }
                    for assignment in script.split("var flashvars_").skip(1) {
                        let Some((id, value)) = assignment.split_once('=') else {
                            continue;
                        };
                        if !id.trim().bytes().all(|byte| byte.is_ascii_digit()) {
                            continue;
                        }
                        let mut json =
                            serde_json::Deserializer::from_str(value).into_iter::<Player>();
                        if let Some(Ok(player)) = json.next() {
                            self.players.borrow_mut().push(player);
                        }
                    }
                }
            }
            if name == "a" {
                self.anchor.replace(None);
                self.asset_anchor.set(false);
            }
            if name == "picture" {
                self.picture.set(None);
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
        let marked_asset = attr("class")
            .is_some_and(|classes| classes.split_ascii_whitespace().any(asset_class))
            || attr("id").is_some_and(asset_class)
            || attr("alt").is_some_and(|alt| {
                alt.split_ascii_whitespace()
                    .any(|part| part.eq_ignore_ascii_case("logo"))
            });
        let asset = marked_asset || explicitly_small;
        if name == "a" {
            self.asset_anchor.set(
                asset
                    || attr("href") == Some("/")
                    || attr("rel").is_some_and(|rel| {
                        rel.split_ascii_whitespace()
                            .any(|token| token.eq_ignore_ascii_case("home"))
                    }),
            );
        }
        let srcset = ["data-srcset", "data-lazy-srcset", "srcset"]
            .into_iter()
            .filter_map(attr)
            .map(image_srcset)
            .find(|entries| !entries.is_empty());
        if name == "picture" || (name == "img" && srcset.is_some() && self.picture.get().is_none())
        {
            self.picture_count.set(self.picture_count.get() + 1);
        }
        if name == "picture" {
            self.picture.set(Some(self.picture_count.get()));
        }
        let picture = self
            .picture
            .get()
            .or_else(|| (name == "img" && srcset.is_some()).then(|| self.picture_count.get()));
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
        let push_sized = |link: &str,
                          hint: Option<Kind>,
                          poster: Option<String>,
                          page_asset: bool,
                          (width, height): (Option<u32>, Option<u32>)| {
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
                photo_title: (name == "img" && !page_asset)
                    .then(|| {
                        ["title", "alt"]
                            .into_iter()
                            .filter_map(attr)
                            .map(str::trim)
                            .find(|value| meaningful_photo_title(value))
                            .map(|value| value.chars().take(120).collect())
                    })
                    .flatten(),
                format: attr("type").and_then(|value| {
                    stream_mime(value)
                        .map(|ext| (ext, Kind::Video))
                        .or_else(|| mime_format(value))
                }),
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
                meta_video: name == "meta" && hint == Some(Kind::Video),
                picture: picture
                    .filter(|_| matches!(name, "img" | "source") && self.player.get().is_none()),
            });
        };
        let push = |link, hint, poster, page_asset| {
            push_sized(link, hint, poster, page_asset, (width, height));
        };
        match name {
            "script" => {
                if attr("src").is_none()
                    && attr("type")
                        .is_some_and(|value| value.eq_ignore_ascii_case("application/ld+json"))
                {
                    self.json_ld.borrow_mut().push(String::new());
                    self.in_json_ld.set(true);
                } else if attr("src").is_none() && !self.collect_links {
                    self.script.replace(Some(String::new()));
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
                let picture_source = picture.is_some() && self.player.get().is_none();
                let hint = if name == "img" || picture_source {
                    Some(Kind::Image)
                } else {
                    self.player.get()
                };
                let page_asset = asset
                    || (name == "source" && self.player_asset.get())
                    || (self.player.get().is_none() && self.asset_anchor.get());
                if matches!(hint, Some(Kind::Image)) {
                    let variant_asset = marked_asset || self.asset_anchor.get();
                    if let Some(link) = attr("data-original") {
                        push_sized(link, hint, None, variant_asset, (None, None));
                    }
                    if let Some(entries) = srcset {
                        for (link, width) in entries {
                            push_sized(link, hint, None, variant_asset, (width, None));
                        }
                    }
                    if let Some(link) = attr("data-lazy-src")
                        .or_else(|| attr("data-src"))
                        .or_else(|| attr("src"))
                    {
                        push(link, hint, None, page_asset);
                    }
                } else if let Some(link) = attr("data-src").or_else(|| attr("src")) {
                    push(link, hint, self.poster.borrow().clone(), page_asset);
                }
            }
            "a" => {
                if let Some(link) = attr("href") {
                    let hint = attr("type")
                        .and_then(mime_format)
                        .filter(image_format)
                        .map(|(_, kind)| kind);
                    push(link, hint, None, false);
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

fn document_base(page: &Html, source: &Url) -> Url {
    page.base
        .borrow()
        .as_deref()
        .and_then(|value| source.join(value).ok())
        .filter(allowed)
        .unwrap_or_else(|| source.clone())
}

fn parse_media(page: Html, source: &Url) -> Vec<Media> {
    let base = document_base(&page, source);
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
    let page_cover = page_image
        .as_deref()
        .and_then(|value| base.join(value).ok());
    let player_sources = page.players.into_inner();
    let primary = page_cover
        .as_ref()
        .filter(|cover| allowed(cover))
        .and_then(|cover| {
            let mut matching = player_sources
                .iter()
                .filter(|player| base.join(&player.image_url).ok().as_ref() == Some(cover));
            let player = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            let mut direct = candidates.iter().filter_map(|item| {
                let mut address = base.join(&item.link).ok()?;
                address.set_fragment(None);
                (item.meta_video
                    && !embedded.contains(&address)
                    && allowed(&address)
                    && path_format(&address) == Some(("mp4", Kind::Video)))
                .then_some(address)
            });
            let address = direct.next()?;
            if direct.any(|other| other != address) {
                return None;
            }
            let mut qualities: Vec<_> = player
                .sources
                .iter()
                .take(32)
                .filter_map(|variant| {
                    if variant.format != "hls" {
                        return None;
                    }
                    let quality = variant.quality.as_ref()?;
                    let height = match quality {
                        serde_json::Value::String(value) => declared_quality(value)?,
                        serde_json::Value::Number(value) => declared_quality(&value.to_string())?,
                        _ => return None,
                    };
                    let address = base.join(&variant.url).ok()?;
                    if !allowed(&address)
                        || !address.path().to_ascii_lowercase().ends_with(".m3u8")
                        || (source.scheme() == "https" && address.scheme() != "https")
                    {
                        return None;
                    }
                    Some(Quality {
                        url: Some(address.to_string()),
                        source_origin: None,
                        format_id: None,
                        extension: "mp4".to_owned(),
                        width: None,
                        height: Some(height),
                        size: None,
                    })
                })
                .collect();
            qualities.sort_by_key(|quality| std::cmp::Reverse(quality.resolution()));
            qualities.dedup_by_key(|quality| quality.resolution());
            qualities.truncate(16);
            Some((address, qualities))
        });
    let mut pictures = HashMap::<usize, HashSet<Url>>::new();
    for item in &candidates {
        if let Some(id) = item.picture
            && let Ok(address) = base.join(&item.link)
            && allowed(&address)
        {
            pictures.entry(id).or_default().insert(image_key(address));
        }
    }
    let mut seen = HashMap::new();
    let mut results: Vec<Media> = Vec::new();
    let mut groups = HashMap::<usize, Vec<(Media, Quality)>>::new();
    let mut entries = Vec::new();
    for (media, group) in candidates.into_iter().filter_map(|item| {
        let mut url = base.join(&item.link).ok()?;
        if item.hint == Some(Kind::Image) && image_fragment(&url) {
            return None;
        }
        url.set_fragment(None);
        if !allowed(&url) {
            return None;
        }
        let stream_id = stream_id(&url, item.format.map(|(ext, _)| ext));
        let stream = stream_id.is_some();
        if stream && source.scheme() == "https" && url.scheme() != "https" {
            return None;
        }
        let (extension, kind) = if stream {
            ("mp4", Kind::Video)
        } else {
            item.format
                .or_else(|| path_format(&url))
                .map(|(ext, kind)| {
                    if kind == Kind::Gif {
                        (ext, kind)
                    } else {
                        (ext, item.hint.unwrap_or(kind))
                    }
                })
                .or_else(|| item.hint.map(|kind| ("", kind)))?
        };
        let preview_only = kind == Kind::Video && embedded.contains(&url);
        let primary_video = primary.as_ref().filter(|(address, _)| *address == url);
        if preview_only && item.meta_video && primary.is_some() {
            return None;
        }
        let page_video = preview_only
            || primary_video.is_some()
            || item.meta_video
            || (stream && item.video_group.is_some());
        let preview = if matches!(kind, Kind::Image | Kind::Gif) {
            Some(url.to_string())
        } else {
            item.poster
                .as_deref()
                .or_else(|| {
                    if page_video {
                        page_image.as_deref()
                    } else {
                        None
                    }
                })
                .and_then(|v| base.join(v).ok())
                .filter(allowed)
                .map(|v| v.to_string())
        };
        let name = if let Some(title) = title.as_ref().filter(|_| page_video) {
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
        let photo_title = (kind == Kind::Image && technical_photo_name(&name))
            .then_some(item.photo_title)
            .flatten();
        let group = item
            .video_group
            .filter(|_| kind == Kind::Video && !preview_only && matches!(extension, "mp4" | "webm"))
            .map(|id| {
                (
                    id,
                    Quality {
                        url: Some(url.to_string()),
                        source_origin: None,
                        format_id: stream_id.map(str::to_owned),
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
                source_origin: media_referer(source, &url),
                name,
                photo_title,
                kind,
                size: None,
                width: item.width,
                height: item.height,
                duration: None,
                preview,
                preview_only,
                page_asset: kind != Kind::Audio
                    && !preview_only
                    && (item.page_asset || matches!(extension, "ico" | "svg") || asset_path(&url)),
                qualities: if let Some(id) = stream_id {
                    vec![stream_quality(&url, id, item.quality_height)]
                } else {
                    primary_video.map_or_else(Vec::new, |(_, qualities)| qualities.clone())
                },
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
        if group.len() > 1
            && group
                .iter()
                .any(|(_, quality)| quality.resolution() > 0 || quality.format_id.is_some())
        {
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
            existing.photo_title = existing.photo_title.take().or(media.photo_title);
            existing.preview = existing.preview.take().or(media.preview);
            if existing.qualities.is_empty() {
                existing.qualities = media.qualities;
            }
        } else {
            seen.insert(media.url.clone(), results.len());
            results.push(media);
        }
    }
    let covers: HashSet<_> = results
        .iter()
        .filter(|item| item.kind == Kind::Video && !item.page_asset)
        .filter_map(|item| Url::parse(item.preview.as_deref()?).ok())
        .map(image_key)
        .collect();
    let mut cover_variants = covers.clone();
    for links in pictures
        .into_values()
        .filter(|links| !links.is_disjoint(&covers))
    {
        cover_variants.extend(links);
    }
    results.retain(|item| {
        !matches!(item.kind, Kind::Image | Kind::Gif)
            || Url::parse(&item.url)
                .ok()
                .is_none_or(|url| !cover_variants.contains(&image_key(url)))
    });
    let (assets, media): (Vec<_>, Vec<_>) = results.into_iter().partition(|item| item.page_asset);
    media
        .into_iter()
        .take(MAIN_MEDIA_LIMIT)
        .chain(assets.into_iter().take(PAGE_ASSET_LIMIT))
        .collect()
}

pub(crate) fn stream_quality(url: &Url, id: &str, height: Option<u32>) -> Quality {
    Quality {
        url: Some(url.to_string()),
        source_origin: None,
        format_id: Some(id.to_owned()),
        extension: "mp4".to_owned(),
        width: None,
        height,
        size: None,
    }
}

async fn probe_video_source(
    client: &Client,
    page: &Url,
    address: Url,
    height: Option<u32>,
) -> Option<Quality> {
    let mut source_origin = media_referer(page, &address)?;
    let request = |referer: &str| {
        client
            .get(address.clone())
            .header(reqwest::header::RANGE, "bytes=0-0")
            .header(reqwest::header::REFERER, referer)
            .timeout(Duration::from_secs(6))
            .send()
    };
    let mut response = request(&source_origin).await.ok()?;
    if response.status() == StatusCode::FORBIDDEN && page.origin() != address.origin() {
        source_origin = media_referer(&address, &address)?;
        drop(response);
        response = request(&source_origin).await.ok()?;
    }
    if !matches!(
        response.status(),
        StatusCode::OK | StatusCode::PARTIAL_CONTENT
    ) {
        return None;
    }
    if let Some(id) =
        stream_mime(mime(response.headers())).and_then(|format| stream_id(&address, Some(format)))
    {
        return Some(Quality {
            source_origin: Some(source_origin),
            ..stream_quality(&address, id, height)
        });
    }
    let (extension, Kind::Video) = response_format(&response)? else {
        return None;
    };
    let size = match response.status() {
        StatusCode::PARTIAL_CONTENT => response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit_once('/'))
            .and_then(|(_, total)| total.parse().ok()),
        StatusCode::OK => response.content_length(),
        _ => return None,
    };
    Some(Quality {
        url: Some(address.to_string()),
        source_origin: Some(source_origin),
        format_id: None,
        extension: extension.to_owned(),
        width: None,
        height,
        size,
    })
}

async fn verified_inline_media(client: &Client, source: &Url, page: &mut Html) -> Option<Media> {
    let players = page.inline_players.get_mut();
    if players.len() != 1 {
        return None;
    }
    let InlinePlayer { poster, sources } = players.pop()?;
    let base = page
        .base
        .get_mut()
        .as_deref()
        .and_then(|value| source.join(value).ok())
        .filter(allowed)
        .unwrap_or_else(|| source.clone());
    let resolve = |value: &str| {
        if value.len() > 8192 {
            return None;
        }
        let mut url = base.join(value).ok()?;
        url.set_fragment(None);
        (allowed(&url) && media_referer(source, &url).is_some()).then_some(url)
    };
    let mut seen = HashSet::new();
    let links: Vec<_> = sources
        .into_iter()
        .filter_map(|(value, height)| {
            let address = resolve(&value)?;
            (matches!(path_format(&address), Some((_, Kind::Video)))
                && seen.insert(address.clone()))
            .then_some((address, height))
        })
        .collect();
    if links.is_empty() {
        return None;
    }
    let page_cover = page.page_image.get_mut().as_deref().and_then(resolve);
    let cover = poster.as_deref().and_then(resolve);
    let candidates = page.items.get_mut();
    let matches_source = |address: &Url| {
        seen.iter()
            .any(|variant| same_media_address(address, variant))
    };
    let matches_metadata = cover.is_some() && cover == page_cover
        || candidates
            .iter()
            .filter(|item| item.meta_video)
            .any(|item| {
                resolve(&item.link).is_some_and(|address| {
                    matches_source(&address)
                        || (path_format(&address).is_none()
                            && !is_hls(&address)
                            && !is_dash(&address)
                            && links
                                .iter()
                                .any(|(link, _)| link.origin() == address.origin()))
                })
            });
    let mut qualities: Vec<_> =
        stream::iter(links.into_iter().map(|(address, height)| async move {
            probe_video_source(client, source, address, height)
                .await
                .filter(|quality| quality.format_id.is_none())
        }))
        .buffered(4)
        .filter_map(std::future::ready)
        .collect()
        .await;
    qualities.sort_by_key(|quality| std::cmp::Reverse(quality.resolution()));
    qualities.truncate(16);
    let preview = cover.or_else(|| matches_metadata.then_some(page_cover).flatten());
    if qualities.is_empty() && preview.is_none() {
        return None;
    }
    if matches_metadata {
        // A page can declare both the actual file and an HTML player for the same video.
        candidates.retain(|item| {
            !item.meta_video
                || resolve(&item.link).is_some_and(|address| {
                    !matches_source(&address)
                        && (matches!(path_format(&address), Some((_, Kind::Video)))
                            || is_hls(&address)
                            || is_dash(&address))
                })
        });
    }
    let best = qualities.first();
    let address = best
        .and_then(|quality| quality.url.as_deref())
        .unwrap_or(source.as_str());
    let name = page.page_title.get_mut().clone().unwrap_or_else(|| {
        best.and_then(|quality| {
            Url::parse(address)
                .ok()
                .map(|url| media_name(&url, &quality.extension))
        })
        .unwrap_or_else(|| "media".to_owned())
    });
    Some(Media {
        url: address.to_owned(),
        source_origin: best.and_then(|quality| quality.source_origin.clone()),
        name,
        photo_title: None,
        kind: Kind::Video,
        size: best.and_then(|quality| quality.size),
        width: None,
        height: best.and_then(|quality| quality.height),
        duration: None,
        preview: preview.map(Into::into),
        preview_only: best.is_none(),
        page_asset: false,
        qualities,
    })
}

pub async fn find(client: &Client, value: &str, revalidate: bool) -> AppResult<Vec<Media>> {
    let mut request = client.get(url(value)?);
    if revalidate {
        request = request.header(CACHE_CONTROL, "no-cache");
    }
    let mut response = checked_page(request.send().await.map_err(|_| "network")?).await?;
    let base = response.url().clone();
    let stream = stream_id(&base, stream_mime(mime(response.headers())));
    if let Some((extension, kind)) = if stream.is_some() {
        Some(("mp4", Kind::Video))
    } else {
        response_format(&response)
    } {
        return Ok(vec![Media {
            url: base.to_string(),
            source_origin: None,
            name: media_name(&base, extension),
            photo_title: None,
            kind,
            size: if stream.is_some() {
                None
            } else {
                response.content_length()
            },
            width: None,
            height: None,
            duration: None,
            preview: matches!(kind, Kind::Image | Kind::Gif).then(|| base.to_string()),
            preview_only: false,
            page_asset: matches!(extension, "ico" | "svg"),
            qualities: if let Some(id) = stream {
                vec![stream_quality(&base, id, None)]
            } else {
                Vec::new()
            },
        }]);
    }
    let mut page = tokenize(&read_html(&mut response).await?, false);
    let primary = verified_inline_media(client, &base, &mut page).await;
    let declared = structured_media(&page, &base, &mut Vec::new());
    let image_base = document_base(&page, &base);
    let images = verified_image_objects(client, &base, &image_base, declared.images).await;
    page.items.get_mut().splice(..0, images);
    let mut items = parse_media(page, &base);
    if let Some(primary) = primary {
        items.retain(|item| {
            item.url != primary.url
                && !(matches!(item.kind, Kind::Image | Kind::Gif)
                    && Some(item.url.as_str()) == primary.preview.as_deref())
        });
        items.insert(0, primary);
    }
    append_verified_video_objects(client, &base, declared.videos, &mut items).await;
    items.truncate(GALLERY_LIMIT);
    Ok(items)
}

const MAX_COLLECTION_PAGES: usize = 10;
const MAX_POSTS: usize = 120;
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(45);

async fn checked_page(mut response: Response) -> AppResult<Response> {
    if response.error_for_status_ref().is_ok() {
        return Ok(response);
    }
    let status = response.status();
    if matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::SERVICE_UNAVAILABLE
    ) && matches!(
        mime(response.headers()),
        "text/html" | "application/xhtml+xml"
    ) {
        let browser_check = response
            .headers()
            .get("cf-mitigated")
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"challenge"));
        if browser_check
            || tokio::time::timeout(
                Duration::from_secs(2),
                read_limited(&mut response, 48 * 1024, "pageTooLarge"),
            )
            .await
            .is_ok_and(|body| body.is_ok_and(|bytes| is_browser_challenge(&bytes)))
        {
            return Err("pageBrowserChallenge");
        }
    }
    Err(match status {
        StatusCode::UNAUTHORIZED => "pageUnauthorized",
        StatusCode::FORBIDDEN => "pageForbidden",
        _ => "httpError",
    })
}

fn is_browser_challenge(bytes: &[u8]) -> bool {
    let html = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    html.contains("data-pow=") && html.contains("checking your browser")
}

#[derive(Serialize)]
pub struct Collection {
    pub(crate) items: Vec<Media>,
    pub(crate) partial: bool,
}

#[derive(Default)]
struct StructuredMedia {
    videos: Vec<Media>,
    images: Vec<Candidate>,
}

fn structured_links(
    value: &serde_json::Value,
    media: &mut StructuredMedia,
    posts: &mut Vec<String>,
    base: &Url,
    in_list: bool,
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                structured_links(value, media, posts, base, in_list);
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
                && let Some(source_origin) = media_referer(base, &link)
            {
                link.set_fragment(None);
                let stream = stream_id(
                    &link,
                    field("encodingFormat")
                        .and_then(stream_mime)
                        .filter(|_| path_format(&link).is_none()),
                );
                let thumbnail = field("thumbnailUrl")
                    .or_else(|| object.get("thumbnailUrl")?.as_array()?.first()?.as_str());
                media.videos.push(Media {
                    url: link.to_string(),
                    source_origin: Some(source_origin),
                    name: field("name")
                        .filter(|name| !name.trim().is_empty())
                        .map(|name| name.chars().take(120).collect())
                        .unwrap_or_else(|| {
                            media_name(&link, path_format(&link).map_or("mp4", |(ext, _)| ext))
                        }),
                    photo_title: None,
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
                    qualities: stream
                        .map_or_else(Vec::new, |id| vec![stream_quality(&link, id, None)]),
                });
            }
            if media.images.len() < 32
                && is_type("ImageObject")
                && let Some(link) = field("contentUrl").or_else(|| field("url"))
                && !link.trim().is_empty()
                && link.len() <= 8192
            {
                let format = field("encodingFormat")
                    .and_then(mime_format)
                    .filter(image_format);
                media.images.push(Candidate {
                    link: link.trim().to_owned(),
                    photo_title: field("name")
                        .map(str::trim)
                        .filter(|name| meaningful_photo_title(name))
                        .map(|name| name.chars().take(120).collect()),
                    format,
                    hint: Some(Kind::Image),
                    ..Candidate::default()
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
                    media,
                    posts,
                    base,
                    key == "itemListElement" && is_type("ItemList") && !is_type("BreadcrumbList"),
                );
            }
        }
        _ => {}
    }
}

fn structured_media(page: &Html, source: &Url, posts: &mut Vec<String>) -> StructuredMedia {
    let mut media = StructuredMedia::default();
    for script in page.json_ld.take() {
        if let Ok(json) = serde_json::from_str(&script) {
            structured_links(&json, &mut media, posts, source, false);
        }
    }
    media
}

async fn verified_image_objects(
    client: &Client,
    source: &Url,
    base: &Url,
    images: Vec<Candidate>,
) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    stream::iter(images.into_iter().filter_map(|item| {
        let mut address = base.join(&item.link).ok()?;
        if !allowed(&address) || image_fragment(&address) {
            return None;
        }
        address.set_fragment(None);
        let referer = media_referer(source, &address)?;
        seen.insert(address.clone())
            .then_some((item, address, referer))
    }))
    .map(|(mut item, address, referer)| async move {
        item.format = if let Some(format) = path_format(&address).filter(image_format) {
            item.format.or(Some(format))
        } else {
            let response = client
                .get(address)
                .header(reqwest::header::RANGE, "bytes=0-0")
                .header(reqwest::header::REFERER, referer)
                .timeout(Duration::from_secs(3))
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?;
            response_format(&response).filter(image_format)
        };
        item.format.is_some().then_some(item)
    })
    .buffered(8)
    .filter_map(std::future::ready)
    .collect()
    .await
}

async fn append_verified_video_objects(
    client: &Client,
    source: &Url,
    declared: Vec<Media>,
    items: &mut Vec<Media>,
) {
    let mut seen = HashSet::new();
    let mut verified = stream::iter(
        declared
            .into_iter()
            .filter_map(|item| {
                let address = url(&item.url).ok()?;
                let known = items.iter().any(|existing| {
                    existing.kind == Kind::Video
                        && !existing.preview_only
                        && existing.qualities.iter().any(|quality| {
                            quality
                                .url
                                .as_deref()
                                .and_then(|value| url(value).ok())
                                .is_some_and(|variant| same_media_address(&address, &variant))
                        })
                });
                (!known && seen.insert(address.clone())).then_some((item, address))
            })
            .take(12)
            .map(|(mut item, address)| async move {
                if item.qualities.is_empty() {
                    let quality = probe_video_source(client, source, address, None).await?;
                    item.source_origin.clone_from(&quality.source_origin);
                    item.size = quality.size;
                    item.qualities.push(quality);
                }
                Some(item)
            }),
    )
    .buffered(4)
    .filter_map(std::future::ready)
    .collect::<Vec<_>>()
    .await;
    if verified.is_empty() {
        return;
    }
    let links: HashSet<_> = verified.iter().map(|item| item.url.as_str()).collect();
    let covers: HashSet<_> = verified
        .iter()
        .filter_map(|item| item.preview.as_deref())
        .collect();
    items.retain(|item| {
        !links.contains(item.url.as_str())
            && !(matches!(item.kind, Kind::Image | Kind::Gif) && covers.contains(item.url.as_str()))
    });
    verified.append(items);
    *items = verified;
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
    let response = client
        .get(address.clone())
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|_| "network")?;
    let mut response = checked_page(response).await?;
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
        let structured = structured_media(&page, &base, &mut links);
        let next = page.next.take();
        add_collection_videos(&mut items, &mut videos_seen, parse_media(page, &base));
        add_collection_videos(
            &mut items,
            &mut videos_seen,
            collection_video_objects(structured.videos),
        );
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
            let mut videos =
                collection_video_objects(structured_media(&page, &base, &mut Vec::new()).videos);
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

fn collection_video_objects(mut videos: Vec<Media>) -> Vec<Media> {
    videos.retain(|item| {
        url(&item.url)
            .is_ok_and(|address| path_format(&address).is_some_and(|(_, kind)| kind == Kind::Video))
    });
    videos
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

#[derive(Serialize)]
pub struct MediaInspection {
    pub extension: &'static str,
    pub size: Option<u64>,
}

pub async fn inspect(
    client: &Client,
    value: &str,
    source_origin: Option<&str>,
) -> AppResult<MediaInspection> {
    let target = url(value)?;
    let referer = source_origin
        .map(url)
        .transpose()?
        .and_then(|source| media_referer(&source, &target));
    let request = client.head(target);
    let request = match referer {
        Some(origin) => request.header(reqwest::header::REFERER, origin),
        None => request,
    };
    let response = request
        .send()
        .await
        .map_err(|_| "network")?
        .error_for_status()
        .map_err(|_| "httpError")?;
    let (extension, _) = response_format(&response).ok_or("unsupported")?;
    let size = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    Ok(MediaInspection { extension, size })
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

pub(crate) async fn read_limited(
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
