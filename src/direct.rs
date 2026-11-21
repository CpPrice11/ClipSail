use percent_encoding::percent_decode_str;
use reqwest::{Response, header::CONTENT_DISPOSITION};

use crate::discovery::{self, AppResult};

fn disposition(header: &str) -> (bool, Option<String>) {
    let mut quoted = false;
    let mut escaped = false;
    let mut fields = header.split(|ch| {
        if escaped {
            escaped = false;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == '\\' && quoted {
            escaped = true;
        } else if ch == ';' && !quoted {
            return true;
        }
        false
    });
    let attachment = fields
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("attachment"));
    let mut plain = None;
    let mut extended = None;
    for (key, value) in fields.filter_map(|field| field.split_once('=')) {
        let value = value.trim();
        if key.trim().eq_ignore_ascii_case("filename*") {
            if let Some((encoding, value)) = value.split_once('\'')
                && encoding.eq_ignore_ascii_case("utf-8")
                && let Some((_, encoded)) = value.split_once('\'')
            {
                extended = percent_decode_str(encoded)
                    .decode_utf8()
                    .ok()
                    .map(|v| v.into_owned());
            }
        } else if key.trim().eq_ignore_ascii_case("filename") {
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value);
            plain = Some(value.to_owned());
        }
    }
    (
        attachment,
        extended.filter(|name| !name.is_empty()).or(plain),
    )
}

fn safe_name(name: &str) -> String {
    let name = name.trim_matches([' ', '.']);
    let (stem, suffix) = name
        .rsplit_once('.')
        .map_or((name, None), |(stem, suffix)| (stem, Some(suffix)));
    let stem = discovery::safe_file_stem(stem).unwrap_or_else(|| "download".to_owned());
    if let Some(suffix) = suffix {
        let extension = discovery::clean_file_component(suffix, 24);
        let extension = extension.trim_matches([' ', '.']);
        if !extension.is_empty() {
            return format!("{stem}.{extension}");
        }
    }
    stem
}

pub fn name(response: &Response) -> AppResult<String> {
    let (attachment, filename) = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|header| std::str::from_utf8(header.as_bytes()).ok())
        .map(disposition)
        .unwrap_or_default();
    let last = response.url().path().rsplit('/').next().unwrap_or_default();
    let path = percent_decode_str(last).decode_utf8_lossy();
    let mime = discovery::mime(response.headers());
    if (mime.eq_ignore_ascii_case("text/html")
        || mime.eq_ignore_ascii_case("application/xhtml+xml"))
        && !attachment
        && !path.rsplit_once('.').is_some_and(|(_, ext)| {
            matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm" | "xhtml")
        })
    {
        return Err("directPage");
    }
    Ok(safe_name(
        filename
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&path),
    ))
}
