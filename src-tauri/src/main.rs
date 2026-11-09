#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cutter;
mod fetch;
mod ffmpeg;
mod hls;
mod media;
mod player;
mod prefs;
mod update;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

struct AppState {
    ffmpeg: Option<PathBuf>,
    player: Arc<player::Player>,
    job: Arc<cutter::Control>,
    /// The window's size before it became the mini player.
    full_size: std::sync::Mutex<Option<tauri::LogicalSize<f64>>>,
}

#[derive(Serialize)]
struct Env {
    platform: &'static str,
    version: String,
    ffmpeg: bool,
    output_folder: String,
    effects: Vec<prefs::Effect>,
    lang: String,
    theme: String,
    /// Whether to ask GitHub once a day for a newer version.
    update_check: bool,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn env_info(app: AppHandle, state: State<AppState>) -> Env {
    let p = prefs::load();
    // Only a folder the user chose (and that is still there): nothing is
    // saved anywhere by default, so the Mac never asks to let the app in
    let output_folder = p
        .get("output_folder")
        .and_then(Value::as_str)
        .filter(|f| std::path::Path::new(f).is_dir())
        .unwrap_or_default()
        .to_string();
    Env {
        platform: if cfg!(target_os = "macos") { "macos" } else if cfg!(windows) { "windows" } else { "linux" },
        version: app.package_info().version.to_string(),
        ffmpeg: state.ffmpeg.is_some(),
        output_folder,
        effects: prefs::load_effects(),
        lang: p.get("lang").and_then(Value::as_str).unwrap_or("en").to_string(),
        theme: p.get("theme").and_then(Value::as_str).unwrap_or("auto").to_string(),
        update_check: p.get("update_check").and_then(Value::as_bool).unwrap_or(true),
    }
}

#[tauri::command]
fn set_pref(key: String, value: Value) {
    prefs::set(&key, value);
}

#[tauri::command]
fn save_effects(effects: Vec<prefs::Effect>) {
    prefs::save_effects(&effects);
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    blocking(move || {
        app.dialog()
            .file()
            .set_title("Select Output Folder")
            .blocking_pick_folder()
            .and_then(|p| p.into_path().ok())
            .map(|p| util::path_str(&p))
    })
    .await
}

#[tauri::command]
async fn pick_files(app: AppHandle, kind: String, multiple: bool) -> Result<Vec<String>, String> {
    blocking(move || {
        let dialog = app.dialog().file().set_directory(util::home_dir());
        use cutter::{AAC_EXTS, AUDIO_EXTS, VIDEO_EXTS};
        let sound = AAC_EXTS[..2].iter().chain(&AUDIO_EXTS);
        let dialog = if kind == "effect" {
            dialog.set_title("Select Sound Effect").add_filter("Audio", &sound.copied().collect::<Vec<_>>())
        } else {
            let all: Vec<&str> = sound.chain(&AAC_EXTS[2..]).chain(&VIDEO_EXTS).copied().collect();
            dialog.set_title("Select Audio or Video File").add_filter("Audio & Video", &all)
        };
        let picked = if multiple {
            dialog.blocking_pick_files().unwrap_or_default()
        } else {
            dialog.blocking_pick_file().into_iter().collect()
        };
        picked
            .into_iter()
            .filter_map(|p| p.into_path().ok())
            .map(|p| util::path_str(&p))
            .collect()
    })
    .await
}

#[tauri::command]
fn clipboard_write(app: AppHandle, text: String) {
    let _ = app.clipboard().write_text(text);
}

#[tauri::command]
async fn convert_live(url: String) -> Result<String, String> {
    blocking(move || hls::live_to_replay(&url)).await?
}

#[tauri::command]
async fn player_load(state: State<'_, AppState>, url: String, live: Option<String>) -> Result<player::LoadInfo, String> {
    let player = state.player.clone();
    blocking(move || player.load(&url, live.as_deref())).await?
}

#[derive(Serialize)]
struct SpaceInfo {
    duration: f64,
    live: bool,
    /// Where live is now, for a Space on air: its length plus what came out since.
    edge: f64,
}

/// How long a Space (or file) is, and whether it is on air, without loading
/// it into the player: for one looked at while another plays.
#[tauri::command]
async fn space_info(state: State<'_, AppState>, url: String, live: Option<String>) -> Result<SpaceInfo, String> {
    let ffmpeg = state.ffmpeg.clone();
    blocking(move || {
        if hls::is_m3u8(&url) {
            let list = hls::resolve_known(&url, live.as_deref())?;
            let since = list.ends_at.map_or(0.0, |t| (now_secs() - t).max(0.0));
            let edge = if list.ended { list.duration } else { list.duration + since };
            Ok(SpaceInfo { duration: list.duration, live: !list.ended, edge })
        } else {
            let duration = ffmpeg.and_then(|ff| ffmpeg::probe_duration(&ff, &url)).unwrap_or(0.0);
            Ok(SpaceInfo { duration, live: false, edge: duration })
        }
    })
    .await?
}

#[derive(Serialize)]
struct Fresh {
    url: String,
    duration: f64,
    edge: f64,
}

/// A replay link of a Space on air made now, holding all that is out: what
/// Copy gives. The bar grows to it too.
#[tauri::command]
async fn fresh_replay(state: State<'_, AppState>, url: String, live: Option<String>) -> Result<Fresh, String> {
    let player = state.player.clone();
    blocking(move || {
        let dynamic = hls::dynamic_for(&url, live.as_deref()).ok_or("not a Space")?;
        let list = hls::replay_now(&dynamic).or_else(|_| hls::resolve_fresh(&url, live.as_deref()))?;
        hls::keep_grown(&url, live.as_deref(), &list.segments, list.ends_at, false);
        player.take_fresh(&url, &list);
        let since = list.ends_at.map_or(0.0, |t| (now_secs() - t).max(0.0));
        Ok(Fresh { url: list.url.clone(), duration: list.duration, edge: list.duration + since })
    })
    .await?
}

/// Whether a Space is still on air; None without an answer.
#[tauri::command]
async fn space_live(url: String, live: Option<String>) -> Result<Option<bool>, String> {
    blocking(move || {
        hls::dynamic_for(&url, live.as_deref()).and_then(|d| match hls::check_live(&d) {
            hls::LiveCheck::On(_) => Some(true),
            hls::LiveCheck::Over => {
                // Over: the whole of it, once, for when it is opened or cut
                let _ = hls::resolve_fresh(&url, live.as_deref());
                Some(false)
            }
            hls::LiveCheck::Unknown => None,
        })
    })
    .await
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// GO LIVE: the newest of a Space on air, playing.
#[tauri::command]
async fn player_go_live(state: State<'_, AppState>) -> Result<(), String> {
    let player = state.player.clone();
    blocking(move || player.go_live()).await?
}

/// The mini player: the window only as tall as the player (`height`), its
/// width kept; and back to the size it had.
#[tauri::command]
fn set_mini(window: tauri::WebviewWindow, state: State<AppState>, on: bool, height: f64) -> Result<(), String> {
    use tauri::LogicalSize;
    let e = |e: tauri::Error| e.to_string();
    let mut saved = state.full_size.lock().unwrap();
    if on {
        let scale = window.scale_factor().map_err(e)?;
        let size = window.inner_size().map_err(e)?.to_logical::<f64>(scale);
        if saved.is_none() {
            *saved = Some(size);
        }
        window.set_min_size(Some(LogicalSize::new(460.0, height))).map_err(e)?;
        window.set_max_size(Some(LogicalSize::new(10_000.0, height))).map_err(e)?;
        window.set_size(LogicalSize::new(size.width, height)).map_err(e)?;
    } else {
        let full = saved.take().unwrap_or(LogicalSize::new(560.0, 740.0));
        window.set_max_size(None::<LogicalSize<f64>>).map_err(e)?;
        window.set_min_size(Some(LogicalSize::new(460.0, 560.0))).map_err(e)?;
        // It grows up and down around where the small window is, and stays
        // whole on its screen: growing only downwards, a small window moved
        // low would hide most of the full one below the screen's edge
        let at = window.outer_position().map_err(e)?;
        let now = window.outer_size().map_err(e)?;
        let frame = now.height.saturating_sub(window.inner_size().map_err(e)?.height) as f64;
        let tall = full.height * window.scale_factor().map_err(e)? + frame;
        if let Some(screen) = window.current_monitor().map_err(e)? {
            let area = screen.work_area();
            let top = area.position.y as f64;
            let bottom = top + area.size.height as f64;
            let y = (at.y as f64 + now.height as f64 / 2.0 - tall / 2.0).min(bottom - tall).max(top);
            window.set_position(tauri::PhysicalPosition::new(at.x, y.round() as i32)).map_err(e)?;
        }
        window.set_size(full).map_err(e)?;
    }
    Ok(())
}

/// Where the window goes and how big its content is, to be whole on the
/// screen's free area (`area`: left, top, width, height; without the taskbar
/// or the Dock), or None when it already is. `at` and `outer` are the
/// window's place and size with its frame, `frame` what the system adds
/// around the content (on Windows, the title bar), `least` the smallest
/// content allowed. All in the screen's own pixels.
fn fit(area: (f64, f64, f64, f64), at: (f64, f64), outer: (f64, f64), frame: (f64, f64), least: (f64, f64)) -> Option<((f64, f64), (f64, f64))> {
    let (left, top, width, height) = area;
    let whole = at.0 >= left && at.1 >= top && at.0 + outer.0 <= left + width && at.1 + outer.1 <= top + height;
    if whole {
        return None;
    }
    let w = outer.0.min(width).max(least.0 + frame.0);
    let h = outer.1.min(height).max(least.1 + frame.1);
    let place = (left + ((width - w) / 2.0).max(0.0), top + ((height - h) / 2.0).max(0.0));
    Some((place, (w - frame.0, h - frame.1)))
}

/// On opening: a window too big for its screen (a small one, or the title
/// bar Windows adds above the content) is made smaller, never below the
/// smallest size, and put in the middle, so that nothing is hidden below the
/// screen's edge or the taskbar.
fn fit_on_screen(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    let Some(screen) = window.current_monitor()? else { return Ok(()) };
    let area = screen.work_area();
    let (at, outer, inner) = (window.outer_position()?, window.outer_size()?, window.inner_size()?);
    let scale = window.scale_factor()?;
    let frame = (outer.width.saturating_sub(inner.width) as f64, outer.height.saturating_sub(inner.height) as f64);
    let found = fit(
        (area.position.x as f64, area.position.y as f64, area.size.width as f64, area.size.height as f64),
        (at.x as f64, at.y as f64),
        (outer.width as f64, outer.height as f64),
        frame,
        (460.0 * scale, 560.0 * scale),
    );
    if let Some(((x, y), (w, h))) = found {
        window.set_size(tauri::PhysicalSize::new(w.round() as u32, h.round() as u32))?;
        window.set_position(tauri::PhysicalPosition::new(x.round() as i32, y.round() as i32))?;
    }
    Ok(())
}

/// What plays, for the system's Now Playing and media keys.
#[tauri::command]
fn media_now(app: AppHandle, now: media::NowPlaying) {
    media::show(&app, now);
}

#[tauri::command]
fn player_play(state: State<AppState>) -> Result<(), String> {
    state.player.play()
}

#[tauri::command]
fn player_pause(state: State<AppState>) {
    state.player.pause();
}

#[tauri::command]
fn player_seek(state: State<AppState>, t: f64) -> Result<(), String> {
    state.player.seek(t)
}

#[tauri::command]
fn player_speed(state: State<AppState>, speed: f64) -> Result<(), String> {
    state.player.set_speed(speed)
}

#[tauri::command]
fn player_state(state: State<AppState>) -> player::State {
    state.player.state()
}

#[tauri::command]
async fn fx_play(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let player = state.player.clone();
    blocking(move || player.fx_play(&path)).await?
}

#[tauri::command]
fn fx_stop(state: State<AppState>) {
    state.player.fx_stop();
}

/// How long a sound effect lasts, for the merged file's total.
#[tauri::command]
async fn sound_length(state: State<'_, AppState>, path: String) -> Result<Option<f64>, String> {
    Ok(state.ffmpeg.as_deref().and_then(|ff| ffmpeg::probe_duration(ff, &path)))
}

#[tauri::command]
fn start_job(app: AppHandle, state: State<AppState>, job: cutter::Job) -> Result<(), String> {
    cutter::start(app, state.ffmpeg.clone(), state.job.clone(), job)
}

#[tauri::command]
fn cancel_job(state: State<AppState>) {
    state.job.stop();
}

/// A newer version on GitHub, checked at most once a day.
#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<update::Update>, String> {
    let current = app.package_info().version.to_string();
    blocking(move || update::check(&current)).await
}

#[tauri::command]
fn dismiss_update(version: String) {
    update::dismiss(&version);
}

#[tauri::command]
fn open_update(url: String) -> Result<(), String> {
    update::open(&url)
}

#[cfg(target_os = "macos")]
const CHECK_UPDATES: &str = "check_updates";

/// "Check for Updates…" under "About" in the Mac's menu bar, in the menu
/// Tauri gives a Mac app anyway.
#[cfg(target_os = "macos")]
fn add_update_item(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        app_menu.insert(&MenuItem::with_id(app, CHECK_UPDATES, "Check for Updates…", true, None::<&str>)?, 1)?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id().as_ref() == CHECK_UPDATES {
            let app = app.clone();
            std::thread::spawn(move || tell_about_updates(&app));
        }
    });
    Ok(())
}

/// Asks GitHub now and says what it found in a small window of its own.
#[cfg(target_os = "macos")]
fn tell_about_updates(app: &AppHandle) {
    use tauri_plugin_dialog::{MessageDialogButtons, MessageDialogKind};
    let current = app.package_info().version.to_string();
    match update::check_now(&current) {
        update::Found::Newer(found) => app
            .dialog()
            .message(format!("Spacecut {} is out. You have {current}.", found.version.trim_start_matches(['v', 'V'])))
            .title("A new version is available")
            .buttons(MessageDialogButtons::OkCancelCustom("Download".into(), "Later".into()))
            .show(move |download| {
                if download {
                    let _ = update::open(&found.url);
                }
            }),
        update::Found::UpToDate => app
            .dialog()
            .message(format!("Spacecut {current} is the newest version."))
            .title("You're up to date")
            .kind(MessageDialogKind::Info)
            .show(|_| {}),
        update::Found::NoAnswer => app
            .dialog()
            .message("Spacecut couldn't reach GitHub. Check your internet connection and try again.")
            .title("Couldn't check for updates")
            .kind(MessageDialogKind::Warning)
            .show(|_| {}),
    }
}

/// Says the cut is over, unless the user is already looking at the window.
#[tauri::command]
fn notify_done(app: AppHandle, window: tauri::Window, body: String) {
    if window.is_focused().unwrap_or(false) {
        return;
    }
    let _ = app.notification().builder().title("Spacecut").body(body).show();
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let ffmpeg = ffmpeg::locate(app.path().resource_dir().ok());
            app.manage(AppState {
                player: Arc::new(player::Player::new(ffmpeg.clone())),
                job: Arc::new(cutter::Control::default()),
                full_size: Default::default(),
                ffmpeg,
            });
            #[cfg(target_os = "macos")]
            add_update_item(app.handle())?;
            media::start(app.handle());
            // The window is made hidden and centred (tauri.conf.json), sized
            // to its screen here, and only then shown: it opens where it
            // stays, without a jump. Shown even if fitting failed.
            if let Some(window) = app.get_webview_window("main") {
                let _ = fit_on_screen(&window);
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            env_info,
            set_pref,
            save_effects,
            pick_folder,
            pick_files,
            clipboard_write,
            convert_live,
            player_load,
            space_info,
            space_live,
            fresh_replay,
            player_play,
            player_go_live,
            set_mini,
            media_now,
            player_pause,
            player_seek,
            player_state,
            player_speed,
            fx_play,
            fx_stop,
            sound_length,
            start_job,
            cancel_job,
            notify_done,
            check_update,
            dismiss_update,
            open_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Spacecut")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Leave no ffmpeg running behind the closed window
                let state = app.state::<AppState>();
                state.job.stop();
                state.player.shutdown();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn a_window_too_tall_for_its_screen_is_made_to_fit() {
        let least = (460.0, 560.0);
        // Fits: left as it is
        assert_eq!(fit((0.0, 0.0, 1920.0, 1032.0), (100.0, 50.0), (560.0, 772.0), (0.0, 32.0), least), None);
        // The screen in a virtual machine: 765 high, 48 of it the taskbar;
        // 740 of content and the 32 of the title bar don't fit
        let ((x, y), (w, h)) = fit((0.0, 0.0, 1040.0, 717.0), (20.0, 20.0), (560.0, 772.0), (0.0, 32.0), least).unwrap();
        assert_eq!((w, h), (560.0, 685.0));
        assert_eq!((x, y), (240.0, 0.0));
        // Never below the smallest size
        let (_, (_, h)) = fit((0.0, 0.0, 800.0, 500.0), (0.0, 0.0), (560.0, 772.0), (0.0, 32.0), least).unwrap();
        assert_eq!(h, 560.0);
        // Whole but partly off the screen's free area: only moved
        let ((_, y), (_, h)) = fit((0.0, 0.0, 1920.0, 1032.0), (0.0, 400.0), (560.0, 772.0), (0.0, 32.0), least).unwrap();
        assert_eq!((y, h), (130.0, 740.0));
    }
}
