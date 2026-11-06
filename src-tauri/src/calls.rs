//! Pauses the music when a Teams or Slack call starts and resumes it when the call ends.
//!
//! A call is detected from the microphone: Windows lists every app that is capturing
//! audio right now (WASAPI capture sessions), so "Teams or Slack is recording from a
//! microphone" means a call, a huddle or the pre-join screen. No admin rights, no
//! window titles, works with every language and Teams/Slack version.
//! Only the players Lumen paused itself are resumed, and only if they are still paused.

use std::collections::HashMap;
use std::time::Duration;

use sysinfo::{Pid, ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter};
use windows::core::Interface;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus;
use windows::Win32::Media::Audio::{
    eCapture, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// Executables of the call apps (new Teams, classic Teams, Slack).
const CALL_APPS: [&str; 3] = ["ms-teams.exe", "teams.exe", "slack.exe"];
const POLL: Duration = Duration::from_secs(2);
/// Consecutive polls needed to change state: start after ~4 s, end after ~6 s, so a
/// short glitch (device switch, reconnect) does not toggle the music.
const START_POLLS: u32 = 2;
const END_POLLS: u32 = 3;

/// Process ids currently capturing from any active microphone.
unsafe fn capturing_pids() -> windows::core::Result<Vec<u32>> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
    let mut pids = Vec::new();
    for d in 0..devices.GetCount()? {
        let Ok(device) = devices.Item(d) else { continue };
        let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
        let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
        for s in 0..sessions.GetCount().unwrap_or(0) {
            let Ok(control) = sessions.GetSession(s) else { continue };
            if control.GetState().ok() != Some(AudioSessionStateActive) {
                continue;
            }
            if let Ok(pid) = control.cast::<IAudioSessionControl2>().and_then(|c| c.GetProcessId()) {
                pids.push(pid);
            }
        }
    }
    Ok(pids)
}

/// Name of the call app that owns `pid` (the process itself or one of its parents:
/// Teams and Slack record from helper processes).
fn call_app(sys: &System, pid: u32) -> Option<String> {
    let mut cur = sys.process(Pid::from_u32(pid));
    for _ in 0..4 {
        let p = cur?;
        let name = p.name().to_string_lossy().to_lowercase();
        if CALL_APPS.contains(&name.as_str()) {
            return Some(name);
        }
        cur = p.parent().and_then(|pp| sys.process(pp));
    }
    None
}

/// The call app currently using a microphone, if any.
pub fn active_call(sys: &mut System) -> Option<String> {
    let pids = unsafe { capturing_pids() }.unwrap_or_default();
    if pids.is_empty() {
        return None;
    }
    sys.refresh_processes(ProcessesToUpdate::All, true);
    pids.into_iter().find_map(|pid| call_app(sys, pid))
}

/// Pauses every playing player; returns their app ids.
fn pause_players() -> Vec<String> {
    let mut paused = Vec::new();
    let Ok(manager) = crate::media::request_manager() else { return paused };
    let Ok(sessions) = manager.GetSessions() else { return paused };
    for session in sessions {
        let playing = session
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .is_ok_and(|s| s == PlaybackStatus::Playing);
        if !playing {
            continue;
        }
        let id = session.SourceAppUserModelId().map(|s| s.to_string()).unwrap_or_default();
        if session.TryPauseAsync().and_then(|op| op.get()).unwrap_or(false) {
            paused.push(id);
        }
    }
    paused
}

/// Resumes the players paused by `pause_players`, if they are still paused.
fn resume_players(ids: &[String]) {
    let Ok(manager) = crate::media::request_manager() else { return };
    let Ok(sessions) = manager.GetSessions() else { return };
    for session in sessions {
        let id = session.SourceAppUserModelId().map(|s| s.to_string()).unwrap_or_default();
        if !ids.contains(&id) {
            continue;
        }
        let paused = session
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .is_ok_and(|s| s == PlaybackStatus::Paused);
        if paused {
            let _ = session.TryPlayAsync().and_then(|op| op.get());
        }
    }
}

/// Debounced call state: `step` returns Some(true) when a call starts, Some(false)
/// when it ends.
#[derive(Default)]
struct Debounce {
    in_call: bool,
    streak: u32,
}

impl Debounce {
    fn step(&mut self, seen: bool) -> Option<bool> {
        if seen == self.in_call {
            self.streak = 0;
            return None;
        }
        self.streak += 1;
        let needed = if seen { START_POLLS } else { END_POLLS };
        if self.streak < needed {
            return None;
        }
        self.streak = 0;
        self.in_call = seen;
        Some(seen)
    }
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        crate::media::com_init();
        let mut sys = System::new();
        let mut state = Debounce::default();
        let mut paused: Vec<String> = Vec::new();
        let mut app_name = String::new();
        loop {
            std::thread::sleep(POLL);
            let call = active_call(&mut sys);
            if let Some(n) = &call {
                app_name = n.clone();
            }
            match state.step(call.is_some()) {
                Some(true) => {
                    if crate::settings::get().pause_on_calls {
                        paused = pause_players();
                    }
                    let _ = app.emit("call", HashMap::from([("active", "true"), ("app", app_name.as_str())]));
                }
                Some(false) => {
                    resume_players(&paused);
                    paused.clear();
                    let _ = app.emit("call", HashMap::from([("active", "false"), ("app", app_name.as_str())]));
                }
                None => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_state_is_debounced() {
        let mut d = Debounce::default();
        assert_eq!(d.step(true), None);
        assert_eq!(d.step(false), None); // glitch: back to zero
        assert_eq!(d.step(true), None);
        assert_eq!(d.step(true), Some(true));
        assert_eq!(d.step(true), None);
        assert_eq!(d.step(false), None);
        assert_eq!(d.step(false), None);
        assert_eq!(d.step(false), Some(false));
        assert_eq!(d.step(false), None);
    }

    /// Opens the default microphone and checks that this process is seen as capturing.
    #[test]
    fn sees_a_process_recording() {
        use windows::Win32::Media::Audio::{eConsole, IAudioClient, AUDCLNT_SHAREMODE_SHARED};
        crate::media::com_init();
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            let Ok(device) = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) else {
                eprintln!("no microphone: skipped");
                return;
            };
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).unwrap();
            let fmt = client.GetMixFormat().unwrap();
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, fmt, None).unwrap();
            client.Start().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            let pids = capturing_pids().unwrap();
            client.Stop().unwrap();
            assert!(pids.contains(&std::process::id()), "{pids:?}");
        }
    }

    /// Prints which app is using the microphone right now (manual check).
    #[test]
    #[ignore]
    fn show_capturing_apps() {
        crate::media::com_init();
        let mut sys = System::new_all();
        let pids = unsafe { capturing_pids() }.unwrap();
        for pid in &pids {
            let name = sys.process(Pid::from_u32(*pid)).map(|p| p.name().to_string_lossy().to_string());
            println!("capturing: pid {pid} {name:?} -> call app {:?}", call_app(&sys, *pid));
        }
        println!("active call: {:?}", active_call(&mut sys));
    }
}
