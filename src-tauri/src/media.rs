//! Current media session (Spotify, browsers, Media Player...) through the
//! Windows "Global System Media Transport Controls" WinRT API.
//! A worker thread polls the session and emits `media` only when something changed.

use std::sync::Mutex;
use std::time::{Duration, Instant};

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
        let mut art = ArtCache::default();

        loop {
            if manager.is_none() {
                manager = request_manager().ok();
            }
            let info = match &manager {
                Some(m) => read_session(m, &mut art).unwrap_or_else(|e| {
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

/// Cover of the current track. Players (browsers above all: YouTube, Twitch) publish
/// the new title first and the new thumbnail a moment later, and when several players
/// are open the session switch can report the previous thumbnail for a while. So the
/// cover is read again on every poll for a few seconds after each change, then every
/// 15 s, and the latest one wins.
#[derive(Default)]
struct ArtCache {
    key: String,
    art: Option<String>,
    changed_at: Option<Instant>,
    checked_at: Option<Instant>,
}

const ART_SETTLE: Duration = Duration::from_secs(8);
const ART_RECHECK: Duration = Duration::from_secs(15);

impl ArtCache {
    fn needs_read(&self, key: &str) -> bool {
        self.key != key
            || self.art.is_none()
            || self.changed_at.is_none_or(|t| t.elapsed() < ART_SETTLE)
            || self.checked_at.is_none_or(|t| t.elapsed() >= ART_RECHECK)
    }
    fn update(&mut self, key: &str, art: Option<String>) {
        if self.key != key {
            self.key = key.to_string();
            self.changed_at = Some(Instant::now());
        }
        self.art = art;
        self.checked_at = Some(Instant::now());
    }
}

fn read_session(manager: &SessionManager, cache: &mut ArtCache) -> windows::core::Result<MediaInfo> {
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

    let key = format!("{app}|{title}|{artist}");
    if cache.needs_read(&key) {
        cache.update(&key, read_cover(&props));
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
        art: cache.art.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_is_reread_after_a_change() {
        let mut c = ArtCache::default();
        assert!(c.needs_read("spotify|a|x"));
        c.update("spotify|a|x", Some("old".into()));
        // right after a change: keep reading (the thumbnail may still be the previous one)
        assert!(c.needs_read("spotify|a|x"));
        c.update("spotify|a|x", Some("new".into()));
        assert_eq!(c.art.as_deref(), Some("new"));
        // settled: no read until the periodic check
        c.changed_at = Some(Instant::now() - ART_SETTLE);
        assert!(!c.needs_read("spotify|a|x"));
        c.checked_at = Some(Instant::now() - ART_RECHECK);
        assert!(c.needs_read("spotify|a|x"));
        // another track or player: read at once
        c.checked_at = Some(Instant::now());
        assert!(c.needs_read("chrome|twitch live|"));
        // no cover yet: keep trying
        c.update("chrome|twitch live|", None);
        c.changed_at = Some(Instant::now() - ART_SETTLE);
        assert!(c.needs_read("chrome|twitch live|"));
    }
}
