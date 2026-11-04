//! Opening things from the notch: the app that sent a notification, a meeting's
//! join link, a file. The notch window never takes focus (WS_EX_NOACTIVATE), but it
//! did receive the user's click, so it may hand the foreground to what it launches.

use windows::core::HSTRING;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY, SW_SHOWNORMAL};

pub fn shell_open(target: &str) {
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(target), None, None, SW_SHOWNORMAL);
    }
}

/// Open (or bring to front) an installed app by its AppUserModelID, e.g. "MSTeams_8wekyb3d8bbwe!MSTeams".
#[tauri::command]
pub async fn open_app(aumid: String) -> Result<(), String> {
    let ok = !aumid.is_empty() && aumid.chars().all(|c| c.is_ascii_alphanumeric() || "._-!{}".contains(c));
    if !ok {
        return Err("app non valida".into());
    }
    shell_open(&format!("shell:AppsFolder\\{aumid}"));
    Ok(())
}

/// Join a meeting. Teams links go straight to the Teams app instead of the browser.
#[tauri::command]
pub async fn open_link(url: String) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err("link non valido".into());
    }
    let target = match url.strip_prefix("https://teams.microsoft.com") {
        Some(rest) if rest.starts_with("/l/") || rest.starts_with("/meet/") => format!("msteams:{rest}"),
        _ => url,
    };
    shell_open(&target);
    Ok(())
}
