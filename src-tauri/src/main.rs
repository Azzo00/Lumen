#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agenda;
mod audio;
mod devices;
mod downloads;
mod fullscreen;
mod git;
mod hover;
mod icons;
mod launch;
mod media;
mod notify;
mod pcstats;
mod settings;
mod system;
mod updater;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, PhysicalPosition};

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch opens the settings instead of creating a second notch.
            settings::open_settings_window(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            media::media_command,
            media::media_snapshot,
            hover::set_hit_rect,
            hover::drag_start,
            system::system_snapshot,
            system::set_volume,
            system::toggle_mute,
            system::set_brightness,
            system::set_radio,
            system::take_screenshot,
            agenda::agenda_snapshot,
            agenda::refresh_agenda,
            git::git_pull_all,
            git::open_git_log,
            launch::open_app,
            launch::open_link,
            downloads::open_download,
            downloads::reveal_download,
            updater::check_update,
            updater::install_update,
            pcstats::pc_stats,
            settings::get_settings,
            settings::save_settings,
            settings::list_monitors,
            settings::open_settings,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            settings::load(&handle);

            let win = app.get_webview_window("main").expect("main window missing");
            place_window(&handle);
            hover::make_non_activating(&win);
            win.set_ignore_cursor_events(true)?;
            win.show()?;

            let open_settings = MenuItem::with_id(app, "settings", "Impostazioni…", true, None::<&str>)?;
            let reload = MenuItem::with_id(app, "reload", "Ricarica", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Esci da Lumen", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&open_settings, &reload, &sep, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("app icon missing"))
                .tooltip("Lumen")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "settings" => settings::open_settings_window(app),
                    "quit" => app.exit(0),
                    "reload" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.eval("location.reload()");
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            media::start(handle.clone());
            audio::start(handle.clone());
            system::start(handle.clone());
            devices::start(handle.clone());
            notify::start(handle.clone());
            fullscreen::start(handle.clone());
            agenda::start(handle.clone());
            downloads::start(handle.clone());
            updater::start(handle.clone());
            hover::start(handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Lumen");
}

/// The monitor chosen in the settings, or the primary one.
pub fn target_monitor(win: &tauri::WebviewWindow) -> Option<tauri::Monitor> {
    let wanted = settings::get().monitor;
    win.available_monitors()
        .ok()
        .and_then(|list| list.into_iter().find(|m| !wanted.is_empty() && m.name() == Some(&wanted)))
        .or_else(|| win.primary_monitor().ok().flatten())
}

/// Range of x positions (physical px) the notch window can take on its monitor.
pub fn x_range(win: &tauri::WebviewWindow) -> Option<(i32, i32, i32)> {
    let monitor = target_monitor(win)?;
    let size = win.outer_size().ok()?;
    let min = monitor.position().x;
    let free = (monitor.size().width as i32 - size.width as i32).max(0);
    Some((min, min + free, monitor.position().y))
}

/// Place the (transparent) notch window at the top of the chosen monitor, at the saved position.
pub fn place_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    let Some((min, max, y)) = x_range(&win) else { return };
    let x = min + ((max - min) as f64 * settings::get().position).round() as i32;
    let _ = win.set_position(PhysicalPosition::new(x, y));
}
