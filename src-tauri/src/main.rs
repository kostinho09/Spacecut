#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cutter;
mod fetch;
mod ffmpeg;
mod hls;
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
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn env_info(app: AppHandle, state: State<AppState>) -> Env {
    let p = prefs::load();
    let output_folder = p
        .get("output_folder")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| util::path_str(&util::desktop_dir()));
    Env {
        platform: if cfg!(target_os = "macos") { "macos" } else if cfg!(windows) { "windows" } else { "linux" },
        version: app.package_info().version.to_string(),
        ffmpeg: state.ffmpeg.is_some(),
        output_folder,
        effects: prefs::load_effects(),
        lang: p.get("lang").and_then(Value::as_str).unwrap_or("en").to_string(),
        theme: p.get("theme").and_then(Value::as_str).unwrap_or("auto").to_string(),
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
        let dialog = if kind == "effect" {
            dialog
                .set_title("Select Sound Effect")
                .add_filter("Audio", &["mp3", "m4a", "wav", "aac", "flac", "ogg", "aiff"])
        } else {
            dialog.set_title("Select Audio or Video File").add_filter(
                "Audio & Video",
                &["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "ts", "aiff", "mp4", "mkv", "mov", "avi", "webm", "m4v", "flv"],
            )
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
fn clipboard_read(app: AppHandle) -> String {
    app.clipboard().read_text().unwrap_or_default()
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

/// Says the cut is over, unless the user is already looking at the window.
#[tauri::command]
fn notify_done(app: AppHandle, window: tauri::Window, body: String) {
    if window.is_focused().unwrap_or(false) {
        return;
    }
    let _ = app.notification().builder().title("Space Cutter").body(body).show();
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
                ffmpeg,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            env_info,
            set_pref,
            save_effects,
            pick_folder,
            pick_files,
            clipboard_read,
            clipboard_write,
            convert_live,
            player_load,
            player_play,
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
        .expect("error while starting Space Cutter")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Leave no ffmpeg running behind the closed window
                let state = app.state::<AppState>();
                state.job.stop();
                state.player.shutdown();
            }
        });
}
