//! The system's own media controls. The Space being heard shows in the Mac's
//! Now Playing (Control Center, and apps that read it, such as a notch app)
//! and in Windows' media overlay; their play/pause and skip buttons, the
//! keyboard's media keys and headphone buttons reach the player. Nothing to
//! allow: neither system asks for a permission for this.

use std::cell::RefCell;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};
use tauri::{AppHandle, Emitter};
#[cfg(windows)]
use tauri::Manager;

thread_local! {
    // Made and used on the main thread only, as both systems expect
    static CONTROLS: RefCell<Option<MediaControls>> = const { RefCell::new(None) };
    // The picture beside the title (the app's icon), and what was last shown,
    // so that the title and picture are only sent again when they change
    static ARTWORK: RefCell<Option<String>> = const { RefCell::new(None) };
    static SHOWN: RefCell<Option<(String, String, Option<u64>)>> = const { RefCell::new(None) };
}

/// The app's icon, for the picture the system shows beside the title (the
/// Mac's Control Center shows only a picture the app gives it, unlike apps
/// that look the playing app's icon up themselves).
const ICON: &[u8] = include_bytes!("../icons/128x128@2x.png");

/// Puts the icon in a file the system can read, and gives its address.
fn artwork_url() -> Option<String> {
    let path = std::env::temp_dir().join("spacecut-now-playing.png");
    std::fs::write(&path, ICON).ok()?;
    url::Url::from_file_path(&path).ok().map(|u| u.to_string())
}

/// What a press of a system button means to the window: "play", "pause",
/// "toggle", "back" or "forward" (10 seconds, as the app's own buttons), or
/// a place to go to.
fn command(event: MediaControlEvent) -> Option<Value> {
    Some(match event {
        MediaControlEvent::Play => json!("play"),
        MediaControlEvent::Pause | MediaControlEvent::Stop => json!("pause"),
        MediaControlEvent::Toggle => json!("toggle"),
        MediaControlEvent::Next
        | MediaControlEvent::Seek(SeekDirection::Forward)
        | MediaControlEvent::SeekBy(SeekDirection::Forward, _) => json!("forward"),
        MediaControlEvent::Previous
        | MediaControlEvent::Seek(SeekDirection::Backward)
        | MediaControlEvent::SeekBy(SeekDirection::Backward, _) => json!("back"),
        MediaControlEvent::SetPosition(MediaPosition(at)) => json!({ "position": at.as_secs_f64() }),
        _ => return None,
    })
}

/// Sets the controls up; called from the app's setup, on the main thread.
/// Their buttons come to the window as `media-control` events.
pub fn start(app: &AppHandle) {
    #[cfg(windows)]
    let hwnd = app
        .get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as *mut std::ffi::c_void);
    #[cfg(not(windows))]
    let hwnd = None;
    let config = PlatformConfig { display_name: "Spacecut", dbus_name: "spacecut", hwnd };
    let Ok(mut controls) = MediaControls::new(config) else { return };
    let to_window = app.clone();
    let attached = controls.attach(move |event| {
        if let Some(what) = command(event) {
            let _ = to_window.emit("media-control", what);
        }
    });
    if attached.is_ok() {
        CONTROLS.with(|c| *c.borrow_mut() = Some(controls));
        ARTWORK.with(|a| *a.borrow_mut() = artwork_url());
    }
}

/// What the system is told is playing. With nothing in the player, `stopped`.
#[derive(Deserialize, Debug, Clone)]
pub struct NowPlaying {
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    /// None for a Space on air: it has no end yet
    duration: Option<f64>,
    #[serde(default)]
    position: f64,
    #[serde(default)]
    playing: bool,
    #[serde(default)]
    stopped: bool,
}

/// Tells the system, on the main thread, what plays and where it has got to.
pub fn show(app: &AppHandle, now: NowPlaying) {
    let _ = app.run_on_main_thread(move || {
        CONTROLS.with(|c| {
            let mut c = c.borrow_mut();
            let Some(controls) = c.as_mut() else { return };
            if now.stopped {
                SHOWN.with(|s| *s.borrow_mut() = None);
                let _ = controls.set_playback(MediaPlayback::Stopped);
                return;
            }
            let duration = now.duration.filter(|d| d.is_finite() && *d > 0.0).map(Duration::from_secs_f64);
            let shown = (now.title.clone(), now.artist.clone(), duration.map(|d| d.as_millis() as u64 / 100));
            if SHOWN.with(|s| s.borrow().as_ref() != Some(&shown)) {
                ARTWORK.with(|a| {
                    let _ = controls.set_metadata(MediaMetadata {
                        title: Some(&now.title),
                        artist: Some(&now.artist),
                        cover_url: a.borrow().as_deref(),
                        duration,
                        ..Default::default()
                    });
                });
                SHOWN.with(|s| *s.borrow_mut() = Some(shown));
            }
            let progress = Some(MediaPosition(Duration::from_secs_f64(now.position.max(0.0))));
            let _ = controls.set_playback(if now.playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_buttons_become_the_apps_own_commands() {
        assert_eq!(command(MediaControlEvent::Toggle), Some(json!("toggle")));
        assert_eq!(command(MediaControlEvent::Stop), Some(json!("pause")));
        assert_eq!(command(MediaControlEvent::Next), Some(json!("forward")));
        assert_eq!(command(MediaControlEvent::SeekBy(SeekDirection::Backward, Duration::from_secs(15))), Some(json!("back")));
        assert_eq!(command(MediaControlEvent::SetPosition(MediaPosition(Duration::from_secs(90)))), Some(json!({ "position": 90.0 })));
        assert_eq!(command(MediaControlEvent::Quit), None);
    }
}
