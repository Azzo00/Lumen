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
    /// "auto" (follows Windows), "dark", "oled", "glass", "light", "custom".
    pub theme: String,
    /// Colour and opacity (0.3 – 1) of the pill with the "custom" theme.
    pub custom_color: String,
    pub custom_opacity: f64,
    /// Blurred album cover as the background of the music view.
    pub cover_blur: bool,
    /// Animated background with 2-3 colours taken from the album cover.
    pub cover_gradient: bool,
    /// Coloured light under the notch that pulses with the bass.
    pub bass_glow: bool,
    /// Round cover spinning like a record while the music plays.
    pub vinyl: bool,
    /// Notch attached to the top edge of the screen (square top, concave "ears")
    /// instead of a pill floating a few pixels below it.
    pub docked: bool,
    /// "bars" or "ring" (wave around the cover).
    pub visualizer: String,
    /// Size multiplier of the pill (0.85 – 1.25).
    pub scale: f64,
    /// Corner roundness: 0 = squarer, 0.5 = default, 1 = as round as possible.
    pub roundness: f64,
    /// Monitor name, or "" for the primary monitor.
    pub monitor: String,
    /// Horizontal position along the top edge: 0 = left, 0.5 = centre, 1 = right.
    pub position: f64,
    pub hide_fullscreen: bool,
    /// Keep the notch (and its popups) out of screen sharing and screenshots:
    /// still visible on the PC, invisible to whoever watches a Teams/Slack share.
    pub hide_in_share: bool,
    /// Pause the music while a Teams/Slack call is using the microphone.
    pub pause_on_calls: bool,
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
            theme: "auto".into(),
            custom_color: "#1e2a3a".into(),
            custom_opacity: 0.9,
            cover_blur: true,
            cover_gradient: true,
            bass_glow: true,
            vinyl: false,
            docked: true,
            visualizer: "bars".into(),
            scale: 1.0,
            roundness: 0.5,
            monitor: String::new(),
            position: 0.5,
            hide_fullscreen: true,
            hide_in_share: true,
            pause_on_calls: true,
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
        if !self.roundness.is_finite() {
            self.roundness = 0.5;
        }
        self.roundness = self.roundness.clamp(0.0, 1.0);
        if !self.position.is_finite() {
            self.position = 0.5;
        }
        self.position = self.position.clamp(0.0, 1.0);
        if !["auto", "dark", "oled", "glass", "light", "custom"].contains(&self.theme.as_str()) {
            self.theme = "auto".into();
        }
        if !self.custom_opacity.is_finite() {
            self.custom_opacity = 0.9;
        }
        self.custom_opacity = self.custom_opacity.clamp(0.3, 1.0);
        let hex = self.custom_color.trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            self.custom_color = "#1e2a3a".into();
        }
        if self.visualizer != "ring" {
            self.visualizer = "bars".into();
        }
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
    crate::apply_share_privacy(app);
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
