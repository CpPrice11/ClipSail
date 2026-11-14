use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager, State};
use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA;
use webview2_com::{CoTaskMemPWSTR, WebResourceRequestedEventHandler};
use windows::core::{HSTRING, PWSTR, w};

use crate::discovery::{self, AppResult};

#[derive(Default)]
pub struct PreviewSource(Mutex<Option<(String, String)>>);

#[tauri::command]
pub fn set_preview_source(
    state: State<'_, Arc<PreviewSource>>,
    url: String,
    source_origin: Option<String>,
) -> AppResult<()> {
    let target = discovery::url(&url)?;
    let referer = source_origin
        .as_deref()
        .map(discovery::url)
        .transpose()?
        .and_then(|source| discovery::media_referer(&source, &target));
    *state.0.lock().map_err(|_| "unknown")? = referer.map(|referer| (target.to_string(), referer));
    Ok(())
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
                let handler =
                    WebResourceRequestedEventHandler::create(Box::new(move |_, event| {
                        let Some(event) = event else { return Ok(()) };
                        let Ok(guard) = source.0.lock() else {
                            return Ok(());
                        };
                        let Some((target, referer)) = guard.as_ref() else {
                            return Ok(());
                        };
                        let request = event.Request()?;
                        let mut uri = PWSTR::null();
                        request.Uri(&mut uri)?;
                        if &CoTaskMemPWSTR::from(uri).to_string() == target {
                            request
                                .Headers()?
                                .SetHeader(w!("Referer"), &HSTRING::from(referer))?;
                        }
                        Ok(())
                    }));
                let mut token = 0;
                core.add_WebResourceRequested(&handler, &mut token)
            })()
        };
    });
}
