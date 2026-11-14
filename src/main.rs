#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod discovery;
mod downloads;
mod firefox;
mod updater;
mod youtube;

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
    inner: Mutex<Inner>,
    _instance: fs::File,
}

impl App {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("download queue lock")
    }

    fn save(&self, inner: &Inner) -> AppResult<()> {
        downloads::write_json(&self.state_file, &inner.jobs)
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

#[tauri::command]
fn connect_firefox() -> AppResult<()> {
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
    let manifest = dir.join("native-host.json");
    let info = serde_json::json!({
        "name": "local.media.downloader",
        "description": "ClipSail Firefox bridge",
        "path": exe,
        "type": "stdio",
        "allowed_extensions": [firefox::ADDON_ID],
    });
    downloads::write_json(&manifest, &info).map_err(|_| "firefoxConnectError")?;
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (entry, _) = key
        .create_subkey(r"Software\Mozilla\NativeMessagingHosts\local.media.downloader")
        .map_err(|_| "firefoxConnectError")?;
    entry
        .set_value("", &manifest.as_os_str())
        .map_err(|_| "firefoxConnectError")
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
                return discovery::find(&state.client, &youtube::watch_url(&id))
                    .await
                    .ok()
                    .filter(|items| items.iter().any(|item| item.preview_only))
                    .ok_or(error);
            }
        }
    }
    discovery::find(&state.client, &url).await
}

#[tauri::command]
async fn find_collection(
    state: State<'_, Arc<App>>,
    url: String,
) -> AppResult<discovery::Collection> {
    discovery::find_collection(&state.client, &url).await
}

#[derive(Serialize)]
struct SavedMedia {
    id: Uuid,
    name: String,
    kind: discovery::Kind,
    size: u64,
    path: PathBuf,
    saved_at: Option<u64>,
    can_check_qualities: bool,
}

fn saved_media(job: Job) -> Option<SavedMedia> {
    let name = job.file?;
    let (_, kind) = discovery::file_format(&name)?;
    let mut components = Path::new(&name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return None;
    }
    let path = job.folder.join(&name);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let folder = job.folder.canonicalize().ok()?;
    let resolved = path.canonicalize().ok()?;
    if resolved.parent() != Some(folder.as_path()) {
        return None;
    }
    Some(SavedMedia {
        id: job.id,
        name,
        kind,
        size: metadata.len(),
        path,
        saved_at: metadata
            .created()
            .or_else(|_| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
        can_check_qualities: kind == discovery::Kind::Video && saved_youtube_id(&job.url).is_some(),
    })
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
async fn inspect_media(state: State<'_, Arc<App>>, url: String) -> AppResult<Option<u64>> {
    discovery::inspect(&state.client, &url).await
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
        &folder,
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
    let extension = if firefox::is_youtube_video(&url) {
        "mp4"
    } else {
        discovery::file_format(url.path())
            .expect("validated video format")
            .0
    };
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
        let extension = if can_check_qualities {
            "mp4"
        } else {
            discovery::file_format(url.path())?.0
        };
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
    let mut dir = handle.path().app_local_data_dir()?;
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("MEDIA_DOWNLOADER_TEST_DATA") {
        dir = PathBuf::from(path);
    }
    #[cfg(not(debug_assertions))]
    let _ = &mut dir;
    Ok(dir)
}

fn setup(handle: &AppHandle) -> Result<App, Box<dyn std::error::Error>> {
    use std::os::windows::fs::OpenOptionsExt;
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
        if matches!(
            job.status,
            Status::Running | Status::Pausing | Status::Queued
        ) {
            job.status = Status::Paused;
        }
    }
    let mut folder = handle.path().download_dir()?;
    if let Ok(contents) = fs::read(dir.join("download-folder.json"))
        && let Ok(saved) = serde_json::from_slice::<PathBuf>(&contents)
        && saved.is_dir()
    {
        folder = saved;
    }
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("MEDIA_DOWNLOADER_TEST_FOLDER") {
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
    let job = state
        .lock()
        .jobs
        .iter()
        .find(|job| job.id == id && job.status == Status::Completed)
        .cloned()
        .ok_or("disk")?;
    let file = saved_media(job).ok_or("disk")?;
    if !file.path.is_absolute() {
        return Err("disk");
    }
    std::process::Command::new("explorer.exe")
        .arg(file.path)
        .spawn()
        .map_err(|_| "disk")?;
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
    let open = MenuItem::with_id(handle, "tray-open", "Відкрити ClipSail", true, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "tray-quit", "Вийти з ClipSail", true, None::<&str>)?;
    let menu = Menu::with_items(handle, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("clipsail")
        .tooltip("ClipSail")
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

fn main() {
    let browser_launch = std::env::args().any(|arg| arg == "--browser-request");
    let mut context = tauri::generate_context!();
    if browser_launch {
        context.config_mut().app.windows[0].focus = false;
    }
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            match setup(app.handle()) {
                Ok(state) => {
                    let state = Arc::new(state);
                    app.manage(Arc::clone(&state));
                    install_tray(app.handle())?;
                    // Update an existing browser registration when Pullora launches a new version folder.
                    #[cfg(not(debug_assertions))]
                    if !browser_launch
                        && firefox::directory()
                            .is_some_and(|dir| dir.join("native-host.json").is_file())
                    {
                        let _ = connect_firefox();
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
        .invoke_handler(tauri::generate_handler![
            find_media,
            find_collection,
            saved_gallery,
            saved_video_qualities,
            enqueue_saved_quality,
            inspect_media,
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
        ])
        .run(context);
    if let Err(error) = result {
        rfd::MessageDialog::new()
            .set_title("ClipSail")
            .set_description(format!(
                "Не вдалося запустити програму. / Could not start the app.\n\n{error}"
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}
