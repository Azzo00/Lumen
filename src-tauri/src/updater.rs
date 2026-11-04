//! Automatic updates from GitHub Releases (latest.json published by the release workflow).
//! Checks one minute after start and then every 6 hours; when a newer signed version
//! exists the notch shows a popup with "Installa". Installing downloads the NSIS
//! installer, verifies its signature with the public key in tauri.conf.json, runs it
//! in passive mode and restarts Lumen.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    version: String,
    current: String,
    notes: Option<String>,
}

static PENDING: Mutex<Option<Update>> = Mutex::new(None);

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            if let Ok(Some(info)) = tauri::async_runtime::block_on(check(&app)) {
                let _ = app.emit("update-available", info);
            }
            std::thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("Controllo non riuscito ({e})"))?;
    let info = update.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone(),
    });
    if let Ok(mut p) = PENDING.lock() {
        *p = update;
    }
    Ok(info)
}

/// "Controlla aggiornamenti" in the settings.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    check(&app).await
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let pending = PENDING.lock().ok().and_then(|mut p| p.take());
    let update = match pending {
        Some(u) => u,
        None => match app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? {
            Some(u) => u,
            None => return Err("Nessun aggiornamento disponibile".into()),
        },
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("Installazione non riuscita ({e})"))?;
    // on Windows the installer closes Lumen itself; this covers the other cases
    app.restart();
}
