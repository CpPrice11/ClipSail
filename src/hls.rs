use std::{path::Path, process::Stdio, time::Duration};

use futures_util::{StreamExt, stream};
use reqwest::{
    Client, Url,
    header::{RANGE, REFERER},
};
use tokio::{fs::File, io::AsyncWriteExt};

use crate::{
    discovery::{self, AppResult},
    youtube,
};

pub fn is_playlist(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with(".m3u8")
}

fn relative(base: &Url, value: &str) -> AppResult<Url> {
    let address = base.join(value).map_err(|_| "unsupported")?;
    let address = discovery::url(address.as_str())?;
    if base.scheme() == "https" && address.scheme() != "https" {
        return Err("unsupported");
    }
    Ok(address)
}

enum Playlist {
    Variant {
        address: Url,
        dimensions: Option<(u32, u32)>,
    },
    Media(MediaPlaylist),
}

struct MediaPlaylist {
    initialization: Option<Url>,
    segments: Vec<Url>,
    seconds: Option<f64>,
}

fn parse(base: &Url, text: &str) -> AppResult<Playlist> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err("unsupported");
    }
    let mut variant = false;
    let mut dimensions = None;
    let mut complete = false;
    let mut duration = false;
    let mut seconds = Some(0.0);
    let mut addresses = Vec::new();
    let mut initialization = None;
    for line in lines {
        let key = line.split(':').next().unwrap_or(line);
        match key {
            "#EXT-X-STREAM-INF" if addresses.is_empty() && !variant && !duration => {
                variant = true;
                dimensions = line.split(',').find_map(|part| {
                    let attributes = part.split_once(':').map_or(part, |(_, rest)| rest);
                    let (width, height) = attributes
                        .trim()
                        .strip_prefix("RESOLUTION=")?
                        .split_once('x')?;
                    let (width, height) = (width.parse::<u32>().ok()?, height.parse::<u32>().ok()?);
                    (width > 0 && height > 0).then_some((width, height))
                });
            }
            "#EXT-X-MAP"
                if addresses.is_empty() && !variant && !complete && initialization.is_none() =>
            {
                let uri = line
                    .strip_prefix("#EXT-X-MAP:URI=\"")
                    .and_then(|value| value.strip_suffix('"'))
                    .filter(|value| !value.is_empty() && !value.contains('"'))
                    .ok_or("unsupported")?;
                initialization = Some(relative(base, uri)?);
            }
            "#EXT-X-ENDLIST" => complete = true,
            "#EXTINF" if !variant && !duration && !complete => {
                duration = true;
                let segment_seconds = line
                    .strip_prefix("#EXTINF:")
                    .and_then(|value| value.split(',').next())
                    .and_then(|value| value.trim().parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value >= 0.0);
                seconds = seconds
                    .zip(segment_seconds)
                    .map(|(total, value)| total + value);
            }
            "#EXT-X-TARGETDURATION"
            | "#EXT-X-ALLOW-CACHE"
            | "#EXT-X-PLAYLIST-TYPE"
            | "#EXT-X-VERSION"
            | "#EXT-X-MEDIA-SEQUENCE"
            | "#EXT-X-INDEPENDENT-SEGMENTS" => {}
            // Reject encryption, byte ranges and renditions with separate tracks.
            _ if line.starts_with('#') => return Err("unsupported"),
            _ if (variant || duration) && !complete && addresses.len() < 12_000 => {
                let address = relative(base, line)?;
                let valid = if variant {
                    initialization.is_none() && addresses.is_empty() && is_playlist(&address)
                } else if initialization.is_some() {
                    let path = address.path().to_ascii_lowercase();
                    path.ends_with(".m4s") || path.ends_with(".mp4")
                } else {
                    address.path().to_ascii_lowercase().ends_with(".ts")
                };
                if !valid {
                    return Err("unsupported");
                }
                addresses.push(address);
                duration = false;
            }
            _ => return Err("unsupported"),
        }
    }
    match (variant, complete, duration, addresses.len()) {
        (true, false, false, 1) => Ok(Playlist::Variant {
            address: addresses.remove(0),
            dimensions,
        }),
        (false, true, false, 1..) => Ok(Playlist::Media(MediaPlaylist {
            initialization,
            segments: addresses,
            seconds: seconds.filter(|value| value.is_finite() && *value > 0.0),
        })),
        _ => Err("unsupported"),
    }
}

async fn get(
    client: &Client,
    url: Url,
    referer: Option<&str>,
    timeout: Duration,
) -> AppResult<reqwest::Response> {
    let mut request = client.get(url).timeout(timeout);
    if let Some(value) = referer {
        request = request.header(REFERER, value);
    }
    let response = request.send().await.map_err(|_| "network")?;
    if response.status() == reqwest::StatusCode::GONE {
        return Err("changed");
    }
    let response = response.error_for_status().map_err(|_| "httpError")?;
    if response.status() != reqwest::StatusCode::OK {
        return Err("changed");
    }
    Ok(response)
}

async fn playlist(client: &Client, url: Url, referer: Option<&str>) -> AppResult<Playlist> {
    let mut response = get(client, url, referer, Duration::from_secs(15)).await?;
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
) -> AppResult<(MediaPlaylist, Option<(u32, u32)>)> {
    let (current, dimensions) = match playlist(client, url, referer).await? {
        Playlist::Variant {
            address,
            dimensions,
        } => (playlist(client, address, referer).await?, dimensions),
        complete => (complete, None),
    };
    match current {
        Playlist::Media(media) => Ok((media, dimensions)),
        Playlist::Variant { .. } => Err("unsupported"),
    }
}

pub async fn retain_available(client: &Client, item: &mut discovery::Media) -> bool {
    let choices = &mut item.qualities;
    if choices.is_empty() {
        return false;
    }
    let has_ffmpeg = youtube::ffmpeg().is_ok();
    let referer = item.source_origin.as_deref();
    let playlists: Vec<_> = choices
        .iter()
        .map(|quality| {
            quality
                .url
                .as_deref()
                .and_then(|value| discovery::url(value).ok())
                .filter(is_playlist)
        })
        .collect();
    let available = stream::iter(playlists.into_iter().map(|url| async move {
        match url {
            Some(url) if has_ffmpeg => media_playlist(client, url, referer)
                .await
                .map(|(media, dimensions)| (dimensions, media.seconds)),
            Some(_) => Err("youtubeTools"),
            None => Ok((None, None)),
        }
    }))
    .buffered(4)
    .collect::<Vec<_>>()
    .await;
    let changed = available.contains(&Err("changed"));
    let mut available = available.into_iter();
    choices.retain_mut(|quality| match available.next() {
        Some(Ok((dimensions, seconds))) => {
            item.duration = item.duration.or(seconds);
            if let Some((width, height)) = dimensions {
                quality.width = Some(width);
                quality.height = Some(height);
            }
            true
        }
        _ => false,
    });
    changed
}

pub async fn inspect_resolution(
    client: &Client,
    url: Url,
    referer: Option<&str>,
) -> AppResult<(u32, u32)> {
    const SAMPLE_LIMIT: usize = 512 * 1024;
    static PROBES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    if !is_playlist(&url) {
        return Err("unsupported");
    }
    let _permit = PROBES.try_acquire().map_err(|_| "unknown")?;
    let (media, dimensions) = media_playlist(client, url, referer).await?;
    if let Some(dimensions) = dimensions {
        return Ok(dimensions);
    }
    let fragmented = media.initialization.is_some();
    let segment = media
        .initialization
        .or_else(|| media.segments.into_iter().next())
        .ok_or("unsupported")?;
    let mut request = client
        .get(segment)
        .header(RANGE, format!("bytes=0-{}", SAMPLE_LIMIT - 1))
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

fn clear_mp4_bytes(mut data: &[u8], initialization: bool) -> bool {
    let (metadata, payload): (&[u8; 4], &[u8; 4]) = if initialization {
        (b"moov", b"ftyp")
    } else {
        (b"moof", b"mdat")
    };
    let mut found_metadata = false;
    let mut found_payload = false;
    while let Some(header) = data.first_chunk::<8>() {
        let mut size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        if size == 1 {
            let Some(extended) = data.get(8..16) else {
                return false;
            };
            let Ok(extended_size) =
                usize::try_from(u64::from_be_bytes(extended.try_into().unwrap()))
            else {
                return false;
            };
            if extended_size < 16 {
                return false;
            }
            size = extended_size;
        } else if size == 0 {
            size = data.len();
        }
        if size < 8 || size > data.len() {
            return false;
        }
        let kind = &header[4..];
        if kind == b"pssh" {
            return false;
        }
        if kind == metadata {
            if data[8..size]
                .windows(4)
                .any(|box_type| matches!(box_type, b"pssh" | b"sinf" | b"encv" | b"enca" | b"senc"))
            {
                return false;
            }
            found_metadata = true;
        }
        found_payload |= kind == payload;
        data = &data[size..];
    }
    data.is_empty() && found_metadata && found_payload
}

pub async fn save(
    client: &Client,
    url: Url,
    referer: Option<&str>,
    dir: &Path,
    progress: impl Fn(u64),
) -> AppResult<u64> {
    let ffmpeg = youtube::ffmpeg()?;
    let (
        MediaPlaylist {
            initialization,
            segments,
            ..
        },
        _,
    ) = media_playlist(client, url, referer).await?;
    let fragmented = initialization.is_some();
    let transport = dir.join(if fragmented {
        "fragments.mp4"
    } else {
        "stream.ts"
    });
    let mut target = File::create(&transport).await.map_err(|_| "disk")?;
    let mut bytes = 0;
    for (index, segment) in initialization.into_iter().chain(segments).enumerate() {
        let mut response = get(client, segment, referer, Duration::from_secs(60)).await?;
        let data = discovery::read_limited(&mut response, 32 * 1024 * 1024, "unsupported").await?;
        let valid = if fragmented {
            clear_mp4_bytes(&data, index == 0)
        } else {
            data.len() >= 188 && data[0] == 0x47 && data.len() % 188 == 0
        };
        if !valid {
            return Err("unsupported");
        }
        target.write_all(&data).await.map_err(|_| "disk")?;
        bytes += data.len() as u64;
        progress(bytes);
    }
    target.flush().await.map_err(|_| "disk")?;
    drop(target);
    let output = dir.join("stream.mp4");
    let mut command = tokio::process::Command::new(ffmpeg);
    command
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .args([
            "-nostdin",
            "-y",
            "-xerror",
            "-protocol_whitelist",
            "file",
            "-f",
            if fragmented { "mov" } else { "mpegts" },
        ]);
    if fragmented {
        command.args(["-enable_drefs", "0"]);
    }
    let status = command
        .arg("-i")
        .arg(&transport)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-c",
            "copy",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ])
        .arg(&output)
        .status()
        .await
        .map_err(|_| "mergeFailed")?;
    let bytes = tokio::fs::metadata(output)
        .await
        .map_err(|_| "mergeFailed")?
        .len();
    if !status.success() || bytes == 0 {
        return Err("mergeFailed");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_clear_complete_transport_streams() {
        let base = Url::parse("https://example.org/list/index.m3u8").unwrap();
        let clear =
            "#EXTM3U\n#EXTINF:6.25,\nseg.ts?signature=keep\n#EXTINF:1.25,\nnext.ts\n#EXT-X-ENDLIST";
        let Ok(Playlist::Media(MediaPlaylist {
            initialization: None,
            segments,
            seconds,
        })) = parse(&base, clear)
        else {
            panic!("clear VOD with one segment should work");
        };
        assert_eq!(
            segments[0].as_str(),
            "https://example.org/list/seg.ts?signature=keep"
        );
        assert_eq!(seconds, Some(7.5));
        let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=30000,RESOLUTION=1280x720\n720.m3u8";
        assert!(matches!(
            parse(&base, master),
            Ok(Playlist::Variant {
                dimensions: Some((1280, 720)),
                ..
            })
        ));
        for unsupported in [
            clear.replace("#EXTINF", "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF"),
            clear.replace("#EXT-X-ENDLIST", ""),
            clear.replace("seg.ts", "file:///temp/seg.ts"),
            clear.replace("seg.ts", "http://example.org/seg.ts"),
            master.to_owned() + "\nextra.m3u8",
        ] {
            assert!(parse(&base, &unsupported).is_err());
        }
    }

    #[test]
    fn pairs_fragments_with_initialization_and_rejects_encryption() {
        let base = Url::parse("https://example.org/list/index.m3u8").unwrap();
        let clear = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4?token=1\"\n#EXTINF:6,\nchunk.m4s?token=2\n#EXT-X-ENDLIST";
        let Ok(Playlist::Media(MediaPlaylist {
            initialization: Some(init),
            segments,
            ..
        })) = parse(&base, clear)
        else {
            panic!("clear fragmented MP4 should work");
        };
        assert_eq!(init.as_str(), "https://example.org/list/init.mp4?token=1");
        assert_eq!(
            segments[0].as_str(),
            "https://example.org/list/chunk.m4s?token=2"
        );
        assert!(parse(&base, &clear.replace("chunk.m4s", "chunk.ts")).is_err());
        assert!(
            parse(
                &base,
                &clear.replace("#EXTINF", "#EXT-X-KEY:METHOD=SAMPLE-AES\n#EXTINF")
            )
            .is_err()
        );
        assert!(
            parse(
                &base,
                &clear.replace("?token=1\"", "?token=1\",BYTERANGE=\"10@0\"")
            )
            .is_err()
        );
        assert!(clear_mp4_bytes(b"\0\0\0\x08ftyp\0\0\0\x08moov", true));
        assert!(!clear_mp4_bytes(b"\0\0\0\x08ftyp\0\0\0\x0cmoovencv", true));
        assert!(clear_mp4_bytes(b"\0\0\0\x08moof\0\0\0\x08mdat", false));
        assert!(!clear_mp4_bytes(b"\0\0\0\x0cmoofsenc\0\0\0\x08mdat", false));
    }
}
