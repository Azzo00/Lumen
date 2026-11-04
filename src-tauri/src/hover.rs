//! The notch window is much bigger than the pill and fully transparent around it.
//! The window ignores the mouse by default (clicks fall through to whatever is
//! behind it); a small thread watches the cursor and makes the window
//! interactive only while the cursor is over the pill's rectangle, which the
//! frontend reports in CSS pixels relative to the window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

/// True while the user is dragging the notch along the top edge.
pub static DRAGGING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Deserialize)]
pub struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

static HIT_RECT: Mutex<Rect> = Mutex::new(Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });

#[tauri::command]
pub fn set_hit_rect(rect: Rect) {
    if let Ok(mut r) = HIT_RECT.lock() {
        *r = rect;
    }
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        let mut was_inside = false;
        loop {
            std::thread::sleep(Duration::from_millis(30));
            let Some(win) = app.get_webview_window("main") else { continue };
            let hidden = crate::fullscreen::HIDDEN.load(std::sync::atomic::Ordering::Relaxed);
            let inside = !hidden && (DRAGGING.load(Ordering::Relaxed) || cursor_inside(&win));
            if inside != was_inside {
                was_inside = inside;
                let _ = win.set_ignore_cursor_events(!inside);
                let _ = app.emit("hover", inside);
            }
        }
    });
}

fn cursor_inside(win: &WebviewWindow) -> bool {
    let (Ok(cursor), Ok(origin), Ok(scale)) = (win.cursor_position(), win.inner_position(), win.scale_factor())
    else {
        return false;
    };
    let Ok(r) = HIT_RECT.lock().map(|r| *r) else { return false };
    let x = (cursor.x - origin.x as f64) / scale;
    let y = (cursor.y - origin.y as f64) / scale;
    x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h
}

/// Start dragging the notch horizontally. The window follows the cursor until the
/// left mouse button is released; then the position is saved in the settings.
#[tauri::command]
pub fn drag_start(app: AppHandle) {
    if DRAGGING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        let finish = || DRAGGING.store(false, Ordering::SeqCst);
        let Some(win) = app.get_webview_window("main") else { return finish() };
        let (Ok(start_cursor), Ok(start_pos), Some((min, max, y))) =
            (win.cursor_position(), win.outer_position(), crate::x_range(&win))
        else {
            return finish();
        };
        let mut x = start_pos.x;
        loop {
            std::thread::sleep(Duration::from_millis(8));
            if let Ok(c) = win.cursor_position() {
                let nx = (start_pos.x + (c.x - start_cursor.x).round() as i32).clamp(min, max);
                if nx != x {
                    x = nx;
                    let _ = win.set_position(PhysicalPosition::new(x, y));
                }
            }
            let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
            if !down {
                break;
            }
        }
        let mut s = crate::settings::get();
        s.position = if max > min { (x - min) as f64 / (max - min) as f64 } else { 0.5 };
        // Snap to the centre when close to it.
        if (s.position - 0.5).abs() < 0.02 {
            s.position = 0.5;
        }
        let _ = crate::settings::store(&app, s);
        finish();
    });
}

/// Clicking the notch must not steal focus from the app the user is working in.
pub fn make_non_activating(win: &WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE};

    if let Ok(h) = win.hwnd() {
        let hwnd = HWND(h.0 as _);
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
        }
    }
}
