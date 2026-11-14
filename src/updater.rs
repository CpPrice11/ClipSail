use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{App, AppResult, Status};
use std::sync::Arc;

#[derive(Serialize)]
pub struct Channel {
    kind: &'static str,
    version: String,
}

#[tauri::command]
pub fn update_channel(app: AppHandle) -> Channel {
    let kind = crate::youtube::app_folder().map_or("portable", |folder| {
        if folder.join("pullora-package").is_file() {
            "pullora"
        } else if folder.join("uninstall.exe").is_file() {
            "standalone"
        } else {
            "portable"
        }
    });
    Channel {
        kind,
        version: app.package_info().version.to_string(),
    }
}

async fn available_update(app: &AppHandle) -> AppResult<Option<Update>> {
    if update_channel(app.clone()).kind != "standalone" {
        return Err("updateUnavailable");
    }
    app.updater_builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "updateUnavailable")?
        .check()
        .await
        .map_err(|_| "updateCheckFailed")
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> AppResult<Option<String>> {
    Ok(available_update(&app).await?.map(|update| update.version))
}

#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, Arc<App>>) -> AppResult<()> {
    let Some(update) = available_update(&app).await? else {
        return Err("updateCurrent");
    };
    {
        let mut inner = state.lock();
        if inner.updating
            || !inner.active.is_empty()
            || inner.jobs.iter().any(|job| job.status == Status::Queued)
        {
            return Err("updateBusy");
        }
        inner.updating = true;
    }
    let result = update.download_and_install(|_, _| {}, || {}).await;
    state.lock().updating = false;
    state.inner().pump();
    result.map_err(|_| "updateInstallFailed")
}
