//! User settings: stored as JSON in the app config dir
//! (%APPDATA%\dev.lumen.notch\settings.json), cached in memory, broadcast as
//! `settings-changed` whenever they are saved.

use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub const ALL_MODES: [&str; 6] = ["music", "controls", "pc", "status", "calendar", "shelf"];

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Views reachable with the mouse wheel, in order.
    pub modes: Vec<String>,
    /// "cover" = colour taken from the album art, "fixed" = `accent_color`.
    pub accent_mode: String,
    pub accent_color: String,
    /// Size multiplier of the pill (0.85 – 1.25).
    pub scale: f64,
    /// Monitor name, or "" for the primary monitor.
    pub monitor: String,
    /// Horizontal position along the top edge: 0 = left, 0.5 = centre, 1 = right.
    pub position: f64,
    pub hide_fullscreen: bool,
    pub notifications: bool,
    /// Show the text of notifications, or only the app name.
    pub notification_preview: bool,
    pub device_popups: bool,
    pub download_popups: bool,
    /// the shelf view was added to `modes` once (0.6.0); after that the user decides
    pub shelf_added: bool,
    pub weather_name: String,
    pub weather_lat: Option<f64>,
    pub weather_lon: Option<f64>,
    /// Where the calendar comes from: "outlook" (classic Outlook, COM), "ics" (published link), "off".
    pub calendar_source: String,
    /// Published ICS link of the Outlook calendar, used when the source is "ics".
    pub calendar_url: String,
    /// Folder whose repositories the "Git pull" tile updates.
    pub git_root: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            modes: ALL_MODES.iter().map(|m| m.to_string()).collect(),
            accent_mode: "cover".into(),
            accent_color: "#ff8a5c".into(),
            scale: 1.0,
            monitor: String::new(),
            position: 0.5,
            hide_fullscreen: true,
            notifications: true,
            notification_preview: true,
            device_popups: true,
            download_popups: true,
            shelf_added: false,
            weather_name: String::new(),
            weather_lat: None,
            weather_lon: None,
            calendar_source: "outlook".into(),
            calendar_url: String::new(),
            git_root: r"D:\Project".into(),
        }
    }
}

impl Settings {
    fn sanitized(mut self) -> Self {
        if !self.shelf_added {
            if !self.modes.iter().any(|m| m == "shelf") {
                self.modes.push("shelf".into());
            }
            self.shelf_added = true;
        }
        self.modes.retain(|m| ALL_MODES.contains(&m.as_str()));
        self.modes.dedup();
        if self.modes.is_empty() {
            self.modes = vec!["music".into()];
        }
        if !self.scale.is_finite() {
            self.scale = 1.0;
        }
        self.scale = self.scale.clamp(0.85, 1.25);
        if !self.position.is_finite() {
            self.position = 0.5;
        }
        self.position = self.position.clamp(0.0, 1.0);
        if self.accent_mode != "fixed" {
            self.accent_mode = "cover".into();
        }
        self.calendar_url = self.calendar_url.trim().to_string();
        if !["outlook", "ics", "off"].contains(&self.calendar_source.as_str()) {
            self.calendar_source = "outlook".into();
        }
        self.git_root = self.git_root.trim().to_string();
        self
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn get() -> Settings {
    SETTINGS.read().ok().and_then(|s| s.clone()).unwrap_or_default()
}

fn file_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

pub fn load(app: &AppHandle) {
    let loaded = file_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .unwrap_or_default()
        .sanitized();
    if let Ok(mut s) = SETTINGS.write() {
        *s = Some(loaded);
    }
}

#[tauri::command]
pub fn get_settings() -> Settings {
    get()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    store(&app, settings)
}

pub fn store(app: &AppHandle, settings: Settings) -> Result<Settings, String> {
    let settings = settings.sanitized();
    let path = file_path(app).ok_or("cartella impostazioni non disponibile")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    let old = get();
    let calendar_changed = old.calendar_url != settings.calendar_url || old.calendar_source != settings.calendar_source;
    if let Ok(mut s) = SETTINGS.write() {
        *s = Some(settings.clone());
    }
    if calendar_changed {
        crate::agenda::refresh_agenda();
    }
    let _ = app.emit("settings-changed", &settings);
    crate::place_window(app);
    Ok(settings)
}

#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Vec<String> {
    app.get_webview_window("main")
        .and_then(|w| w.available_monitors().ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| m.name().cloned())
        .collect()
}

// async: creating a window from a synchronous command deadlocks on Windows.
#[tauri::command]
pub async fn open_settings(app: AppHandle) {
    open_settings_window(&app);
}

pub fn open_settings_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Impostazioni di Lumen")
        .inner_size(560.0, 720.0)
        .resizable(false)
        .maximizable(false)
        .center()
        .build();
}
