//! "Mensola": files dropped on the notch stay there until they are dragged out again
//! (into a mail, a chat, another folder) or removed. The shelf only keeps the path:
//! the original file is never moved or copied. The list survives restarts
//! (shelf.json in the app config folder); files that no longer exist are dropped.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

const MAX_ITEMS: usize = 30;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u32,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub icon: Option<String>,
}

static ITEMS: Mutex<Vec<Item>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn store_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("shelf.json"))
}

fn make_item(path: &Path) -> Option<Item> {
    let meta = std::fs::metadata(path).ok()?;
    let name = path.file_name()?.to_string_lossy().to_string();
    let path_s = path.to_string_lossy().to_string();
    Some(Item {
        id: NEXT_ID.fetch_add(1, Ordering::SeqCst),
        icon: crate::icons::shell_icon_data_url(&path_s, 48),
        path: path_s,
        name,
        size: if meta.is_dir() { 0 } else { meta.len() },
        is_dir: meta.is_dir(),
    })
}

/// Current items; files deleted or moved meanwhile are removed.
pub fn items() -> Vec<Item> {
    let Ok(mut items) = ITEMS.lock() else { return vec![] };
    items.retain(|i| Path::new(&i.path).exists());
    items.clone()
}

pub fn item(id: u32) -> Option<Item> {
    items().into_iter().find(|i| i.id == id)
}

fn changed(app: &AppHandle) {
    let list = items();
    if let Some(p) = store_path(app) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let paths: Vec<&str> = list.iter().map(|i| i.path.as_str()).collect();
        let _ = std::fs::write(p, serde_json::to_string(&paths).unwrap_or_default());
    }
    let _ = app.emit("shelf", list);
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        let Some(p) = store_path(&app) else { return };
        let Ok(text) = std::fs::read_to_string(p) else { return };
        let paths: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
        let loaded: Vec<Item> = paths.iter().filter_map(|p| make_item(Path::new(p))).collect();
        if let Ok(mut items) = ITEMS.lock() {
            *items = loaded;
        }
        changed(&app);
    });
}

/// Adds files or folders (already on the shelf: ignored). Returns the new list.
pub fn add_paths(app: &AppHandle, paths: &[PathBuf]) -> Vec<Item> {
    let new: Vec<Item> = {
        let existing = items();
        paths
            .iter()
            .filter(|p| !existing.iter().any(|i| Path::new(&i.path) == p.as_path()))
            .filter_map(|p| make_item(p))
            .collect()
    };
    if let Ok(mut items) = ITEMS.lock() {
        items.extend(new);
        let extra = items.len().saturating_sub(MAX_ITEMS);
        items.drain(..extra);
    }
    changed(app);
    items()
}

#[tauri::command]
pub fn shelf_list() -> Vec<Item> {
    items()
}

#[tauri::command]
pub async fn shelf_add(app: AppHandle, paths: Vec<String>) -> Vec<Item> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    add_paths(&app, &paths)
}

#[tauri::command]
pub fn shelf_remove(app: AppHandle, id: u32) {
    if let Ok(mut items) = ITEMS.lock() {
        items.retain(|i| i.id != id);
    }
    changed(&app);
}

#[tauri::command]
pub fn shelf_clear(app: AppHandle) {
    if let Ok(mut items) = ITEMS.lock() {
        items.clear();
    }
    changed(&app);
}

#[tauri::command]
pub fn shelf_open(id: u32) -> Result<(), String> {
    let it = item(id).ok_or("file non trovato")?;
    crate::launch::shell_open(&it.path);
    Ok(())
}

#[tauri::command]
pub fn shelf_reveal(id: u32) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let it = item(id).ok_or("file non trovato")?;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", it.path))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Starts a real Windows drag of the file (like dragging it from Explorer): it can be
/// dropped into Outlook, Teams, a browser upload or a folder (copy, never move).
#[tauri::command]
pub fn shelf_drag(app: AppHandle, window: WebviewWindow, id: u32) -> Result<(), String> {
    let it = item(id).ok_or("file non trovato")?;
    let image = crate::icons::shell_icon_png(&it.path, 64).unwrap_or_default();
    app.run_on_main_thread(move || {
        let _ = drag::start_drag(
            &window,
            drag::DragItem::Files(vec![PathBuf::from(&it.path)]),
            drag::Image::Raw(image),
            |_, _| {},
            drag::Options::default(),
        );
    })
    .map_err(|e| e.to_string())
}

/// Puts a file on the shelf without an app (tests).
#[cfg(test)]
pub fn test_put(path: &Path) -> u32 {
    let it = make_item(path).unwrap();
    let id = it.id;
    ITEMS.lock().unwrap().push(it);
    id
}
