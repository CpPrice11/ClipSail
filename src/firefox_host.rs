#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod firefox;
mod paths;
// The native host only uses the YouTube format lookup from these shared modules.
#[allow(dead_code)]
mod discovery;
#[allow(dead_code)]
mod youtube;
mod yt_dlp;

use std::{
    fs,
    io::{self, Read, Write},
};

use firefox::{
    ADDON_ID, BrowserStatus, CHROME_ORIGIN, MAX_BYTES, Report, VideoRequest, is_youtube_video,
};
use serde::Deserialize;

const MAX_JOBS: usize = 100;
const CREATE_NO_WINDOW: u32 = 0x08000000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityRequest {
    youtube_qualities: String,
}

fn video_qualities(request: QualityRequest) -> discovery::AppResult<Vec<discovery::Quality>> {
    let url = reqwest::Url::parse(&request.youtube_qualities).map_err(|_| "sourceChanged")?;
    if !is_youtube_video(&url)
        || url.as_str() != request.youtube_qualities
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("sourceChanged");
    }
    let id = youtube::video_id(&url).ok_or("sourceChanged")?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "youtubeUnavailable")?
        .block_on(youtube::qualities(&id))
}

fn read_payload(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut length = [0; 4];
    input.read_exact(&mut length).ok()?;
    let size = u32::from_le_bytes(length) as usize;
    if !(1..=MAX_BYTES).contains(&size) {
        return None;
    }
    let mut payload = vec![0; size];
    input.read_exact(&mut payload).ok()?;
    Some(payload)
}

fn parse_report(payload: &[u8]) -> Option<Report> {
    let report: Report = serde_json::from_slice(payload).ok()?;
    if report.jobs.len() > MAX_JOBS
        || report.jobs.iter().any(|job| {
            job.name.trim().is_empty()
                || job.name.chars().count() > 180
                || job
                    .name
                    .chars()
                    .any(|c| c.is_control() || c == '/' || c == '\\')
                || job.status == BrowserStatus::FirefoxOffline
        })
    {
        return None;
    }
    Some(report)
}

fn save(report: &Report) -> Option<()> {
    let dir = firefox::directory()?;
    fs::create_dir_all(&dir).ok()?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir).ok()?;
    serde_json::to_writer(temp.as_file_mut(), report).ok()?;
    temp.persist(dir.join(format!("{}.json", report.profile)))
        .ok()?;
    Some(())
}

fn accept_video(video: &VideoRequest) -> Option<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::process::CommandExt;
    video.validate()?;
    let dir = firefox::incoming_directory()?;
    let app_data = dir.parent()?.parent()?;
    let folder = paths::app_folder()?;
    let app = ["LoadSail.exe", "ClipSail.exe", "media-downloader.exe"]
        .into_iter()
        .map(|name| folder.join(name))
        .find(|path| path.is_file())?;
    fs::create_dir_all(&dir).ok()?;
    // Serialize submissions while checking the pending limit and starting the app.
    let _submission = (0..20).find_map(|_| {
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(dir.join("submit.lock"))
            .ok();
        if lock.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        lock
    })?;
    if fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .take(32)
        .count()
        >= 32
    {
        return None;
    }
    let mut temp = tempfile::NamedTempFile::new_in(&dir).ok()?;
    serde_json::to_writer(temp.as_file_mut(), video).ok()?;
    let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    temp.persist_noclobber(&path).ok()?;

    let queue_lock = app_data.join("queue.lock");
    let running = || {
        fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(&queue_lock)
            .is_err_and(|error| error.raw_os_error() == Some(32))
    };
    if running() {
        return Some(());
    }
    // Firefox terminates the native host's job when the host exits.
    let child = std::process::Command::new(&app)
        .current_dir(app.parent()?)
        .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
        .arg("--browser-request")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            if !path.exists() || running() {
                return Some(());
            }
            match child.try_wait() {
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
                Ok(None) => return Some(()),
                _ => break,
            }
        }
    }
    let _ = fs::remove_file(path);
    None
}

fn authorized_browser(first: Option<&std::ffi::OsStr>, second: Option<&std::ffi::OsStr>) -> bool {
    first == Some(std::ffi::OsStr::new(CHROME_ORIGIN))
        || second == Some(std::ffi::OsStr::new(ADDON_ID))
}

fn main() -> io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    if !authorized_browser(args.next().as_deref(), args.next().as_deref()) {
        return Ok(());
    }
    let response = match read_payload(&mut io::stdin().lock()) {
        Some(payload) => {
            if let Ok(request) = serde_json::from_slice::<QualityRequest>(&payload) {
                match video_qualities(request) {
                    Ok(qualities) => serde_json::json!({ "ok": true, "qualities": qualities }),
                    Err(error) => serde_json::json!({ "ok": false, "error": error }),
                }
            } else {
                let accepted = if let Ok(video) = serde_json::from_slice::<VideoRequest>(&payload) {
                    accept_video(&video).is_some()
                } else {
                    parse_report(&payload)
                        .and_then(|report| save(&report))
                        .is_some()
                };
                serde_json::json!({ "ok": accepted })
            }
        }
        None => serde_json::json!({ "ok": false }),
    };
    let response = serde_json::to_vec(&response)?;
    let mut output = io::stdout().lock();
    output.write_all(&(response.len() as u32).to_le_bytes())?;
    output.write_all(&response)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_only_the_expected_chrome_and_firefox_extensions() {
        use std::ffi::OsStr;
        assert!(authorized_browser(Some(OsStr::new(CHROME_ORIGIN)), None));
        assert!(authorized_browser(
            Some(OsStr::new("host.json")),
            Some(OsStr::new(ADDON_ID))
        ));
        assert!(!authorized_browser(
            Some(OsStr::new(
                "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"
            )),
            None
        ));
        assert!(!authorized_browser(None, None));
    }

    fn read_report(input: &mut impl Read) -> Option<Report> {
        parse_report(&read_payload(input)?)
    }

    #[test]
    fn accepts_only_direct_video_and_origin() {
        let mut video = VideoRequest {
            url: "https://cdn.example.com/clip.MP4?sig=test".to_owned(),
            origin: "https://example.com".to_owned(),
            title: "Clip".to_owned(),
            quality: 1080,
            auto_start: false,
            format_id: None,
        };
        assert!(video.validate().is_some());
        video.origin.push_str("/watch?q=private");
        assert!(video.validate().is_none());
        video.origin = "https://example.com".to_owned();
        video.url = "file:///C:/clip.mp4".to_owned();
        assert!(video.validate().is_none());
        video.url = "https://example.com/stream.m3u8".to_owned();
        assert!(video.validate().is_none());
        video.url = "https://www.youtube.com/watch?v=aqz-KE-bpKQ".to_owned();
        assert!(video.validate().is_none());
        video.origin = "https://www.youtube.com".to_owned();
        assert!(video.validate().is_some());
        video.auto_start = true;
        assert!(video.validate().is_some());
        video.format_id = Some("18".to_owned());
        assert!(video.validate().is_some());
        video.auto_start = false;
        assert!(video.validate().is_none());
        video.auto_start = true;
        video.format_id = Some("18+140+22".to_owned());
        assert!(video.validate().is_none());
        video.format_id = Some("18".to_owned());
        video.url = "https://example.com/video.mp4".to_owned();
        assert!(video.validate().is_none());
        video.format_id = None;
        video.url = "https://example.com/photo.jpg".to_owned();
        assert!(video.validate().is_some());
        video.url = "https://example.com/audio.mp3".to_owned();
        assert!(video.validate().is_some());
        video.auto_start = false;
        assert!(video.validate().is_none());

        for url in [
            "https://example.com/watch?v=aqz-KE-bpKQ",
            "https://abc@www.youtube.com/watch?v=aqz-KE-bpKQ",
            "https://www.youtube.com/watch?v=aqz-KE-bpKQ#other",
        ] {
            assert_eq!(
                video_qualities(QualityRequest {
                    youtube_qualities: url.to_owned()
                })
                .unwrap_err(),
                "sourceChanged"
            );
        }
    }

    #[test]
    fn accepts_only_bounded_download_metadata() {
        let valid = r#"{"profile":"f2a7ff8a-534a-4598-a430-899ed2b38fb2","jobs":[{"id":4,"name":"video.mp4","status":"running","done":1,"total":2}]}"#;
        fn packet(value: &str) -> Vec<u8> {
            let mut bytes = (value.len() as u32).to_le_bytes().to_vec();
            bytes.extend_from_slice(value.as_bytes());
            bytes
        }
        assert!(read_report(&mut packet(valid).as_slice()).is_some());
        assert!(
            read_report(&mut packet(&valid.replace("video.mp4", "../video.mp4")).as_slice())
                .is_none()
        );
        assert!(
            read_report(
                &mut packet(&valid.replace("\"id\":4", "\"id\":4,\"url\":\"https://example.org\""))
                    .as_slice()
            )
            .is_none()
        );
        assert!(read_report(&mut ((MAX_BYTES + 1) as u32).to_le_bytes().as_slice()).is_none());
    }
}
