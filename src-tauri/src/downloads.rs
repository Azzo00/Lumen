//! "Download finished" popup: watches the user's Downloads folder (polled) and emits
//! `download` when a new file appears and has stopped growing. Browser temp files
//! (.crdownload, .part, ...) are ignored: the finished file shows up under its real name.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadEvent {
    name: String,
    path: String,
    size: u64,
    icon: Option<String>,
}

const TEMP_EXT: [&str; 7] = ["crdownload", "part", "partial", "tmp", "download", "opdownload", "!ut"];

pub fn downloads_dir() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

fn is_temp(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("~$")
        || lower.starts_with('.')
        || lower == "desktop.ini"
        || Path::new(&lower).extension().and_then(|e| e.to_str()).is_some_and(|e| TEMP_EXT.contains(&e))
}

fn list(dir: &Path) -> HashMap<String, (u64, SystemTime)> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let md = e.metadata().ok()?;
                    if !md.is_file() {
                        return None;
                    }
                    Some((e.file_name().to_string_lossy().to_string(), (md.len(), md.modified().ok()?)))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let Some(dir) = downloads_dir() else { return };
        // files already there at startup are not announced
        let mut known: HashSet<String> = list(&dir).into_keys().collect();
        // new files waiting to stop growing: name → size at the previous poll
        let mut pending: HashMap<String, u64> = HashMap::new();
        loop {
            std::thread::sleep(Duration::from_millis(1500));
            let now = list(&dir);
            for (name, &(size, modified)) in &now {
                if known.contains(name) || is_temp(name) {
                    continue;
                }
                let recent = modified.elapsed().map(|e| e < Duration::from_secs(600)).unwrap_or(true);
                match pending.get(name) {
                    // same size twice in a row and readable → finished
                    Some(&prev) if prev == size && size > 0 => {
                        let path = dir.join(name);
                        if std::fs::File::open(&path).is_err() {
                            continue;
                        }
                        pending.remove(name);
                        known.insert(name.clone());
                        if recent && crate::settings::get().download_popups {
                            let p = path.to_string_lossy().to_string();
                            let icon = crate::icons::shell_icon_data_url(&p, 64);
                            let _ = app.emit("download", DownloadEvent { name: name.clone(), path: p, size, icon });
                        }
                    }
                    _ => {
                        pending.insert(name.clone(), size);
                    }
                }
            }
            // forget files that were deleted or renamed
            known.retain(|n| now.contains_key(n));
            pending.retain(|n, _| now.contains_key(n));
        }
    });
}

/// Only files inside the Downloads folder can be opened from the popup.
fn checked(path: &str) -> Result<PathBuf, String> {
    let dir = downloads_dir().ok_or("cartella Download non trovata")?;
    let p = PathBuf::from(path);
    let ok = p.parent().and_then(|d| d.canonicalize().ok()) == dir.canonicalize().ok() && p.is_file();
    if ok { Ok(p) } else { Err("file non valido".into()) }
}

#[tauri::command]
pub async fn open_download(path: String) -> Result<(), String> {
    let p = checked(&path)?;
    crate::launch::shell_open(&p.to_string_lossy());
    Ok(())
}

#[tauri::command]
pub async fn reveal_download(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let p = checked(&path)?;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", p.display()))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
