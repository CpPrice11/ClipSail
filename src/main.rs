#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dash;
mod direct;
mod discovery;
mod downloads;
mod extractor;
mod firefox;
mod hls;
mod paths;
mod player_capture;
mod playback;
mod saved_file;
mod stream;
mod twitter;
mod updater;
mod youtube;
mod yt_dlp;

use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use discovery::{AppResult, Media};
use downloads::{Job, Selection, Status, Update};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};
use tokio::sync::watch;
use uuid::Uuid;

const MAX_ACTIVE: usize = 2;

struct Inner {
    jobs: Vec<Job>,
    active: HashMap<Uuid, watch::Sender<bool>>,
    folder: PathBuf,
    error: Option<&'static str>,
    updating: bool,
}

struct App {
    client: reqwest::Client,
    state_file: PathBuf,
    portable_root: Option<PathBuf>,
    inner: Mutex<Inner>,
    _instance: fs::File,
}

impl App {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("download queue lock")
    }

    fn save(&self, inner: &Inner) -> AppResult<()> {
        let Some(root) = self.portable_root.as_deref() else {
            return downloads::write_json(&self.state_file, &inner.jobs);
        };
        let mut jobs = inner.jobs.clone();
        for job in &mut jobs {
            job.folder = stored_folder(&job.folder, root).to_path_buf();
        }
        downloads::write_json(&self.state_file, &jobs)
    }

    fn pump(self: &Arc<Self>) {
        let mut inner = self.lock();
        if inner.updating {
            return;
        }
        while inner.active.len() < MAX_ACTIVE {
            let Some(index) = inner.jobs.iter().position(|j| j.status == Status::Queued) else {
                break;
            };
            let (stop, receiver) = watch::channel(false);
            inner.jobs[index].status = Status::Running;
            let job = inner.jobs[index].clone();
            inner.active.insert(job.id, stop);
            let app = Arc::clone(self);
            tauri::async_runtime::spawn(async move {
                let result = downloads::run(&app.client, &job, receiver, |update| {
                    let mut inner = app.lock();
                    let entry = inner
                        .jobs
                        .iter_mut()
                        .find(|j| j.id == job.id)
                        .expect("active job");
                    match update {
                        Update::Progress(done, total) => {
                            entry.done = done;
                            entry.total = total;
                        }
                        Update::Completed(file, size) => {
                            entry.file = Some(file);
                            entry.done = size;
                            entry.total = Some(size);
                            entry.status = Status::Completed;
                            entry.error = None;
                            if let Err(error) = app.save(&inner) {
                                inner.error = Some(error);
                            }
                        }
                    }
                })
                .await;

                let mut inner = app.lock();
                let entry = inner
                    .jobs
                    .iter_mut()
                    .find(|j| j.id == job.id)
                    .expect("active job");
                if let Err(error) = result {
                    let pausing = entry.status == Status::Pausing || error == "paused";
                    entry.status = if pausing {
                        Status::Paused
                    } else {
                        Status::Failed
                    };
                    entry.error = (!pausing).then(|| error.to_owned());
                }
                inner.active.remove(&job.id);
                if let Err(error) = app.save(&inner) {
                    inner.error = Some(error);
                }
                drop(inner);
                app.pump();
            });
        }
    }
}

#[derive(Serialize)]
struct Snapshot {
    folder: PathBuf,
    jobs: Vec<Job>,
    error: Option<&'static str>,
}

const FIREFOX_HOST_KEY: &str = r"Software\Mozilla\NativeMessagingHosts\local.media.downloader";

#[tauri::command]
fn connect_firefox(chrome: Option<bool>) -> AppResult<()> {
    let current = std::env::current_exe().map_err(|_| "firefoxConnectError")?;
    let packaged = current
        .with_file_name("tools")
        .join("media-downloader-firefox.exe");
    let exe = if packaged.is_file() {
        packaged
    } else {
        current.with_file_name("media-downloader-firefox.exe")
    };
    if !exe.is_file() {
        return Err("firefoxHostMissing");
    }
    let dir = firefox::directory().ok_or("firefoxConnectError")?;
    fs::create_dir_all(&dir).map_err(|_| "firefoxConnectError")?;
    let chrome = chrome.unwrap_or(false);
    let (file, registry_key, permission, extension) = if chrome {
        (
            "chrome-native-host.json",
            r"Software\Google\Chrome\NativeMessagingHosts\local.media.downloader",
            "allowed_origins",
            firefox::CHROME_ORIGIN,
        )
    } else {
        (
            "native-host.json",
            FIREFOX_HOST_KEY,
            "allowed_extensions",
            firefox::ADDON_ID,
        )
    };
    let manifest = dir.join(file);
    let mut info = serde_json::json!({
        "name": "local.media.downloader",
        "description": "LoadSail browser bridge",
        "path": exe,
        "type": "stdio",
    });
    info[permission] = serde_json::json!([extension]);
    downloads::write_json(&manifest, &info).map_err(|_| "firefoxConnectError")?;
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (entry, _) = key
        .create_subkey(registry_key)
        .map_err(|_| "firefoxConnectError")?;
    entry
        .set_value("", &manifest.as_os_str())
        .map_err(|_| "firefoxConnectError")
}

#[cfg(not(debug_assertions))]
fn connect_default_firefox() {
    let Some(dir) = firefox::directory() else {
        return;
    };
    let manifest = dir.join("native-host.json");
    let registry = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let existing = registry
        .open_subkey(FIREFOX_HOST_KEY)
        .and_then(|entry| entry.get_value::<std::ffi::OsString, _>(""))
        .ok()
        .map(PathBuf::from);
    // Retain another working installation selected by the user.
    if existing
        .as_ref()
        .filter(|path| **path != manifest)
        .is_some_and(|path| {
            fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.len() <= firefox::MAX_BYTES as u64)
                && fs::read(path)
                    .ok()
                    .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
                    .and_then(|info| info.get("path")?.as_str().map(PathBuf::from))
                    .is_some_and(|host| host.is_absolute() && host.is_file())
        })
    {
        return;
    }
    let _ = connect_firefox(Some(false));
}

#[tauri::command]
fn firefox_downloads() -> Vec<firefox::Report> {
    let Some(dir) = firefox::directory() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "json" || Uuid::parse_str(path.file_stem()?.to_str()?).is_err()
            {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            (metadata.is_file() && metadata.len() <= firefox::MAX_BYTES as u64)
                .then(|| (metadata.modified().ok(), path))
        })
        .collect();
    files.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .take(8)
        .filter_map(|(modified, path)| {
            let file = fs::read(path).ok()?;
            let mut report: firefox::Report = serde_json::from_slice(&file).ok()?;
            let stale = modified
                .and_then(|time| time.elapsed().ok())
                .is_none_or(|elapsed| elapsed.as_secs() > 40);
            if stale {
                for job in &mut report.jobs {
                    if job.status == firefox::BrowserStatus::Running {
                        job.status = firefox::BrowserStatus::FirefoxOffline;
                    }
                }
            }
            Some(report)
        })
        .collect()
}

#[tauri::command]
async fn find_media(state: State<'_, Arc<App>>, url: String) -> AppResult<Vec<Media>> {
    let parsed = discovery::url(&url)?;
    if let Some(id) = youtube::video_id(&parsed) {
        match youtube::find(&id).await {
            Ok(items) => return Ok(items),
            Err(error) => {
                return discovery::find(&state.client, &youtube::watch_url(&id), false)
                    .await
                    .ok()
                    .filter(|items| items.iter().any(|item| item.preview_only))
                    .ok_or(error);
            }
        }
    }
    let (videos, page) = tokio::join!(
        twitter::find(parsed.clone()),
        find_page(&state.client, &url)
    );
    let mut videos = if let Some(videos) = videos {
        videos
    } else {
        let usable = page.as_ref().is_ok_and(|items| {
            items.iter().any(|item| {
                !item.preview_only
                    && (item.url == parsed.as_str()
                        || item.kind == discovery::Kind::Video
                            && (!item.qualities.is_empty()
                                || discovery::file_format(&item.name)
                                    .is_some_and(|(_, kind)| kind == discovery::Kind::Video)))
            })
        });
        if usable
            || discovery::path_format(&parsed).is_some()
            || discovery::is_hls(&parsed)
            || discovery::is_dash(&parsed)
        {
            return page;
        }
        let Some(video) = extractor::find(&state.client, &parsed).await else {
            return page;
        };
        vec![video]
    };
    if let Ok(items) = page {
        let remaining = discovery::GALLERY_LIMIT - videos.len();
        videos.extend(
            items
                .into_iter()
                .filter(|item| item.kind != discovery::Kind::Video)
                .take(remaining),
        );
    }
    Ok(videos)
}

async fn find_page(client: &reqwest::Client, url: &str) -> AppResult<Vec<Media>> {
    let mut best = Vec::new();
    let mut best_count = 0;
    for attempt in 0..=3 {
        let items = match discovery::find(client, url, attempt > 0).await {
            Err(_) if attempt > 0 => break,
            result => result?,
        };
        let (available, changed) = check_streams(client, items).await;
        let count = available.iter().map(|item| item.qualities.len()).sum();
        // A refreshed page can supply new CDN links without discarding a better result.
        if attempt == 0 || (!available.is_empty() && count >= best_count) {
            best = available;
            best_count = count;
        }
        if !changed {
            break;
        }
    }
    Ok(best)
}

async fn check_streams(client: &reqwest::Client, items: Vec<Media>) -> (Vec<Media>, bool) {
    use futures_util::{StreamExt, stream};

    let checked = stream::iter(items.into_iter().map(|mut item| async move {
        let is_stream = item.qualities.iter().any(|quality| {
            quality.url.as_deref() == Some(item.url.as_str())
                && (hls::is_stream_quality(quality) || dash::is_stream_quality(quality))
        });
        let mut changed = hls::retain_available(client, &mut item).await;
        changed |= dash::retain_available(client, &mut item).await;
        if is_stream && item.qualities.is_empty() {
            item.preview_only = true;
            (item.preview.is_some().then_some(item), changed)
        } else {
            (Some(item), changed)
        }
    }))
    .buffered(4)
    .collect::<Vec<_>>()
    .await;
    let changed = checked.iter().any(|(_, changed)| *changed);
    (
        checked.into_iter().filter_map(|(item, _)| item).collect(),
        changed,
    )
}

#[tauri::command]
async fn find_collection(
    state: State<'_, Arc<App>>,
    url: String,
) -> AppResult<discovery::Collection> {
    if let Some(items) = twitter::find(discovery::url(&url)?).await {
        return Ok(discovery::Collection {
            items,
            partial: false,
        });
    }
    let mut collection = discovery::find_collection(&state.client, &url).await?;
    (collection.items, _) = check_streams(&state.client, collection.items).await;
    Ok(collection)
}

#[derive(Serialize)]
struct SavedMedia {
    id: Uuid,
    name: String,
    kind: discovery::Kind,
    size: u64,
    path: PathBuf,
    created_at: Option<u64>,
    modified_at: Option<u64>,
    can_check_qualities: bool,
}

fn saved_media(job: Job) -> Option<SavedMedia> {
    let name = job.file?;
    let (_, kind) = discovery::file_format(&name)?;
    let (path, metadata) = saved_file::resolve(&job.folder, &name)?;
    Some(SavedMedia {
        id: job.id,
        name,
        kind,
        size: metadata.len(),
        path,
        created_at: gallery_timestamp(metadata.created()),
        modified_at: gallery_timestamp(metadata.modified()),
        can_check_qualities: kind == discovery::Kind::Video && saved_youtube_id(&job.url).is_some(),
    })
}

fn gallery_timestamp(result: std::io::Result<std::time::SystemTime>) -> Option<u64> {
    let duration = result.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(duration.as_millis()).ok()
}

fn saved_youtube_id(url: &str) -> Option<String> {
    youtube::video_id(&discovery::url(url).ok()?)
}

fn saved_video_source(state: &App, id: Uuid) -> AppResult<(String, String)> {
    let inner = state.lock();
    let job = inner
        .jobs
        .iter()
        .find(|job| job.id == id && job.status == Status::Completed)
        .ok_or("qualityUnavailable")?;
    let video_id = saved_youtube_id(&job.url).ok_or("qualityUnavailable")?;
    Ok((video_id, job.name.clone()))
}

#[tauri::command]
async fn saved_video_qualities(
    state: State<'_, Arc<App>>,
    id: Uuid,
) -> AppResult<Vec<discovery::Quality>> {
    let (video_id, _) = saved_video_source(&state, id)?;
    youtube::qualities(&video_id).await
}

#[tauri::command]
fn enqueue_saved_quality(state: State<'_, Arc<App>>, id: Uuid, format_id: String) -> AppResult<()> {
    let (video_id, name) = saved_video_source(&state, id)?;
    let name = name.strip_suffix(".mp4").unwrap_or(&name).to_owned();
    enqueue(
        state,
        vec![Selection {
            url: youtube::watch_url(&video_id),
            name,
            size: None,
            preview_only: false,
            format_id: Some(format_id),
            source_origin: None,
            direct: false,
        }],
    )
}

#[tauri::command]
async fn saved_gallery(
    handle: AppHandle,
    state: State<'_, Arc<App>>,
) -> AppResult<Vec<SavedMedia>> {
    let jobs: Vec<_> = state
        .lock()
        .jobs
        .iter()
        .rev()
        .filter(|job| job.status == Status::Completed)
        .cloned()
        .collect();
    let files = tauri::async_runtime::spawn_blocking(move || {
        let mut seen = std::collections::HashSet::new();
        jobs.into_iter()
            .filter_map(saved_media)
            .filter(|file| seen.insert(file.path.clone()))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|_| "disk")?;
    let scope = handle.asset_protocol_scope();
    files
        .into_iter()
        .map(|file| {
            if !scope.is_allowed(&file.path) {
                scope.allow_file(&file.path).map_err(|_| "disk")?;
            }
            Ok(file)
        })
        .collect()
}

#[tauri::command]
async fn inspect_media(
    state: State<'_, Arc<App>>,
    url: String,
    source_origin: Option<String>,
) -> AppResult<discovery::MediaInspection> {
    discovery::inspect(&state.client, &url, source_origin.as_deref()).await
}

#[tauri::command]
async fn inspect_stream_resolution(
    state: State<'_, Arc<App>>,
    url: String,
    source_origin: Option<String>,
    format_id: Option<String>,
) -> AppResult<(u32, u32)> {
    let target = discovery::url(&url)?;
    let referer = source_origin
        .as_deref()
        .map(discovery::url)
        .transpose()?
        .and_then(|source| discovery::media_referer(&source, &target));
    if dash::is_stream(&target, format_id.as_deref()) {
        return dash::inspect_resolution(
            &state.client,
            target,
            format_id.as_deref(),
            referer.as_deref(),
        )
        .await;
    }
    hls::inspect_resolution(
        &state.client,
        target,
        format_id.as_deref(),
        referer.as_deref(),
    )
    .await
}

#[tauri::command]
async fn fetch_preview(state: State<'_, Arc<App>>, url: String) -> AppResult<String> {
    discovery::preview(&state.client, &url).await
}

fn accept_browser_downloads(state: &Arc<App>) {
    for (_, id) in pending_browser_videos() {
        let Ok((path, video)) = browser_request(id) else {
            continue;
        };
        if video.auto_start {
            let name = browser_video_name(&video);
            let format_id = video.format_id.clone();
            if let Err(error) = queue_browser_video(state, id, path, video, name, false, format_id)
            {
                state.lock().error = Some(error);
            }
        }
    }
}

#[tauri::command]
fn snapshot(state: State<'_, Arc<App>>) -> Snapshot {
    let inner = state.lock();
    Snapshot {
        folder: inner.folder.clone(),
        jobs: inner
            .jobs
            .iter()
            .filter(|job| !job.archived)
            .cloned()
            .collect(),
        error: inner.error,
    }
}

#[tauri::command]
async fn pick_folder(state: State<'_, Arc<App>>) -> AppResult<Option<PathBuf>> {
    let folder = state.lock().folder.clone();
    let result = rfd::AsyncFileDialog::new()
        .set_directory(folder)
        .pick_folder()
        .await;
    let Some(handle) = result else {
        return Ok(None);
    };
    let folder = handle.path().to_owned();
    if !folder.is_dir() {
        return Err("disk");
    }
    downloads::write_json(
        &state.state_file.with_file_name("download-folder.json"),
        &state
            .portable_root
            .as_deref()
            .map_or(folder.as_path(), |root| stored_folder(&folder, root)),
    )?;
    state.lock().folder = folder.clone();
    Ok(Some(folder))
}

#[tauri::command]
fn enqueue(state: State<'_, Arc<App>>, items: Vec<Selection>) -> AppResult<()> {
    let mut inner = state.lock();
    let pending = downloads::prepare_selection(items, &inner.folder, &inner.jobs)?;
    save_jobs(&state, &mut inner, pending)?;
    drop(inner);
    state.pump();
    Ok(())
}

#[tauri::command]
async fn inspect_direct(
    state: State<'_, Arc<App>>,
    url: String,
) -> AppResult<downloads::DirectInfo> {
    downloads::inspect_direct(&state.client, &url).await
}

fn save_jobs(state: &App, inner: &mut Inner, pending: Vec<Job>) -> AppResult<()> {
    let previous = inner.jobs.len();
    inner.jobs.extend(pending);
    if let Err(error) = state.save(inner) {
        inner.jobs.truncate(previous);
        return Err(error);
    }
    inner.error = None;
    Ok(())
}

fn browser_request(id: Uuid) -> AppResult<(PathBuf, firefox::VideoRequest)> {
    let path = firefox::incoming_directory()
        .ok_or("disk")?
        .join(format!("{id}.json"));
    let metadata = fs::symlink_metadata(&path).map_err(|_| "browserRequestExpired")?;
    if !metadata.file_type().is_file() || metadata.len() > firefox::MAX_BYTES as u64 {
        return Err("browserRequestExpired");
    }
    let request: firefox::VideoRequest =
        serde_json::from_slice(&fs::read(&path).map_err(|_| "disk")?)
            .map_err(|_| "browserRequestExpired")?;
    request.validate().ok_or("browserRequestExpired")?;
    Ok((path, request))
}

#[derive(Serialize)]
struct IncomingVideo {
    id: Uuid,
    name: String,
    origin: String,
    format: String,
    quality: u16,
    can_check_qualities: bool,
}

fn pending_browser_videos() -> Vec<(std::time::SystemTime, Uuid)> {
    let Some(entries) = firefox::incoming_directory().and_then(|dir| fs::read_dir(dir).ok()) else {
        return Vec::new();
    };
    let mut pending: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = Uuid::parse_str(path.file_stem()?.to_str()?).ok()?;
            (path.extension()? == "json").then_some((entry.metadata().ok()?.modified().ok()?, id))
        })
        .collect();
    pending.sort_unstable();
    pending
}

fn browser_video_name(video: &firefox::VideoRequest) -> String {
    let url = video.validate().expect("validated browser video");
    let extension = video.extension(&url).expect("validated video format");
    if video.title.trim().is_empty() {
        discovery::media_name(&url, extension)
    } else {
        discovery::safe_media_name(&video.title, extension)
    }
}

#[tauri::command]
fn next_browser_video(handle: AppHandle, state: State<'_, Arc<App>>) -> Option<IncomingVideo> {
    for (_, id) in pending_browser_videos() {
        let Ok((path, video)) = browser_request(id) else {
            continue;
        };
        if video.auto_start {
            continue;
        }
        if state.lock().jobs.iter().any(|job| job.id == id) {
            let _ = fs::remove_file(path);
            continue;
        }
        let url = video.validate()?;
        let can_check_qualities = firefox::is_youtube_video(&url);
        let extension = video.extension(&url)?;
        let name = browser_video_name(&video);
        if let Some(window) = handle.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
        return Some(IncomingVideo {
            id,
            name,
            origin: video.origin,
            format: extension.to_owned(),
            quality: video.quality,
            can_check_qualities,
        });
    }
    None
}

#[tauri::command]
async fn browser_video_qualities(id: Uuid) -> AppResult<Vec<discovery::Quality>> {
    let (_, video) = browser_request(id)?;
    let video_id = saved_youtube_id(&video.url).ok_or("qualityUnavailable")?;
    youtube::qualities(&video_id).await
}

#[tauri::command]
fn decide_browser_video(
    state: State<'_, Arc<App>>,
    id: Uuid,
    action: String,
    name: String,
    format_id: Option<String>,
) -> AppResult<()> {
    let (path, video) = browser_request(id)?;
    if action == "cancel" {
        return fs::remove_file(path).map_err(|_| "disk");
    }
    if !matches!(action.as_str(), "now" | "later") {
        return Err("unknown");
    }
    queue_browser_video(&state, id, path, video, name, action == "later", format_id)
}

fn queue_browser_video(
    state: &Arc<App>,
    id: Uuid,
    path: PathBuf,
    video: firefox::VideoRequest,
    name: String,
    deferred: bool,
    format_id: Option<String>,
) -> AppResult<()> {
    let mut inner = state.lock();
    if !inner.jobs.iter().any(|job| job.id == id) {
        let jobs = downloads::prepare_selection(
            vec![Selection {
                url: video.url,
                name,
                size: None,
                preview_only: false,
                format_id,
                source_origin: None,
                direct: false,
            }],
            &inner.folder,
            &inner.jobs,
        );
        let mut jobs = match jobs {
            Ok(jobs) => jobs,
            Err(error) => {
                if video.auto_start {
                    fs::remove_file(path).map_err(|_| "disk")?;
                }
                return Err(error);
            }
        };
        jobs[0].id = id;
        jobs[0].browser_origin = Some(video.origin);
        if deferred {
            jobs[0].status = Status::Deferred;
        }
        save_jobs(state, &mut inner, jobs)?;
    }
    let _ = fs::remove_file(path);
    drop(inner);
    state.pump();
    Ok(())
}

#[tauri::command]
fn change_job(state: State<'_, Arc<App>>, id: Uuid, action: String) -> AppResult<()> {
    let mut inner = state.lock();
    let index = inner
        .jobs
        .iter()
        .position(|j| j.id == id && !j.archived)
        .ok_or("unknown")?;
    if action == "remove" && inner.active.contains_key(&id) {
        return Ok(());
    }
    let original = inner.jobs[index].clone();
    match (action.as_str(), original.status) {
        ("pause", Status::Running) => inner.jobs[index].status = Status::Pausing,
        ("pause", Status::Queued) => inner.jobs[index].status = Status::Paused,
        ("resume", Status::Deferred | Status::Paused | Status::Failed) => {
            inner.jobs[index].status = Status::Queued;
            inner.jobs[index].error = None;
        }
        ("remove", Status::Completed) => inner.jobs[index].archived = true,
        ("remove", Status::Deferred | Status::Failed | Status::Paused | Status::Queued) => {
            inner.jobs.remove(index);
            if let Err(error) = state.save(&inner) {
                inner.jobs.insert(index, original);
                return Err(error);
            }
            let _ = fs::remove_dir_all(original.folder.join(format!(".media-{id}")));
            return Ok(());
        }
        _ => return Ok(()),
    }
    if let Err(error) = state.save(&inner) {
        inner.jobs[index] = original;
        return Err(error);
    }
    inner.error = None;
    if action == "pause" {
        if let Some(stop) = inner.active.get(&id) {
            let _ = stop.send(true);
        }
    }
    drop(inner);
    state.pump();
    Ok(())
}

#[tauri::command]
fn open_folder(state: State<'_, Arc<App>>, id: Uuid) -> AppResult<()> {
    let folder = state
        .lock()
        .jobs
        .iter()
        .find(|j| j.id == id)
        .map(|j| j.folder.clone())
        .ok_or("unknown")?;
    if !folder.is_dir() {
        return Err("disk");
    }
    std::process::Command::new("explorer.exe")
        .arg(folder)
        .spawn()
        .map_err(|_| "disk")?;
    Ok(())
}

fn app_data_directory(handle: &AppHandle) -> tauri::Result<PathBuf> {
    if let Some(root) = paths::portable_root() {
        return Ok(root.join("data"));
    }
    let mut dir = handle.path().app_local_data_dir()?;
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("MEDIA_DOWNLOADER_TEST_DATA") {
        dir = PathBuf::from(path);
    }
    #[cfg(not(debug_assertions))]
    let _ = &mut dir;
    Ok(dir)
}

fn stored_folder<'a>(folder: &'a Path, root: &Path) -> &'a Path {
    folder.strip_prefix(root).unwrap_or(folder)
}

fn resolve_portable_folder(root: &Path, stored: PathBuf) -> std::io::Result<PathBuf> {
    if stored.is_absolute() {
        return Ok(stored);
    }
    if stored
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Ok(root.join(stored));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "Invalid portable download folder",
    ))
}

fn setup(handle: &AppHandle) -> Result<App, Box<dyn std::error::Error>> {
    use std::os::windows::fs::OpenOptionsExt;
    let portable_root = paths::portable_root();
    let dir = app_data_directory(handle)?;
    fs::create_dir_all(&dir)?;
    // Keep the queue and partial files under a single writer across app instances.
    let instance = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(dir.join("queue.lock"))?;
    let state_file = dir.join("queue.json");
    let mut jobs: Vec<Job> = match fs::read(&state_file) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    for job in &mut jobs {
        if let Some(root) = &portable_root {
            job.folder = resolve_portable_folder(root, std::mem::take(&mut job.folder))?;
        }
        if matches!(
            job.status,
            Status::Running | Status::Pausing | Status::Queued
        ) {
            job.status = Status::Paused;
        }
    }
    let mut folder = match &portable_root {
        Some(root) => {
            let folder = root.join("data/Downloads");
            fs::create_dir_all(&folder)?;
            folder
        }
        None => handle.path().download_dir()?,
    };
    if let Ok(contents) = fs::read(dir.join("download-folder.json"))
        && let Ok(saved) = serde_json::from_slice::<PathBuf>(&contents)
        && let Ok(saved) = match &portable_root {
            Some(root) => resolve_portable_folder(root, saved),
            None => Ok(saved),
        }
        && saved.is_dir()
    {
        folder = saved;
    }
    #[cfg(debug_assertions)]
    if portable_root.is_none()
        && let Some(path) = std::env::var_os("MEDIA_DOWNLOADER_TEST_FOLDER")
    {
        folder = PathBuf::from(path);
    }
    #[cfg(not(debug_assertions))]
    let _ = &mut folder;
    let inner = Inner {
        jobs,
        folder,
        active: HashMap::new(),
        error: None,
        updating: false,
    };
    Ok(App {
        client: discovery::client()?,
        inner: Mutex::new(inner),
        state_file,
        portable_root,
        _instance: instance,
    })
}

#[tauri::command]
fn open_youtube(id: String) -> AppResult<()> {
    let url = discovery::url(&youtube::watch_url(&id))?;
    if youtube::video_id(&url).as_deref() != Some(id.as_str()) {
        return Err("invalidUrl");
    }
    open_page(url.to_string())
}

#[tauri::command]
fn open_page(url: String) -> AppResult<()> {
    let url = discovery::url(&url)?;
    std::process::Command::new("explorer.exe")
        .arg(url.as_str())
        .spawn()
        .map_err(|_| "unknown")?;
    Ok(())
}

#[tauri::command]
fn open_saved_file(state: State<'_, Arc<App>>, id: Uuid) -> AppResult<()> {
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::{HSTRING, PCWSTR},
    };

    let job = state
        .lock()
        .jobs
        .iter()
        .find(|job| job.id == id && job.status == Status::Completed)
        .cloned()
        .ok_or("disk")?;
    let (path, _) = saved_file::resolve(&job.folder, job.file.as_deref().ok_or("disk")?)
        .ok_or("disk")?;
    if !path.is_absolute() {
        return Err("disk");
    }
    let path = HSTRING::from(path.as_os_str());
    let result = unsafe {
        ShellExecuteW(None, PCWSTR::null(), &path, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    if result.0 as isize <= 32 {
        return Err("disk");
    }
    Ok(())
}

struct TrayMenu {
    open: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}

#[tauri::command]
fn set_tray_language(menu: State<'_, TrayMenu>, open: String, quit: String) -> AppResult<()> {
    menu.open.set_text(open).map_err(|_| "unknown")?;
    menu.quit.set_text(quit).map_err(|_| "unknown")
}

fn show_main_window(handle: &AppHandle) {
    if let Some(window) = handle.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn install_tray(handle: &AppHandle) -> tauri::Result<()> {
    // The webview replaces these labels with the selected language on startup.
    let open = MenuItem::with_id(handle, "tray-open", "Відкрити LoadSail", true, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "tray-quit", "Вийти з LoadSail", true, None::<&str>)?;
    let menu = Menu::with_items(handle, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("clipsail")
        .tooltip("LoadSail")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|handle, event| match event.id().as_ref() {
            "tray-open" => show_main_window(handle),
            "tray-quit" => handle.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = handle.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(handle)?;
    handle.manage(TrayMenu { open, quit });
    Ok(())
}

struct DesktopMica(bool);

#[tauri::command]
fn desktop_mica(backdrop: State<'_, DesktopMica>) -> bool {
    backdrop.0
}

fn keep_single_title_bar(hwnd: windows::Win32::Foundation::HWND) -> windows::core::Result<()> {
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongW, IsIconic, MINMAXINFO, STYLESTRUCT, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongW, SetWindowPos,
        WM_GETMINMAXINFO, WM_NCACTIVATE, WM_NCDESTROY, WM_STYLECHANGING, WS_CAPTION,
    };

    unsafe extern "system" fn window_style(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        _data: usize,
    ) -> LRESULT {
        if message == WM_NCACTIVATE && !unsafe { IsIconic(hwnd) }.as_bool() {
            // Keep Tao's focus handling. -1 tells DefWindowProc to skip painting the old frame.
            return unsafe { DefSubclassProc(hwnd, message, wparam, LPARAM(-1)) };
        }
        // Tauri restores the Win32 caption style during maximize and restore.
        // Extending the frame would paint those buttons beneath the integrated title bar.
        if message == WM_STYLECHANGING && wparam.0 as i32 == GWL_STYLE.0 {
            if let Some(style) = unsafe { (lparam.0 as *mut STYLESTRUCT).as_mut() } {
                style.styleNew &= !WS_CAPTION.0;
            }
        } else if message == WM_NCDESTROY {
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(window_style), id) };
        }
        let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        // A captionless window must use the work area to keep the taskbar accessible.
        if message == WM_GETMINMAXINFO
            && let Some(limits) = unsafe { (lparam.0 as *mut MINMAXINFO).as_mut() }
        {
            let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                limits.ptMaxPosition = POINT {
                    x: info.rcWork.left - info.rcMonitor.left,
                    y: info.rcWork.top - info.rcMonitor.top,
                };
                limits.ptMaxSize = POINT {
                    x: info.rcWork.right - info.rcWork.left,
                    y: info.rcWork.bottom - info.rcWork.top,
                };
            }
        }
        result
    }

    unsafe {
        SetWindowSubclass(hwnd, Some(window_style), 1, 0).ok()?;
        let style = GetWindowLongW(hwnd, GWL_STYLE) & !(WS_CAPTION.0 as i32);
        SetWindowLongW(hwnd, GWL_STYLE, style);
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        )
    }
}

fn enable_desktop_mica(window: &tauri::WebviewWindow) -> bool {
    use windows::Win32::Graphics::Dwm::{
        DWMSBT_MAINWINDOW, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
        DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
    };
    use windows::Win32::UI::Controls::MARGINS;

    let Ok(hwnd) = window.hwnd() else {
        return false;
    };
    if keep_single_title_bar(hwnd).is_err() {
        return false;
    }
    // Windows 11 supplies the backdrop. Extend it through the transparent WebView's client area.
    let margins = MARGINS {
        cxLeftWidth: -1,
        ..Default::default()
    };
    let applied = unsafe {
        let dark = 1i32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            std::ptr::from_ref(&DWMWA_COLOR_NONE).cast(),
            size_of_val(&DWMWA_COLOR_NONE) as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            std::ptr::from_ref(&dark).cast(),
            size_of_val(&dark) as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            std::ptr::from_ref(&DWMSBT_MAINWINDOW).cast(),
            size_of_val(&DWMSBT_MAINWINDOW) as u32,
        )
        .and_then(|()| DwmExtendFrameIntoClientArea(hwnd, &margins))
    };
    applied.is_ok()
}

fn main() {
    let browser_launch = std::env::args().any(|arg| arg == "--browser-request");
    let mut context = tauri::generate_context!();
    if browser_launch {
        context.config_mut().app.windows[0].focus = false;
    }
    let portable_window = paths::portable_root().map(|root| {
        let config = &mut context.config_mut().app.windows[0];
        config.create = false;
        (config.clone(), root.join("data/webview"))
    });
    let commands: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
        desktop_mica,
        find_media,
        find_collection,
        saved_gallery,
        saved_video_qualities,
        enqueue_saved_quality,
        inspect_media,
        inspect_direct,
        inspect_stream_resolution,
        playback::set_preview_source,
        player_capture::start_player_capture,
        player_capture::poll_player_capture,
        player_capture::stop_player_capture,
        fetch_preview,
        snapshot,
        firefox_downloads,
        connect_firefox,
        set_tray_language,
        next_browser_video,
        browser_video_qualities,
        decide_browser_video,
        pick_folder,
        enqueue,
        change_job,
        open_folder,
        open_saved_file,
        open_page,
        open_youtube,
        updater::update_channel,
        updater::check_update,
        updater::install_update
    ];
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            match setup(app.handle()) {
                Ok(state) => {
                    if let Some((config, profile)) = &portable_window {
                        tauri::WebviewWindowBuilder::from_config(app, config)?
                            .data_directory(profile.clone())
                            .build()?;
                    }
                    app.manage(DesktopMica(
                        app.get_webview_window("main")
                            .as_ref()
                            .is_some_and(enable_desktop_mica),
                    ));
                    let state = Arc::new(state);
                    app.manage(Arc::clone(&state));
                    app.manage(player_capture::PlayerCapture::default());
                    playback::install(app.handle());
                    install_tray(app.handle())?;
                    #[cfg(not(debug_assertions))]
                    if !browser_launch {
                        connect_default_firefox();
                        if state.portable_root.is_none()
                            && firefox::directory()
                                .is_some_and(|dir| dir.join("chrome-native-host.json").is_file())
                        {
                            let _ = connect_firefox(Some(true));
                        }
                    }
                    let handle = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        let mut timer =
                            tokio::time::interval(std::time::Duration::from_millis(650));
                        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        loop {
                            timer.tick().await;
                            accept_browser_downloads(&state);
                            if fs::remove_file(state.state_file.with_file_name("show-window"))
                                .is_ok()
                            {
                                show_main_window(&handle);
                            }
                        }
                    });
                }
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .and_then(std::io::Error::raw_os_error)
                        == Some(32) =>
                {
                    if !browser_launch {
                        fs::write(app_data_directory(app.handle())?.join("show-window"), [])?;
                    }
                    app.handle().exit(0);
                }
                Err(error) => return Err(error),
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && window.app_handle().tray_by_id("clipsail").is_some()
                && window.hide().is_ok()
            {
                api.prevent_close();
            }
        })
        .invoke_handler(move |invoke| {
            if invoke.message.webview_ref().label() != "main" {
                invoke.resolver.reject("unsupported");
                return true;
            }
            commands(invoke)
        })
        .run(context);
    if let Err(error) = result {
        rfd::MessageDialog::new()
            .set_title("LoadSail")
            .set_description(format!(
                "Не вдалося запустити програму. / Could not start the app.\n\n{error}"
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}
