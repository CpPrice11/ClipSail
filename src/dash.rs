use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

use quick_xml::{Reader, events::Event};
use reqwest::{Client, Url};

use crate::{
    discovery::{self, AppResult, Media, Quality},
    stream::{self, SEGMENT_LIMIT, Segment, Track},
    youtube,
};

const PART_LIMIT: usize = 12_000;
const URL_BYTES_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct Element {
    name: String,
    attributes: HashMap<String, String>,
    children: Vec<Element>,
    text: String,
}

impl Element {
    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Self> {
        self.children.iter().filter(move |child| child.name == name)
    }

    fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|child| child.name == name)
    }

    fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(String::as_str)
    }
}

fn parse(base: &Url, document: &str) -> AppResult<Manifest> {
    let mut reader = Reader::from_str(document);
    reader.config_mut().check_comments = true;
    reader.config_mut().expand_empty_elements = true;
    let mut stack = vec![Element::default()];
    let mut elements = 0;
    loop {
        match reader.read_event().map_err(|_| "unsupported")? {
            Event::Start(tag) if stack.len() < 24 && elements < 50_000 => {
                let mut element = Element {
                    name: std::str::from_utf8(tag.local_name().as_ref())
                        .map_err(|_| "unsupported")?
                        .to_owned(),
                    ..Element::default()
                };
                for attribute in tag.attributes() {
                    let attribute = attribute.map_err(|_| "unsupported")?;
                    let name =
                        std::str::from_utf8(attribute.key.as_ref()).map_err(|_| "unsupported")?;
                    let value = attribute.unescape_value().map_err(|_| "unsupported")?;
                    element
                        .attributes
                        .insert(name.to_owned(), value.into_owned());
                }
                stack.push(element);
                elements += 1;
            }
            Event::End(_) => {
                let element = stack.pop().ok_or("unsupported")?;
                stack
                    .last_mut()
                    .ok_or("unsupported")?
                    .children
                    .push(element);
            }
            Event::Text(text) => {
                let node = stack.last_mut().ok_or("unsupported")?;
                if node.name == "BaseURL" {
                    node.text
                        .push_str(&text.decode().map_err(|_| "unsupported")?);
                }
            }
            Event::CData(text) => {
                let node = stack.last_mut().ok_or("unsupported")?;
                if node.name == "BaseURL" {
                    node.text
                        .push_str(&text.decode().map_err(|_| "unsupported")?);
                }
            }
            Event::GeneralRef(reference) => {
                let node = stack.last_mut().ok_or("unsupported")?;
                if node.name == "BaseURL" {
                    let name = reference.decode().map_err(|_| "unsupported")?;
                    let escaped = format!("&{name};");
                    node.text.push_str(
                        &quick_xml::escape::unescape(&escaped).map_err(|_| "unsupported")?,
                    );
                }
            }
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) => {}
            Event::Eof if stack.len() == 1 => break,
            _ => return Err("unsupported"),
        }
    }

    let mut roots = stack.pop().ok_or("unsupported")?.children;
    if roots.len() != 1 {
        return Err("unsupported");
    }
    let root = roots.pop().ok_or("unsupported")?;
    if root.name != "MPD"
        || root
            .attribute("type")
            .is_some_and(|value| value != "static")
        || root.children("Period").count() != 1
    {
        return Err("unsupported");
    }
    Ok(Manifest {
        base: base.clone(),
        root,
    })
}

fn duration(text: &str) -> AppResult<f64> {
    let mut text = text.strip_prefix("PT").ok_or("unsupported")?;
    let mut total = 0.0;
    for (unit, scale) in [('H', 3600.0), ('M', 60.0), ('S', 1.0)] {
        if let Some((value, rest)) = text.split_once(unit) {
            let number: f64 = value.parse().map_err(|_| "unsupported")?;
            if !number.is_finite() || number < 0.0 {
                return Err("unsupported");
            }
            total += number * scale;
            text = rest;
        }
    }
    if text.is_empty() && total.is_finite() {
        Ok(total)
    } else {
        Err("unsupported")
    }
}

fn range(value: &str) -> AppResult<(u64, u64)> {
    let (first, last) = value.split_once('-').ok_or("unsupported")?;
    let (first, last): (u64, u64) = (
        first.parse().map_err(|_| "unsupported")?,
        last.parse().map_err(|_| "unsupported")?,
    );
    if last >= first && last - first < SEGMENT_LIMIT as u64 {
        Ok((first, last))
    } else {
        Err("unsupported")
    }
}

fn representation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

pub fn valid_format_id(value: &str) -> bool {
    value == "dash:default"
        || value
            .strip_prefix("dash:rep:")
            .is_some_and(representation_id)
}

pub fn is_stream(url: &Url, format_id: Option<&str>) -> bool {
    format_id.map_or_else(|| discovery::is_dash(url), valid_format_id)
}

pub fn is_stream_quality(quality: &Quality) -> bool {
    quality
        .url
        .as_deref()
        .and_then(|url| discovery::url(url).ok())
        .is_some_and(|url| is_stream(&url, quality.format_id.as_deref()))
}

struct Manifest {
    base: Url,
    root: Element,
}

struct Representation<'a> {
    id: &'a str,
    base: Url,
    levels: [&'a Element; 4],
    bandwidth: u64,
    dimensions: Option<(u32, u32)>,
    main: bool,
}

struct Offers<'a> {
    video: Vec<Representation<'a>>,
    audio: Vec<Representation<'a>>,
    seconds: Option<f64>,
    audio_declared: bool,
}

impl Manifest {
    fn offers(&self) -> AppResult<Offers<'_>> {
        let period = self.root.child("Period").ok_or("unsupported")?;
        if [&self.root, period]
            .iter()
            .any(|element| element.child("ContentProtection").is_some())
        {
            return Err("unsupported");
        }
        let period_start = period
            .attribute("start")
            .map(duration)
            .transpose()?
            .unwrap_or_default();
        let seconds = match period.attribute("duration") {
            Some(value) => Some(duration(value)?),
            None => self
                .root
                .attribute("mediaPresentationDuration")
                .map(duration)
                .transpose()?
                .map(|total| total - period_start),
        }
        .filter(|seconds| *seconds > 0.0);
        let mut offers = Offers {
            video: Vec::new(),
            audio: Vec::new(),
            seconds,
            audio_declared: false,
        };
        let mut video_ids = HashSet::new();
        for adaptation in period.children("AdaptationSet").take(32) {
            for node in adaptation.children("Representation").take(32) {
                let content_type = node
                    .attribute("contentType")
                    .or_else(|| adaptation.attribute("contentType"));
                let mime = node
                    .attribute("mimeType")
                    .or_else(|| adaptation.attribute("mimeType"));
                let audio = content_type == Some("audio")
                    || mime.is_some_and(|value| value.starts_with("audio/"));
                offers.audio_declared |= audio;
                let video = content_type == Some("video")
                    || mime.is_some_and(|value| value.starts_with("video/"));
                if (!audio && !video)
                    || (mime.is_some() && !matches!(mime, Some("video/mp4" | "audio/mp4")))
                    || adaptation.child("ContentProtection").is_some()
                    || node.child("ContentProtection").is_some()
                {
                    continue;
                }
                let levels = [&self.root, period, adaptation, node];
                let Some(id) = node
                    .attribute("id")
                    .filter(|value| representation_id(value))
                else {
                    continue;
                };
                let base = levels.iter().try_fold(self.base.clone(), |base, level| {
                    match level
                        .children("BaseURL")
                        .find(|node| !node.text.trim().is_empty())
                    {
                        Some(node) => stream::relative(&base, node.text.trim()),
                        None => Ok(base),
                    }
                });
                let Ok(base) = base else {
                    continue;
                };
                let dimension = |key| {
                    node.attribute(key)
                        .or_else(|| adaptation.attribute(key))
                        .and_then(|n| n.parse::<u32>().ok())
                        .filter(|n| *n > 0)
                };
                let entry = Representation {
                    id,
                    base,
                    levels,
                    bandwidth: node
                        .attribute("bandwidth")
                        .and_then(|n| n.parse().ok())
                        .unwrap_or_default(),
                    dimensions: dimension("width").zip(dimension("height")),
                    main: [adaptation, node].iter().any(|level| {
                        level
                            .children("Role")
                            .any(|role| role.attribute("value") == Some("main"))
                    }),
                };
                if entry.segment_nodes().is_none() {
                    continue;
                }
                if video
                    && entry.dimensions.is_some()
                    && video_ids.insert(id)
                    && offers.video.len() < 32
                {
                    offers.video.push(entry);
                } else if audio && offers.audio.len() < 32 {
                    offers.audio.push(entry);
                }
            }
        }
        offers.video.sort_by_key(|entry| {
            let (w, h) = entry.dimensions.unwrap_or_default();
            std::cmp::Reverse((w.min(h), entry.bandwidth))
        });
        offers
            .audio
            .sort_by_key(|entry| std::cmp::Reverse((entry.main, entry.bandwidth)));
        if offers.video.is_empty() || (offers.audio_declared && offers.audio.is_empty()) {
            return Err("unsupported");
        }
        Ok(offers)
    }
}

impl<'a> Representation<'a> {
    fn segment_nodes(&self) -> Option<(&str, Vec<&'a Element>)> {
        let kind = self.levels.iter().rev().find_map(|level| {
            ["SegmentTemplate", "SegmentList", "SegmentBase"]
                .into_iter()
                .find(|name| level.child(name).is_some())
        })?;
        let nodes = self
            .levels
            .iter()
            .rev()
            .filter_map(|level| level.child(kind))
            .collect();
        Some((kind, nodes))
    }

    fn expand(&self, pattern: &str, number: u64, time: u64) -> AppResult<String> {
        let mut rest = pattern;
        let mut expanded = String::new();
        while let Some((text, token)) = rest.split_once('$') {
            expanded.push_str(text);
            let (token, tail) = token.split_once('$').ok_or("unsupported")?;
            rest = tail;
            if token.is_empty() {
                expanded.push('$');
                continue;
            }
            let (key, width) = match token.split_once('%') {
                Some((key, width)) => {
                    let width = width
                        .strip_prefix('0')
                        .and_then(|width| width.strip_suffix('d'))
                        .and_then(|width| width.parse::<usize>().ok())
                        .filter(|width| *width <= 20)
                        .ok_or("unsupported")?;
                    (key, width)
                }
                None => (token, 0),
            };
            let value = match key {
                "RepresentationID" if width == 0 => {
                    expanded.push_str(self.id);
                    continue;
                }
                "Number" => number,
                "Time" => time,
                "Bandwidth" => self.bandwidth,
                _ => return Err("unsupported"),
            };
            expanded.push_str(&format!("{value:0width$}"));
        }
        expanded.push_str(rest);
        Ok(expanded)
    }

    fn segment(&self, url: Option<&str>, byte_range: Option<&str>) -> AppResult<Segment> {
        let url = match url {
            Some(url) => stream::relative(&self.base, url)?,
            None => self.base.clone(),
        };
        if url.as_str().len() > 2048 {
            return Err("unsupported");
        }
        Ok(Segment {
            url,
            range: byte_range.map(range).transpose()?,
        })
    }

    fn plan(&self, seconds: Option<f64>) -> AppResult<Plan> {
        let (kind, nodes) = self.segment_nodes().ok_or("unsupported")?;
        let attribute = |name| nodes.iter().find_map(|node| node.attribute(name));
        let child = |name| nodes.iter().find_map(|node| node.child(name));
        let (initialization, segments) = match kind {
            "SegmentTemplate" => {
                let init = attribute("initialization").ok_or("unsupported")?;
                let media = attribute("media").ok_or("unsupported")?;
                if init.is_empty() || media.is_empty() || init.len() > 2048 || media.len() > 2048 {
                    return Err("unsupported");
                }
                let number = number_value(attribute("startNumber"), 1)?;
                let init = self.segment(Some(&self.expand(init, number, 0)?), None)?;
                let timescale = number_value(attribute("timescale"), 1)?;
                let offset = number_value(attribute("presentationTimeOffset"), 0)?;
                if timescale == 0 {
                    return Err("unsupported");
                }
                let end = seconds
                    .and_then(|seconds| duration_ticks(seconds, timescale))
                    .and_then(|ticks| ticks.checked_add(offset));
                let times = match child("SegmentTimeline") {
                    Some(timeline) => timeline_times(timeline, end)?,
                    None => {
                        let spacing = number_value(attribute("duration"), 0)?;
                        let end = end.ok_or("unsupported")?;
                        if spacing == 0 || end == 0 || end.div_ceil(spacing) > PART_LIMIT as u64 {
                            return Err("unsupported");
                        }
                        (0..end.div_ceil(spacing))
                            .map(|index| index * spacing)
                            .collect()
                    }
                };
                let segments = times
                    .into_iter()
                    .enumerate()
                    .map(|(index, time)| {
                        let index = number.checked_add(index as u64).ok_or("unsupported")?;
                        self.segment(Some(&self.expand(media, index, time)?), None)
                    })
                    .collect::<AppResult<Vec<_>>>()?;
                (init, segments)
            }
            "SegmentList" => {
                let init = child("Initialization").ok_or("unsupported")?;
                let init = self.segment(init.attribute("sourceURL"), init.attribute("range"))?;
                let list = nodes
                    .iter()
                    .find(|node| node.child("SegmentURL").is_some())
                    .ok_or("unsupported")?;
                let segments = list
                    .children("SegmentURL")
                    .take(PART_LIMIT + 1)
                    .map(|item| {
                        if item.attribute("media").is_none()
                            && item.attribute("mediaRange").is_none()
                        {
                            return Err("unsupported");
                        }
                        self.segment(item.attribute("media"), item.attribute("mediaRange"))
                    })
                    .collect::<AppResult<Vec<_>>>()?;
                (init, segments)
            }
            "SegmentBase" => {
                let init = child("Initialization").ok_or("unsupported")?;
                let byte_range = init.attribute("range").ok_or("unsupported")?;
                let index =
                    self.segment(None, Some(attribute("indexRange").ok_or("unsupported")?))?;
                let init = self.segment(init.attribute("sourceURL"), Some(byte_range))?;
                return Ok(Plan::Indexed { init, index });
            }
            _ => return Err("unsupported"),
        };
        if segments.is_empty()
            || segments.len() > PART_LIMIT
            || segments
                .iter()
                .map(|segment| segment.url.as_str().len())
                .sum::<usize>()
                > URL_BYTES_LIMIT
        {
            return Err("unsupported");
        }
        Ok(Plan::Ready(Track {
            initialization: Some(initialization),
            segments,
            sequence: 0,
            seconds,
        }))
    }
}

fn number_value(value: Option<&str>, fallback: u64) -> AppResult<u64> {
    value.map_or(Ok(fallback), |value| {
        value.parse().map_err(|_| "unsupported")
    })
}

fn duration_ticks(seconds: f64, timescale: u64) -> Option<u64> {
    let ticks = seconds * timescale as f64;
    (timescale > 0 && ticks > 0.0 && ticks.is_finite() && ticks < u64::MAX as f64)
        .then(|| ticks.ceil() as u64)
}

fn timeline_times(timeline: &Element, end: Option<u64>) -> AppResult<Vec<u64>> {
    let mut times = Vec::new();
    let mut next_time = 0_u64;
    let mut entries = timeline.children("S").peekable();
    while let Some(entry) = entries.next() {
        let time = number_value(entry.attribute("t"), next_time)?;
        let spacing = number_value(entry.attribute("d"), 0)?;
        if spacing == 0 || (!times.is_empty() && time < next_time) {
            return Err("unsupported");
        }
        let repeat = entry
            .attribute("r")
            .unwrap_or("0")
            .parse::<i64>()
            .map_err(|_| "unsupported")?;
        let count = match repeat {
            -1 => {
                let limit = match entries.peek() {
                    Some(next) => number_value(next.attribute("t"), time)?,
                    None => end.ok_or("unsupported")?,
                };
                let remaining = limit
                    .checked_sub(time)
                    .filter(|remaining| *remaining > 0)
                    .ok_or("unsupported")?;
                remaining.div_ceil(spacing)
            }
            0.. => repeat as u64 + 1,
            _ => return Err("unsupported"),
        };
        if count > (PART_LIMIT - times.len()) as u64 {
            return Err("unsupported");
        }
        next_time = time
            .checked_add(count.checked_mul(spacing).ok_or("unsupported")?)
            .ok_or("unsupported")?;
        times.extend((0..count).map(|index| time + spacing * index));
    }
    Ok(times)
}

enum Plan {
    Ready(Track),
    Indexed { init: Segment, index: Segment },
}

fn index_parts(bytes: &[u8], index_start: u64, url: &Url) -> AppResult<Vec<Segment>> {
    let u32_at = |slice: &[u8], offset| {
        slice
            .get(offset..)?
            .first_chunk::<4>()
            .map(|value| u32::from_be_bytes(*value))
    };
    let mut cursor = 0;
    while let Some(header) = bytes
        .get(cursor..)
        .and_then(|bytes| bytes.first_chunk::<8>())
    {
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        if length < 8 {
            return Err("unsupported");
        }
        let end = cursor.checked_add(length).ok_or("unsupported")?;
        let entry = bytes.get(cursor..end).ok_or("unsupported")?;
        if &header[4..] != b"sidx" {
            cursor = end;
            continue;
        }
        let (offset, counter) = match entry.get(8) {
            Some(0) => (u32_at(entry, 24).map(u64::from), 30),
            Some(1) => (
                entry
                    .get(28..36)
                    .and_then(|bytes| bytes.first_chunk::<8>())
                    .map(|bytes| u64::from_be_bytes(*bytes)),
                38,
            ),
            _ => return Err("unsupported"),
        };
        let offset = offset.ok_or("unsupported")?;
        let count = entry
            .get(counter..counter + 2)
            .and_then(|bytes| bytes.first_chunk::<2>())
            .map(|bytes| u16::from_be_bytes(*bytes) as usize)
            .ok_or("unsupported")?;
        if count == 0 || count > PART_LIMIT || counter + 2 + count * 12 > entry.len() {
            return Err("unsupported");
        }
        let mut start = index_start
            .checked_add(end as u64)
            .and_then(|end| end.checked_add(offset))
            .ok_or("unsupported")?;
        return (0..count)
            .map(|index| {
                let size = u32_at(entry, counter + 2 + index * 12).ok_or("unsupported")?;
                if size == 0 || size as usize > SEGMENT_LIMIT {
                    return Err("unsupported");
                }
                let end = start.checked_add(u64::from(size)).ok_or("unsupported")?;
                let part = Segment {
                    url: url.clone(),
                    range: Some((start, end - 1)),
                };
                start = end;
                Ok(part)
            })
            .collect();
    }
    Err("unsupported")
}

impl Plan {
    async fn resolve(
        self,
        client: &Client,
        referer: Option<&str>,
        seconds: Option<f64>,
    ) -> AppResult<Track> {
        let (init, index) = match self {
            Self::Ready(track) => return Ok(track),
            Self::Indexed { init, index } => (init, index),
        };
        let (start, end) = index.range.ok_or("unsupported")?;
        let index_len = end - start + 1;
        if index_len > 1024 * 1024 {
            return Err("unsupported");
        }
        let mut response = stream::get(
            client,
            index.url.clone(),
            referer,
            Duration::from_secs(15),
            index.range,
        )
        .await?;
        let bytes =
            discovery::read_limited(&mut response, index_len as usize, "unsupported").await?;
        if bytes.len() as u64 != index_len {
            return Err("network");
        }
        let segments = index_parts(&bytes, start, &index.url)?;
        Ok(Track {
            initialization: Some(init),
            segments,
            sequence: 0,
            seconds,
        })
    }
}

impl Offers<'_> {
    fn audio_plan(&self) -> AppResult<Option<Plan>> {
        let audio = self
            .audio
            .iter()
            .find_map(|entry| entry.plan(self.seconds).ok());
        if self.audio_declared && audio.is_none() {
            return Err("unsupported");
        }
        Ok(audio)
    }

    fn choice(&self, format_id: Option<&str>) -> AppResult<(&Representation<'_>, Plan)> {
        let selected = format_id.and_then(|value| value.strip_prefix("dash:rep:"));
        self.video
            .iter()
            .filter(|choice| selected.is_none_or(|id| id == choice.id))
            .find_map(|choice| choice.plan(self.seconds).ok().map(|plan| (choice, plan)))
            .ok_or("qualityUnavailable")
    }
}

async fn fetch(client: &Client, url: Url, referer: Option<&str>) -> AppResult<Manifest> {
    let mut response = stream::get(client, url, referer, Duration::from_secs(15), None).await?;
    let base = response.url().clone();
    let bytes = discovery::read_limited(&mut response, 2 * 1024 * 1024, "unsupported").await?;
    parse(
        &base,
        std::str::from_utf8(&bytes).map_err(|_| "unsupported")?,
    )
}

pub async fn retain_available(client: &Client, item: &mut Media) -> bool {
    if !item.qualities.iter().any(is_stream_quality) {
        return false;
    }
    let has_ffmpeg = youtube::ffmpeg().is_ok();
    let mut changed = false;
    let mut choices = Vec::new();
    for quality in std::mem::take(&mut item.qualities) {
        if !is_stream_quality(&quality) {
            choices.push(quality);
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
        match fetch(client, url, item.source_origin.as_deref()).await {
            Ok(manifest) => {
                let Ok(offers) = manifest.offers() else {
                    continue;
                };
                if offers.audio_plan().is_err() {
                    continue;
                }
                let mut count = 0;
                for entry in offers.video.iter() {
                    if entry.plan(offers.seconds).is_err() {
                        continue;
                    }
                    let Some((width, height)) = entry.dimensions else {
                        continue;
                    };
                    choices.push(Quality {
                        url: quality.url.clone(),
                        source_origin: quality.source_origin.clone(),
                        format_id: Some(format!("dash:rep:{}", entry.id)),
                        extension: "mp4".to_owned(),
                        width: Some(width),
                        height: Some(height),
                        size: None,
                    });
                    item.duration = item.duration.or(offers.seconds);
                    count += 1;
                    if count >= 16 {
                        break;
                    }
                }
            }
            Err("changed") => changed = true,
            Err(_) => {}
        }
    }
    choices.sort_by_key(|entry| std::cmp::Reverse(entry.resolution()));
    let mut seen = HashSet::new();
    choices.retain(|entry| seen.insert((entry.url.clone(), entry.format_id.clone())));
    choices.truncate(16);
    item.qualities = choices;
    changed
}

pub async fn inspect_resolution(
    client: &Client,
    url: Url,
    format_id: Option<&str>,
    referer: Option<&str>,
) -> AppResult<(u32, u32)> {
    let manifest = fetch(client, url, referer).await?;
    let offers = manifest.offers()?;
    let (choice, _) = offers.choice(format_id)?;
    choice.dimensions.ok_or("unknown")
}

pub async fn save(
    client: &Client,
    url: Url,
    format_id: Option<&str>,
    referer: Option<&str>,
    dir: &Path,
    progress: impl Fn(u64),
) -> AppResult<u64> {
    let manifest = fetch(client, url, referer).await?;
    let offers = manifest.offers()?;
    let (_, video) = offers.choice(format_id)?;
    let audio = offers.audio_plan()?;
    let (video, audio) = tokio::try_join!(video.resolve(client, referer, offers.seconds), async {
        match audio {
            Some(audio) => audio
                .resolve(client, referer, offers.seconds)
                .await
                .map(Some),
            None => Ok(None),
        }
    })?;
    stream::save(
        client,
        &video,
        audio.as_ref(),
        referer,
        &dir.join("dash"),
        progress,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<MPD type="static" mediaPresentationDuration="PT6S"
      xmlns="urn:mpeg:dash:schema:mpd:2011">
      <BaseURL>catalog/</BaseURL>
      <Period><AdaptationSet contentType="video" mimeType="video/mp4">
        <SegmentTemplate timescale="1000" startNumber="7" initialization="$RepresentationID$/init.mp4"
          media="$RepresentationID$/$Number%04d$-$Time$.m4s">
          <SegmentTimeline><S t="0" d="2000" r="-1"/></SegmentTimeline>
        </SegmentTemplate>
        <Representation id="low" width="640" height="360" bandwidth="300000"/>
        <Representation id="high" width="1280" height="720" bandwidth="1500000"/>
      </AdaptationSet>
      <AdaptationSet contentType="audio" mimeType="audio/mp4"><Role value="main"/>
        <Representation id="audio" bandwidth="128000">
          <SegmentList><Initialization sourceURL="audio-init.mp4"/>
            <SegmentURL media="audio.m4s" mediaRange="0-1023"/>
          </SegmentList>
        </Representation>
      </AdaptationSet></Period>
    </MPD>"#;

    #[test]
    fn resolves_selected_dimensions_templates_and_audio_ranges() {
        let base = Url::parse("https://media.example/video.mpd").unwrap();
        let manifest = parse(&base, SAMPLE).ok().unwrap();
        let offers = manifest.offers().ok().unwrap();
        assert_eq!(offers.video[0].id, "high");
        let (low, plan) = offers.choice(Some("dash:rep:low")).ok().unwrap();
        assert_eq!(low.dimensions, Some((640, 360)));
        let Plan::Ready(track) = plan else {
            panic!("unexpected plan");
        };
        assert_eq!(
            track.initialization.unwrap().url.path(),
            "/catalog/low/init.mp4"
        );
        assert_eq!(track.segments.len(), 3);
        assert_eq!(track.segments[2].url.path(), "/catalog/low/0009-4000.m4s");
        let Some(Plan::Ready(audio)) = offers.audio_plan().ok().flatten() else {
            panic!("missing audio");
        };
        assert_eq!(audio.segments[0].range, Some((0, 1023)));
        assert!(offers.choice(Some("dash:rep:other")).is_err());
    }

    #[test]
    fn rejects_live_and_protected_playlists_and_unsafe_base_urls() {
        let base = Url::parse("https://media.example/video.mpd").unwrap();
        assert!(parse(&base, &SAMPLE.replace("static", "dynamic")).is_err());
        assert!(
            parse(
                &base,
                &format!("<!DOCTYPE MPD [<!ENTITY private SYSTEM 'file:///private'>]>{SAMPLE}")
            )
            .is_err()
        );
        let protected = SAMPLE.replace("<Period>", "<Period><ContentProtection/>");
        assert!(parse(&base, &protected).ok().unwrap().offers().is_err());
        let local = SAMPLE.replace("catalog/", "file:///private/");
        assert!(parse(&base, &local).ok().unwrap().offers().is_err());
        let runaway = SAMPLE.replace("r=\"-1\"", "r=\"10000000\"");
        assert!(
            parse(&base, &runaway)
                .ok()
                .unwrap()
                .offers()
                .ok()
                .unwrap()
                .choice(None)
                .is_err()
        );
    }

    #[test]
    fn resolves_sidx_ranges_and_rejects_nested_references() {
        let url = Url::parse("https://media.example/video.mp4").unwrap();
        let mut index = vec![0; 56];
        index[..4].copy_from_slice(&56_u32.to_be_bytes());
        index[4..8].copy_from_slice(b"sidx");
        index[24..28].copy_from_slice(&4_u32.to_be_bytes());
        index[30..32].copy_from_slice(&2_u16.to_be_bytes());
        index[32..36].copy_from_slice(&20_u32.to_be_bytes());
        index[44..48].copy_from_slice(&30_u32.to_be_bytes());
        let parts = index_parts(&index, 100, &url).unwrap();
        assert_eq!(parts[0].range, Some((160, 179)));
        assert_eq!(parts[1].range, Some((180, 209)));
        index[32] = 128;
        assert!(index_parts(&index, 100, &url).is_err());
    }
}
