use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tauri::{AppHandle, Manager, State};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA, ICoreWebView2_2,
};
use webview2_com::{
    CoTaskMemPWSTR, WebResourceRequestedEventHandler, WebResourceResponseReceivedEventHandler,
};
use windows::core::{HSTRING, Interface, PWSTR, w};

use crate::{
    discovery::{self, AppResult},
    extractor,
};

#[derive(Default)]
pub struct PreviewSource(Mutex<VecDeque<MediaRequest>>);

struct MediaRequest {
    url: String,
    referer: Option<String>,
    user_agent: Option<String>,
}

impl MediaRequest {
    fn redirected(&self, location: &str) -> Option<Self> {
        let original = discovery::url(&self.url).ok()?;
        let target = discovery::url(original.join(location).ok()?.as_str()).ok()?;
        if original.scheme() == "https" && target.scheme() != "https" {
            return None;
        }
        Some(Self {
            url: target.to_string(),
            referer: self.referer.clone(),
            user_agent: self.user_agent.clone(),
        })
    }
}

fn remember(requests: &mut VecDeque<MediaRequest>, request: MediaRequest) {
    requests.push_back(request);
    if requests.len() > 32 {
        requests.pop_front();
    }
}

#[tauri::command]
pub async fn set_preview_source(
    state: State<'_, Arc<PreviewSource>>,
    url: String,
    source_origin: Option<String>,
    format_id: Option<String>,
) -> AppResult<String> {
    let target = discovery::url(&url)?;
    let (target, referer, user_agent) = if let Some(id) = format_id {
        let source = extractor::resolve(&target, &id).await?;
        if source.stream_id.is_some() {
            return Err("unsupported");
        }
        (source.url, source.referer, source.user_agent)
    } else {
        let referer = source_origin
            .as_deref()
            .map(discovery::url)
            .transpose()?
            .and_then(|source| discovery::media_referer(&source, &target));
        (target, referer, None)
    };
    let url = target.to_string();
    let mut requests = state.0.lock().map_err(|_| "unknown")?;
    requests.retain(|request| request.url != url);
    if referer.is_some() || user_agent.is_some() {
        remember(
            &mut requests,
            MediaRequest {
                url: url.clone(),
                referer,
                user_agent,
            },
        );
    }
    Ok(url)
}

pub fn install(app: &AppHandle) {
    let source = Arc::new(PreviewSource::default());
    app.manage(Arc::clone(&source));
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.with_webview(move |webview| {
        // Keep range requests and playback in WebView2, changing only the matched media request.
        let _ = unsafe {
            (|| -> windows::core::Result<()> {
                let core = webview.controller().CoreWebView2()?;
                for pattern in [w!("http://*"), w!("https://*")] {
                    core.AddWebResourceRequestedFilter(
                        pattern,
                        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
                    )?;
                }
                let redirects = Arc::clone(&source);
                let handler =
                    WebResourceRequestedEventHandler::create(Box::new(move |_, event| {
                        let Some(event) = event else { return Ok(()) };
                        let Ok(guard) = source.0.lock() else {
                            return Ok(());
                        };
                        let request = event.Request()?;
                        let mut uri = PWSTR::null();
                        request.Uri(&mut uri)?;
                        let url = CoTaskMemPWSTR::from(uri).to_string();
                        if let Some(source) = guard.iter().find(|source| source.url == url) {
                            let headers = request.Headers()?;
                            if let Some(referer) = &source.referer {
                                headers.SetHeader(w!("Referer"), &HSTRING::from(referer))?;
                            }
                            if let Some(user_agent) = &source.user_agent {
                                headers.SetHeader(w!("User-Agent"), &HSTRING::from(user_agent))?;
                            }
                        }
                        Ok(())
                    }));
                let mut token = 0;
                core.add_WebResourceRequested(&handler, &mut token)?;
                let handler =
                    WebResourceResponseReceivedEventHandler::create(Box::new(move |_, event| {
                        let Some(event) = event else { return Ok(()) };
                        let response = event.Response()?;
                        let mut status = 0;
                        response.StatusCode(&mut status)?;
                        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                            return Ok(());
                        }
                        let mut uri = PWSTR::null();
                        event.Request()?.Uri(&mut uri)?;
                        let url = CoTaskMemPWSTR::from(uri).to_string();
                        let Ok(mut requests) = redirects.0.lock() else {
                            return Ok(());
                        };
                        let Some(request) = requests.iter().find(|request| request.url == url)
                        else {
                            return Ok(());
                        };
                        let mut location = PWSTR::null();
                        if response
                            .Headers()?
                            .GetHeader(w!("Location"), &mut location)
                            .is_err()
                        {
                            return Ok(());
                        }
                        // WebView2 may send subsequent seek requests directly to this URL.
                        if let Some(target) =
                            request.redirected(&CoTaskMemPWSTR::from(location).to_string())
                            && !requests.iter().any(|request| request.url == target.url)
                        {
                            remember(&mut requests, target);
                        }
                        Ok(())
                    }));
                core.cast::<ICoreWebView2_2>()?
                    .add_WebResourceResponseReceived(&handler, &mut token)
            })()
        };
    });
}
