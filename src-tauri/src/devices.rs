//! Device popups: Bluetooth devices connecting/disconnecting (with battery level
//! when Windows knows it) and USB drives plugged in or removed. Emits `device`.
//! The charger is handled in the frontend (Battery API `chargingchange`).

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::core::{Interface, GUID, HSTRING, PCWSTR};
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind};
use windows::Foundation::IReference;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows_collections::IIterable;

const PROP_CONTAINER: &str = "System.Devices.Aep.ContainerId";
/// Battery level shown in Windows Settings, stored on the device's PnP nodes.
const PROP_BATTERY: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
const DRIVE_REMOVABLE: u32 = 2;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEvent {
    kind: &'static str,
    name: String,
    connected: bool,
    battery: Option<u8>,
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        // None = not sampled yet: the first sample only sets the baseline (no popups at startup).
        let mut bt_prev: Option<HashMap<String, (String, Option<GUID>)>> = None;
        let mut drives_prev: Option<HashMap<char, String>> = None;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            if !crate::settings::get().device_popups {
                bt_prev = None;
                drives_prev = None;
                continue;
            }

            let bt_now = connected_bluetooth();
            if let Some(prev) = &bt_prev {
                for (id, (name, container)) in &bt_now {
                    if !prev.contains_key(id) {
                        // Windows publishes the battery level a moment after connecting.
                        std::thread::sleep(Duration::from_millis(1500));
                        let battery = container.as_ref().and_then(battery_level);
                        emit(&app, "bluetooth", name, true, battery);
                    }
                }
                for (id, (name, _)) in prev {
                    if !bt_now.contains_key(id) {
                        emit(&app, "bluetooth", name, false, None);
                    }
                }
            }
            bt_prev = Some(bt_now);

            let drives_now = removable_drives();
            if let Some(prev) = &drives_prev {
                for (letter, label) in &drives_now {
                    if !prev.contains_key(letter) {
                        emit(&app, "usb", label, true, None);
                    }
                }
                for (letter, label) in prev {
                    if !drives_now.contains_key(letter) {
                        emit(&app, "usb", label, false, None);
                    }
                }
            }
            drives_prev = Some(drives_now);
        }
    });
}

fn emit(app: &AppHandle, kind: &'static str, name: &str, connected: bool, battery: Option<u8>) {
    let _ = app.emit("device", DeviceEvent { kind, name: name.to_string(), connected, battery });
}

fn props(keys: &[&str]) -> IIterable<HSTRING> {
    keys.iter().map(|k| HSTRING::from(*k)).collect::<Vec<_>>().into()
}

fn prop<T: windows::core::RuntimeType + 'static>(info: &DeviceInformation, key: &str) -> Option<T> {
    info.Properties().ok()?.Lookup(&HSTRING::from(key)).ok()?.cast::<IReference<T>>().ok()?.Value().ok()
}

fn find_all(aqs: &HSTRING, keys: &[&str], kind: DeviceInformationKind) -> Vec<DeviceInformation> {
    DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(aqs, &props(keys), kind)
        .and_then(|op| op.get())
        .map(|list| list.into_iter().collect())
        .unwrap_or_default()
}

/// Connected Bluetooth devices (classic and LE): id → (name, container id).
pub fn connected_bluetooth() -> HashMap<String, (String, Option<GUID>)> {
    let mut out = HashMap::new();
    let selectors = [
        BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
        BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
    ];
    for sel in selectors.into_iter().flatten() {
        for info in find_all(&sel, &[PROP_CONTAINER], DeviceInformationKind::AssociationEndpoint) {
            let (Ok(id), Ok(name)) = (info.Id(), info.Name()) else { continue };
            let name = name.to_string();
            if name.is_empty() {
                continue;
            }
            // A headset is often listed both as classic and LE: keep one entry per physical device.
            let container = prop::<GUID>(&info, PROP_CONTAINER);
            if out.values().any(|(n, c): &(String, Option<GUID>)| *n == name && *c == container) {
                continue;
            }
            out.insert(id.to_string(), (name, container));
        }
    }
    out
}

fn battery_level(container: &GUID) -> Option<u8> {
    let aqs = HSTRING::from(format!("System.Devices.ContainerId:=\"{{{container:?}}}\""));
    find_all(&aqs, &[PROP_BATTERY], DeviceInformationKind::Device)
        .iter()
        .find_map(|info| prop::<u8>(info, PROP_BATTERY))
        .filter(|level| *level <= 100)
}

/// Removable drives (USB sticks, SD cards): letter → "Label (E:)".
fn removable_drives() -> HashMap<char, String> {
    let mut out = HashMap::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(Some(0)).collect();
        if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != DRIVE_REMOVABLE {
            continue;
        }
        let mut label = [0u16; 128];
        let ok = unsafe { GetVolumeInformationW(PCWSTR(root.as_ptr()), Some(&mut label), None, None, None, None) }.is_ok();
        let len = label.iter().position(|&c| c == 0).unwrap_or(0);
        let name = if ok && len > 0 { String::from_utf16_lossy(&label[..len]) } else { "Unità USB".to_string() };
        out.insert(letter, format!("{name} ({letter}:)"));
    }
    out
}
