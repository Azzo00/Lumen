//! Mirrors new Windows toast notifications (Outlook, Teams, Slack...) into the notch,
//! with the app's own icon (the one shown in the Start menu).
//! Uses UserNotificationListener, polled: its change event needs package identity,
//! which an unpackaged app doesn't have. Windows' own banner is not suppressed.
//!
//! Classic Outlook is started invisibly by the calendar reader (agenda.rs); anything it
//! notifies while doing so would duplicate the new Outlook, so it is ignored.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::Foundation::Size;
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Until this time (unix ms) notifications from classic Outlook are ignored.
pub static IGNORE_CLASSIC_OUTLOOK_UNTIL: AtomicI64 = AtomicI64::new(0);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEvent {
    app: String,
    /// AppUserModelID, used to open the app when the toast is clicked.
    aumid: String,
    title: String,
    body: String,
    icon: Option<String>,
}

fn is_classic_outlook(aumid: &str) -> bool {
    aumid.to_ascii_uppercase().contains("OUTLOOK.EXE")
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let Ok(listener) = UserNotificationListener::Current() else { return };
        if listener.GetAccessStatus().ok() != Some(UserNotificationListenerAccessStatus::Allowed) {
            let _ = listener.RequestAccessAsync().and_then(|op| op.get());
        }

        // None = baseline not taken yet: notifications already present at startup are not shown.
        let mut seen: Option<HashSet<u32>> = None;
        // app identity per AUMID: (display name, icon)
        let mut apps_info: HashMap<String, (String, Option<String>)> = HashMap::new();
        // classic Outlook notifications raised while Lumen was reading its calendar
        let mut ignored: HashSet<u32> = HashSet::new();
        loop {
            std::thread::sleep(Duration::from_millis(1500));
            let settings = crate::settings::get();
            if !settings.notifications {
                seen = None;
                continue;
            }
            let Ok(list) = listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()) else {
                continue;
            };
            let now = chrono::Utc::now().timestamp_millis();
            let ignoring_outlook = now < IGNORE_CLASSIC_OUTLOOK_UNTIL.load(Ordering::Relaxed);
            let mut current = HashSet::new();
            for n in list {
                let Ok(id) = n.Id() else { continue };
                current.insert(id);
                let Some(aumid) = n.AppInfo().and_then(|i| i.AppUserModelId()).ok().map(|s| s.to_string()) else {
                    continue;
                };
                let is_new = seen.as_ref().is_some_and(|s| !s.contains(&id));
                if is_new && ignoring_outlook && is_classic_outlook(&aumid) {
                    ignored.insert(id);
                }
                if !is_new || ignored.contains(&id) {
                    continue;
                }
                if !apps_info.contains_key(&aumid) {
                    if let Some(info) = identify(&n, &aumid) {
                        apps_info.insert(aumid.clone(), info);
                    }
                }
                {
                    if let Some(ev) = describe(&n, &aumid, settings.notification_preview, &apps_info) {
                        let _ = app.emit("notification", ev);
                    }
                }
            }
            ignored.retain(|id| current.contains(id));
            seen = Some(current);
        }
    });
}

/// App name and icon. The icon is the one of the Start menu (shell:AppsFolder), which is
/// the real app icon for both Store apps (new Outlook, Teams) and classic programs; the
/// notification's own logo is the fallback.
fn identify(n: &UserNotification, aumid: &str) -> Option<(String, Option<String>)> {
    let display = n.AppInfo().ok()?.DisplayInfo().ok()?;
    let name = display.DisplayName().ok()?.to_string();
    let icon = crate::icons::app_icon_data_url(aumid, 64).or_else(|| {
        display
            .GetLogo(Size { Width: 64.0, Height: 64.0 })
            .ok()
            .and_then(|logo| windows::core::Interface::cast(&logo).ok())
            .and_then(|r| crate::media::stream_to_data_url(&r))
    });
    Some((name, icon))
}

fn describe(
    n: &UserNotification,
    aumid: &str,
    preview: bool,
    apps_info: &HashMap<String, (String, Option<String>)>,
) -> Option<NotificationEvent> {
    let (app, icon) = apps_info.get(aumid)?.clone();

    let mut texts: Vec<String> = Vec::new();
    if let Ok(binding) = n
        .Notification()
        .and_then(|t| t.Visual())
        .and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric()?))
    {
        if let Ok(elements) = binding.GetTextElements() {
            texts = elements
                .into_iter()
                .filter_map(|e| e.Text().ok().map(|t| t.to_string()))
                .filter(|t| !t.trim().is_empty())
                .collect();
        }
    }

    let (title, body) = if preview {
        let title = texts.first().cloned().unwrap_or_else(|| "Nuova notifica".into());
        (title, texts.iter().skip(1).cloned().collect::<Vec<_>>().join(" · "))
    } else {
        ("Nuova notifica".into(), String::new())
    };
    Some(NotificationEvent { app, aumid: aumid.to_string(), title, body, icon })
}
