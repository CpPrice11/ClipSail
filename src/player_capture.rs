use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::Url;
use serde::Serialize;
use tauri::{
    AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder, webview::NewWindowResponse,
};
use uuid::Uuid;
use webview2_com::{
    CoTaskMemPWSTR,
    Microsoft::Web::WebView2::Win32::{COREWEBVIEW2_PERMISSION_STATE_DENY, ICoreWebView2_2},
    PermissionRequestedEventHandler, WebResourceResponseReceivedEventHandler,
};
use windows::core::{HSTRING, Interface, PWSTR, w};

use crate::{
    App,
    discovery::{self, AppResult, Kind, Media, Quality},
};

#[derive(Default)]
pub struct PlayerCapture(Mutex<Option<Arc<Session>>>);

struct Session {
    id: Uuid,
    title: String,
    inbox: Mutex<Inbox>,
}

#[derive(Default)]
struct Inbox {
    seen: HashSet<String>,
    items: VecDeque<Media>,
    failed: bool,
}

impl Session {
    fn label(&self) -> String {
        format!("media-player-{}", self.id)
    }
}

fn player_address(value: &str) -> AppResult<Url> {
    let url = discovery::url(value)?;
    if url
        .host_str()
        .is_some_and(|host| host.trim_end_matches('.').ends_with(".localhost"))
    {
        return Err("invalidUrl");
    }
    Ok(url)
}

#[tauri::command]
pub async fn start_player_capture(
    app: AppHandle,
    state: State<'_, PlayerCapture>,
    url: String,
    title: String,
) -> AppResult<Uuid> {
    let url = player_address(&url)?;
    let session = Arc::new(Session {
        id: Uuid::new_v4(),
        title: url.host_str().unwrap_or("LoadSail").to_owned(),
        inbox: Mutex::new(Inbox::default()),
    });
    let previous = state
        .0
        .lock()
        .map_err(|_| "playerCaptureFailed")?
        .replace(Arc::clone(&session));
    if let Some(window) = previous.and_then(|old| app.get_webview_window(&old.label())) {
        let _ = window.destroy();
    }
    let app_root = crate::app_data_directory(&app).map_err(|_| "playerCaptureFailed")?;
    let window = WebviewWindowBuilder::new(
        &app,
        session.label(),
        WebviewUrl::External(Url::parse("about:blank").expect("blank URL")),
    )
    .title(title.chars().take(120).collect::<String>())
    .inner_size(1050.0, 740.0)
    .data_directory(app_root.join("player-webview"))
    .incognito(true)
    .browser_extensions_enabled(false)
    .devtools(false)
    .on_navigation(|url| url.as_str() == "about:blank" || player_address(url.as_str()).is_ok())
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .on_download(|_, _| false)
    .build()
    .map_err(|_| "playerCaptureFailed")?;
    let id = session.id;
    window
        .with_webview(move |webview| {
            let capture = Arc::clone(&session);
            let result = unsafe {
                (|| -> windows::core::Result<()> {
                    let core = webview.controller().CoreWebView2()?;
                    let mut token = 0;
                    let permissions =
                        PermissionRequestedEventHandler::create(Box::new(|_, args| {
                            if let Some(args) = args {
                                args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                            }
                            Ok(())
                        }));
                    core.add_PermissionRequested(&permissions, &mut token)?;
                    let handler = WebResourceResponseReceivedEventHandler::create(Box::new(
                        move |sender, event| {
                            let Some(event) = event else { return Ok(()) };
                            let response = event.Response()?;
                            let mut status = 0;
                            response.StatusCode(&mut status)?;
                            if !(200..300).contains(&status) {
                                return Ok(());
                            }
                            let request = event.Request()?;
                            let mut uri = PWSTR::null();
                            request.Uri(&mut uri)?;
                            let Ok(url) = player_address(&CoTaskMemPWSTR::from(uri).to_string())
                            else {
                                return Ok(());
                            };
                            let mut header = PWSTR::null();
                            let mime = response
                                .Headers()?
                                .GetHeader(w!("Content-Type"), &mut header)
                                .ok()
                                .map(|()| CoTaskMemPWSTR::from(header).to_string());
                            let format = mime.as_deref().and_then(|mime| {
                                discovery::stream_mime(mime.split(';').next()?.trim())
                            });
                            let stream = discovery::stream_id(&url, format);
                            let mut method = PWSTR::null();
                            request.Method(&mut method)?;
                            if CoTaskMemPWSTR::from(method).to_string() != "GET" {
                                return Ok(());
                            }
                            let headers = request.Headers()?;
                            let quality = if let Some(stream) = stream {
                                discovery::stream_quality(&url, stream, None)
                            } else {
                                // Fetches made by streaming players can be individual segments.
                                // A direct file comes from the browser's media element request.
                                let mut destination = PWSTR::null();
                                if headers
                                    .GetHeader(w!("Sec-Fetch-Dest"), &mut destination)
                                    .is_err()
                                    || CoTaskMemPWSTR::from(destination).to_string() != "video"
                                {
                                    return Ok(());
                                }
                                let Some((extension, Kind::Video)) =
                                    discovery::resource_format(&url, mime.as_deref().unwrap_or(""))
                                else {
                                    return Ok(());
                                };
                                Quality {
                                    url: Some(url.to_string()),
                                    source_origin: None,
                                    format_id: None,
                                    extension: extension.to_owned(),
                                    width: None,
                                    height: None,
                                    size: None,
                                }
                            };
                            let mut referer = PWSTR::null();
                            let source_origin = headers
                                .GetHeader(w!("Referer"), &mut referer)
                                .ok()
                                .and_then(|()| {
                                    player_address(&CoTaskMemPWSTR::from(referer).to_string()).ok()
                                })
                                .and_then(|source| discovery::media_referer(&source, &url));
                            let mut title = PWSTR::null();
                            let name = sender
                                .and_then(|core| core.DocumentTitle(&mut title).ok())
                                .map(|()| CoTaskMemPWSTR::from(title).to_string())
                                .filter(|name| !name.trim().is_empty())
                                .unwrap_or_else(|| capture.title.clone());
                            let Ok(mut inbox) = capture.inbox.lock() else {
                                return Ok(());
                            };
                            if inbox.seen.len() >= 64 || !inbox.seen.insert(url.to_string()) {
                                return Ok(());
                            }
                            inbox.items.push_back(Media {
                                url: url.to_string(),
                                source_origin,
                                name: name.chars().take(120).collect(),
                                photo_title: None,
                                kind: Kind::Video,
                                size: None,
                                width: None,
                                height: None,
                                duration: None,
                                preview: None,
                                preview_only: false,
                                page_asset: false,
                                qualities: vec![quality],
                            });
                            Ok(())
                        },
                    ));
                    core.cast::<ICoreWebView2_2>()?
                        .add_WebResourceResponseReceived(&handler, &mut token)?;
                    core.Navigate(&HSTRING::from(url.as_str()))
                })()
            };
            if result.is_err()
                && let Ok(mut inbox) = session.inbox.lock()
            {
                inbox.failed = true;
            }
        })
        .map_err(|_| "playerCaptureFailed")?;
    Ok(id)
}

#[derive(Serialize)]
pub struct CaptureUpdate {
    open: bool,
    items: Vec<Media>,
}

#[tauri::command]
pub async fn poll_player_capture(
    handle: AppHandle,
    state: State<'_, PlayerCapture>,
    app: State<'_, Arc<App>>,
    id: Uuid,
) -> AppResult<CaptureUpdate> {
    let session = state
        .0
        .lock()
        .map_err(|_| "playerCaptureFailed")?
        .as_ref()
        .filter(|session| session.id == id)
        .cloned()
        .ok_or("playerCaptureFailed")?;
    let items = {
        let mut inbox = session.inbox.lock().map_err(|_| "playerCaptureFailed")?;
        if inbox.failed {
            return Err("playerCaptureFailed");
        }
        let count = inbox.items.len().min(4);
        inbox.items.drain(..count).collect()
    };
    let (items, _) = tokio::time::timeout(
        Duration::from_secs(16),
        crate::check_streams(&app.client, items),
    )
    .await
    .unwrap_or_default();
    let open = handle.get_webview_window(&session.label()).is_some()
        || session
            .inbox
            .lock()
            .is_ok_and(|inbox| !inbox.items.is_empty());
    Ok(CaptureUpdate { open, items })
}

#[tauri::command]
pub fn stop_player_capture(
    handle: AppHandle,
    state: State<'_, PlayerCapture>,
    id: Uuid,
) -> AppResult<()> {
    let session = {
        let mut active = state.0.lock().map_err(|_| "playerCaptureFailed")?;
        if active.as_ref().is_some_and(|session| session.id == id) {
            active.take()
        } else {
            None
        }
    };
    if let Some(window) = session.and_then(|session| handle.get_webview_window(&session.label())) {
        window.destroy().map_err(|_| "playerCaptureFailed")?;
    }
    Ok(())
}
