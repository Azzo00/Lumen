//! Hides the notch while a fullscreen app (game, video, presentation) is in the
//! foreground on the notch's monitor, and shows it again afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewWindow};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, GWL_STYLE, WS_CAPTION,
};

/// True while the notch is hidden because of a fullscreen app.
pub static HIDDEN: AtomicBool = AtomicBool::new(false);

pub fn start(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(700));
        let Some(win) = app.get_webview_window("main") else { continue };
        let hide = crate::settings::get().hide_fullscreen && unsafe { fullscreen_in_front(&win) };
        if hide != HIDDEN.load(Ordering::Relaxed) {
            HIDDEN.store(hide, Ordering::Relaxed);
            if hide {
                let _ = win.set_ignore_cursor_events(true);
                let _ = win.hide();
            } else {
                let _ = win.show();
            }
        }
    });
}

unsafe fn fullscreen_in_front(win: &WebviewWindow) -> bool {
    let Ok(h) = win.hwnd() else { return false };
    let ours = HWND(h.0 as _);
    let fg = GetForegroundWindow();
    if fg.0.is_null() || fg == ours {
        return false;
    }

    // The desktop and the taskbar cover the whole screen too: never count them.
    let mut class = [0u16; 64];
    let len = GetClassNameW(fg, &mut class) as usize;
    let class = String::from_utf16_lossy(&class[..len]);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return false;
    }

    // Only react to apps on the same monitor as the notch.
    let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
    if monitor != MonitorFromWindow(ours, MONITOR_DEFAULTTONEAREST) {
        return false;
    }

    // Windows' own signal: exclusive D3D games, presentations, "busy" fullscreen apps.
    if let Ok(state) = SHQueryUserNotificationState() {
        if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE || state == QUNS_BUSY {
            return true;
        }
    }

    // Borderless fullscreen (browsers in F11, video players, most modern games):
    // a window without a title bar that covers the whole monitor.
    let mut rect = RECT::default();
    if GetWindowRect(fg, &mut rect).is_err() {
        return false;
    }
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(monitor, &mut info).as_bool() {
        return false;
    }
    let m = info.rcMonitor;
    let covers = rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom;
    let has_caption = (GetWindowLongW(fg, GWL_STYLE) as u32 & WS_CAPTION.0) == WS_CAPTION.0;
    covers && !has_caption
}
