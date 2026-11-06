//! Command Center backend: master volume, screen brightness (laptop panel via WMI),
//! Wi-Fi and Bluetooth radios. A poller emits `system` whenever the state changes,
//! including changes made outside Lumen (keyboard volume keys, Fn brightness keys...).

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::core::{BSTR, PCWSTR};
use windows::Devices::Radios::{Radio, RadioKind, RadioState};
use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Variant::{VariantClear, VariantToInt32, VARENUM, VARIANT};
use windows::Win32::System::Wmi::{IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_GENERIC_FLAG_TYPE};

#[derive(Serialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SystemState {
    volume: u32,
    muted: bool,
    /// None when the screen doesn't support software brightness (e.g. desktop monitors).
    brightness: Option<u32>,
    wifi_on: Option<bool>,
    wifi_ssid: Option<String>,
    wifi_bars: u8,
    bt_on: Option<bool>,
    bt_devices: Vec<String>,
    /// Windows apps use the light theme (Settings > Personalization > Colors).
    light_theme: bool,
}

static LATEST: Mutex<Option<SystemState>> = Mutex::new(None);

/// HKCU\...\Themes\Personalize\AppsUseLightTheme (missing = dark).
fn apps_use_light_theme() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let res = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    res.is_ok() && value == 1
}
static BRIGHTNESS: Mutex<Option<u32>> = Mutex::new(None);
static BRIGHTNESS_TX: OnceLock<Sender<u32>> = OnceLock::new();

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// Runs `f` on a fresh MTA thread: WinRT `.get()` must not block Tauri's main thread.
fn in_background(f: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        com_init();
        f();
    });
}

/* ------------------------------------------------------------------ */
/* polling                                                             */
/* ------------------------------------------------------------------ */

pub fn start(app: AppHandle) {
    start_brightness_worker();
    std::thread::spawn(move || {
        com_init();
        let mut state = SystemState::default();
        let mut last_sent: Option<SystemState> = None;
        let mut last_slow = Instant::now() - Duration::from_secs(60);
        loop {
            // volume is cheap and should feel instant
            if let Ok((v, m)) = read_volume() {
                state.volume = v;
                state.muted = m;
            }
            state.brightness = BRIGHTNESS.lock().ok().and_then(|b| *b);
            // radios, Wi-Fi and Bluetooth devices change rarely
            if last_slow.elapsed() > Duration::from_secs(3) {
                last_slow = Instant::now();
                read_radios(&mut state);
                read_wifi(&mut state);
                state.light_theme = apps_use_light_theme();
                state.bt_devices = if state.bt_on == Some(true) { read_bt_devices() } else { Vec::new() };
            }
            if last_sent.as_ref() != Some(&state) {
                let _ = app.emit("system", &state);
                if let Ok(mut l) = LATEST.lock() {
                    *l = Some(state.clone());
                }
                last_sent = Some(state.clone());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

/// Opens Windows' snipping overlay (same as Win+Shift+S), after a short pause so the
/// notch has time to shrink back and doesn't end up in the capture.
#[tauri::command]
pub async fn take_screenshot() {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    std::thread::sleep(Duration::from_millis(350));
    unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::w!("ms-screenclip:"),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[tauri::command]
pub fn system_snapshot() -> Option<SystemState> {
    LATEST.lock().ok().and_then(|l| l.clone())
}

/* ------------------------------------------------------------------ */
/* volume                                                              */
/* ------------------------------------------------------------------ */

fn endpoint_volume() -> windows::core::Result<IAudioEndpointVolume> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?.Activate(CLSCTX_ALL, None)
    }
}

fn read_volume() -> windows::core::Result<(u32, bool)> {
    let ep = endpoint_volume()?;
    unsafe {
        let level = ep.GetMasterVolumeLevelScalar()?;
        let muted = ep.GetMute()?.as_bool();
        Ok(((level * 100.0).round() as u32, muted))
    }
}

#[tauri::command]
pub fn set_volume(level: u32) {
    in_background(move || {
        if let Ok(ep) = endpoint_volume() {
            unsafe {
                let _ = ep.SetMasterVolumeLevelScalar(level.min(100) as f32 / 100.0, std::ptr::null());
                if level > 0 {
                    let _ = ep.SetMute(false, std::ptr::null());
                }
            }
        }
    });
}

#[tauri::command]
pub fn toggle_mute() {
    in_background(|| {
        if let Ok(ep) = endpoint_volume() {
            unsafe {
                if let Ok(m) = ep.GetMute() {
                    let _ = ep.SetMute(!m.as_bool(), std::ptr::null());
                }
            }
        }
    });
}

/* ------------------------------------------------------------------ */
/* radios, Wi-Fi, Bluetooth                                            */
/* ------------------------------------------------------------------ */

fn radios() -> windows::core::Result<Vec<Radio>> {
    let list = Radio::GetRadiosAsync()?.get()?;
    (0..list.Size()?).map(|i| list.GetAt(i)).collect()
}

fn read_radios(state: &mut SystemState) {
    state.wifi_on = None;
    state.bt_on = None;
    for r in radios().unwrap_or_default() {
        let on = r.State().map(|s| s == RadioState::On).ok();
        match r.Kind() {
            Ok(RadioKind::WiFi) => state.wifi_on = on,
            Ok(RadioKind::Bluetooth) => state.bt_on = on,
            _ => {}
        }
    }
}

fn read_wifi(state: &mut SystemState) {
    state.wifi_ssid = None;
    state.wifi_bars = 0;
    let Ok(profiles) = NetworkInformation::GetConnectionProfiles() else { return };
    for p in profiles {
        let wlan = p.IsWlanConnectionProfile().unwrap_or(false);
        let connected = p
            .GetNetworkConnectivityLevel()
            .map(|l| l != NetworkConnectivityLevel::None)
            .unwrap_or(false);
        if !(wlan && connected) {
            continue;
        }
        state.wifi_ssid = p
            .WlanConnectionProfileDetails()
            .and_then(|d| d.GetConnectedSsid())
            .map(|s| s.to_string())
            .ok()
            .filter(|s| !s.is_empty());
        state.wifi_bars = p.GetSignalBars().and_then(|b| b.Value()).unwrap_or(0);
        return;
    }
}

fn read_bt_devices() -> Vec<String> {
    let mut names: Vec<String> = crate::devices::connected_bluetooth().into_values().map(|(n, _)| n).collect();
    names.sort();
    names.dedup();
    names
}

#[tauri::command]
pub fn set_radio(kind: String, on: bool) {
    in_background(move || {
        let want = match kind.as_str() {
            "wifi" => RadioKind::WiFi,
            "bluetooth" => RadioKind::Bluetooth,
            _ => return,
        };
        // Desktop apps are allowed by default, but access must be requested once.
        let _ = Radio::RequestAccessAsync().and_then(|op| op.get());
        for r in radios().unwrap_or_default() {
            if r.Kind().ok() == Some(want) {
                let target = if on { RadioState::On } else { RadioState::Off };
                let _ = r.SetStateAsync(target).and_then(|op| op.get());
            }
        }
    });
}

/* ------------------------------------------------------------------ */
/* brightness (WMI: works for laptop panels, not for external monitors) */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub fn set_brightness(level: u32) {
    if let Some(tx) = BRIGHTNESS_TX.get() {
        let _ = tx.send(level.min(100));
    }
}

fn start_brightness_worker() {
    let (tx, rx) = channel::<u32>();
    let _ = BRIGHTNESS_TX.set(tx);
    std::thread::spawn(move || {
        com_init();
        let Ok(svc) = (unsafe { wmi_connect() }) else { return };
        let set_params = unsafe { wmi_set_brightness_params(&svc) };
        loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(mut level) => {
                    // a slider drag sends many values: only the latest one matters
                    while let Ok(newer) = rx.try_recv() {
                        level = newer;
                    }
                    if let Some(p) = &set_params {
                        unsafe { wmi_set_brightness(&svc, p, level) };
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let current = unsafe { wmi_read_brightness(&svc) };
            if let Ok(mut b) = BRIGHTNESS.lock() {
                *b = current;
            }
        }
    });
}

unsafe fn wmi_connect() -> windows::core::Result<IWbemServices> {
    let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
    let empty = BSTR::new();
    let svc = locator.ConnectServer(&BSTR::from("ROOT\\WMI"), &empty, &empty, &empty, 0, &empty, None)?;
    // Without an explicit proxy blanket, method calls (the brightness write) are silently ignored.
    // 10 = RPC_C_AUTHN_WINNT, 0 = RPC_C_AUTHZ_NONE
    CoSetProxyBlanket(&svc, 10, 0, PCWSTR::null(), RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE)?;
    Ok(svc)
}

unsafe fn wmi_first(svc: &IWbemServices, query: &str) -> Option<IWbemClassObject> {
    let rows = svc
        .ExecQuery(&BSTR::from("WQL"), &BSTR::from(query), WBEM_GENERIC_FLAG_TYPE(0), None)
        .ok()?;
    let mut row = [None::<IWbemClassObject>; 1];
    let mut returned = 0u32;
    rows.Next(-1, &mut row, &mut returned).ok().ok()?;
    if returned == 0 {
        return None;
    }
    row[0].take()
}

unsafe fn wmi_read_brightness(svc: &IWbemServices) -> Option<u32> {
    let obj = wmi_first(svc, "SELECT CurrentBrightness FROM WmiMonitorBrightness")?;
    let mut v = VARIANT::default();
    obj.Get(windows::core::w!("CurrentBrightness"), 0, &mut v, None, None).ok()?;
    let level = VariantToInt32(&v).ok();
    let _ = VariantClear(&mut v);
    level.map(|l| l.clamp(0, 100) as u32)
}

/// Input-parameter class of WmiMonitorBrightnessMethods.WmiSetBrightness.
/// GetMethod only works on the class definition, not on an instance.
unsafe fn wmi_set_brightness_params(svc: &IWbemServices) -> Option<IWbemClassObject> {
    let mut class_def: Option<IWbemClassObject> = None;
    svc.GetObject(
        &BSTR::from("WmiMonitorBrightnessMethods"),
        WBEM_GENERIC_FLAG_TYPE(0),
        None,
        Some(&mut class_def),
        None,
    )
    .ok()?;
    let mut in_params: Option<IWbemClassObject> = None;
    class_def?
        .GetMethod(windows::core::w!("WmiSetBrightness"), 0, &mut in_params, std::ptr::null_mut())
        .ok()?;
    in_params
}

unsafe fn wmi_set_brightness(svc: &IWbemServices, params_class: &IWbemClassObject, level: u32) {
    let Some(instance) = wmi_first(svc, "SELECT * FROM WmiMonitorBrightnessMethods") else { return };
    let mut path = VARIANT::default();
    if instance.Get(windows::core::w!("__RELPATH"), 0, &mut path, None, None).is_err() {
        return;
    }
    let relpath = path.Anonymous.Anonymous.Anonymous.bstrVal.to_string();
    let _ = VariantClear(&mut path);
    let Ok(params) = params_class.SpawnInstance(0) else { return };

    let mut brightness = VARIANT::default();
    {
        let inner = &mut *brightness.Anonymous.Anonymous;
        inner.vt = VARENUM(17); // VT_UI1
        inner.Anonymous.bVal = level.min(100) as u8;
    }
    let mut timeout = VARIANT::default();
    {
        let inner = &mut *timeout.Anonymous.Anonymous;
        inner.vt = VARENUM(3); // VT_I4
        inner.Anonymous.lVal = 0;
    }
    let _ = params.Put(windows::core::w!("Brightness"), 0, &brightness, 0);
    let _ = params.Put(windows::core::w!("Timeout"), 0, &timeout, 0);

    let _ = svc.ExecMethod(
        &BSTR::from(relpath),
        &BSTR::from("WmiSetBrightness"),
        WBEM_GENERIC_FLAG_TYPE(0),
        None,
        &params,
        None,
        None,
    );
}
