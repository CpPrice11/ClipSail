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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf8_and_quoted_attachment_filenames() {
        assert_eq!(
            disposition(
                "attachment; filename=\"fallback.zip\"; filename*=UTF-8'uk'%D0%A2%D0%B5%D1%81%D1%82.zip"
            ),
            (true, Some("Тест.zip".to_owned()))
        );
        assert_eq!(
            disposition("inline; filename=\"report; copy.pdf\""),
            (false, Some("report; copy.pdf".to_owned()))
        );
    }

    #[test]
    fn keeps_names_inside_the_download_folder() {
        assert_eq!(safe_name("../outside.zip"), "_outside.zip");
        assert_eq!(safe_name("NUL.pdf"), "download.pdf");
        assert_eq!(safe_name("Report:2026.pdf"), "Report_2026.pdf");
        assert_eq!(safe_name("readme"), "readme");
        assert_eq!(safe_name("print.prn"), "print.prn");
        assert_eq!(safe_name(".."), "download");
    }
}
