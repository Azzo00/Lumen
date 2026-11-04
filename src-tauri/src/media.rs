//! Current media session (Spotify, browsers, Media Player...) through the
//! Windows "Global System Media Transport Controls" WinRT API.
//! A worker thread polls the session and emits `media` only when something changed.

use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::core::Interface;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as MediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IInputStream, IRandomAccessStreamReference};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// 100 ns ticks between 1601-01-01 (Windows epoch) and 1970-01-01 (Unix epoch).
const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

#[derive(Serialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    active: bool,
    title: String,
    artist: String,
    app: String,
    playing: bool,
    position_ms: i64,
    duration_ms: i64,
    updated_ms: i64,
    art: Option<String>,
}

/// Last state sent, so a freshly (re)loaded page can ask for it instead of
/// waiting for the next change.
static LATEST: Mutex<Option<MediaInfo>> = Mutex::new(None);

#[tauri::command]
pub fn media_snapshot() -> Option<MediaInfo> {
    LATEST.lock().ok().and_then(|l| l.clone())
}

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn request_manager() -> windows::core::Result<SessionManager> {
    SessionManager::RequestAsync().and_then(|op| op.get())
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        com_init();
        let mut manager: Option<SessionManager> = None;
        let mut last: Option<MediaInfo> = None;
        let mut art_key = String::new();
        let mut art: Option<String> = None;

        loop {
            if manager.is_none() {
                manager = request_manager().ok();
            }
            let info = match &manager {
                Some(m) => read_session(m, &mut art_key, &mut art).unwrap_or_else(|e| {
                    eprintln!("[media] {e:?}");
                    manager = None; // re-request on the next round
                    MediaInfo::default()
                }),
                None => MediaInfo::default(),
            };
            if last.as_ref() != Some(&info) {
                let _ = app.emit("media", &info);
                if let Ok(mut l) = LATEST.lock() {
                    *l = Some(info.clone());
                }
                last = Some(info);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

fn read_session(manager: &SessionManager, art_key: &mut String, art: &mut Option<String>) -> windows::core::Result<MediaInfo> {
    // No current session is reported as an error by the projection.
    let Ok(session) = manager.GetCurrentSession() else {
        return Ok(MediaInfo::default());
    };
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();
    let app = session.SourceAppUserModelId()?.to_string();
    let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing;

    let timeline = session.GetTimelineProperties()?;
    let position_ms = timeline.Position()?.Duration / 10_000;
    let duration_ms = (timeline.EndTime()?.Duration - timeline.StartTime()?.Duration) / 10_000;
    let updated_ms = (timeline.LastUpdatedTime()?.UniversalTime - EPOCH_DIFF_TICKS) / 10_000;

    // The cover is only read again when the track changes, or while it is still missing
    // (players often publish the thumbnail a moment after the title).
    let key = format!("{app}|{title}|{artist}");
    if *art_key != key || art.is_none() {
        *art_key = key;
        *art = read_cover(&props);
    }

    Ok(MediaInfo {
        active: true,
        title,
        artist,
        app,
        playing,
        position_ms,
        duration_ms: duration_ms.max(0),
        updated_ms,
        art: art.clone(),
    })
}

fn read_cover(props: &MediaProperties) -> Option<String> {
    stream_to_data_url(&props.Thumbnail().ok()?)
}

/// Reads a WinRT image reference (album cover, app logo) into a `data:` URL.
pub fn stream_to_data_url(reference: &IRandomAccessStreamReference) -> Option<String> {
    let stream = reference.OpenReadAsync().ok()?.get().ok()?;
    let size = u32::try_from(stream.Size().ok()?).ok()?;
    if size == 0 {
        return None;
    }
    let input: IInputStream = stream.cast().ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    reader.LoadAsync(size).ok()?.get().ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    let mime = stream.ContentType().map(|s| s.to_string()).unwrap_or_default();
    let mime = if mime.is_empty() { "image/png".to_string() } else { mime };
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)))
}

#[tauri::command]
pub fn media_command(cmd: String, position_ms: Option<i64>) {
    std::thread::spawn(move || {
        com_init();
        let Ok(manager) = request_manager() else { return };
        let Ok(session) = manager.GetCurrentSession() else { return };
        let _ = match cmd.as_str() {
            "toggle" => session.TryTogglePlayPauseAsync().and_then(|op| op.get()),
            "next" => session.TrySkipNextAsync().and_then(|op| op.get()),
            "prev" => session.TrySkipPreviousAsync().and_then(|op| op.get()),
            // TimeSpan ticks are 100 ns
            "seek" => match position_ms {
                Some(ms) => session.TryChangePlaybackPositionAsync(ms.max(0) * 10_000).and_then(|op| op.get()),
                None => return,
            },
            _ => return,
        };
    });
}
