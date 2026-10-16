//! Everything the window can ask for. Errors reach the window as plain sentences.
//!
//! Commands that touch the disk (previews, picking folders, settling conflicts) are `async`
//! and do their work on a background thread, so the window never freezes.

use crate::about::About;
use crate::desktop::Desktop;
use crate::login::LaunchAtLogin;
use crate::state::AppState;
use charon_core::engine::Preview;
use charon_core::store::LogLine;
use charon_core::{ConflictChoice, JobId, JobSetup, JobView, Overview, Settings, SyncMode};
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

type Answer<T> = Result<T, String>;

/// Turns any error into the sentence the window shows.
fn say<E: ToString>(error: E) -> String {
    error.to_string()
}

/// Everything the window needs when it opens.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    jobs: Vec<JobView>,
    settings: Settings,
    overview: Overview,
    default_excludes: Vec<String>,
}

#[tauri::command]
pub fn get_state(state: State<AppState>) -> WindowState {
    let engine = &state.engine;
    WindowState {
        jobs: engine.jobs(),
        settings: engine.settings(),
        overview: engine.overview(),
        default_excludes: JobSetup::default_excludes(),
    }
}

#[tauri::command]
pub fn about(app: AppHandle) -> About {
    About::of(&app)
}

// ---------------------------------------------------------------- jobs

#[tauri::command]
pub fn add_job(state: State<AppState>, setup: JobSetup) -> Answer<JobView> {
    state.engine.add_job(setup).map_err(say)
}

#[tauri::command]
pub fn update_job(state: State<AppState>, id: JobId, setup: JobSetup) -> Answer<JobView> {
    state.engine.update_job(&id, setup).map_err(say)
}

#[tauri::command]
pub fn duplicate_job(state: State<AppState>, id: JobId) -> Answer<JobView> {
    state.engine.duplicate_job(&id).map_err(say)
}

#[tauri::command]
pub fn remove_job(state: State<AppState>, id: JobId) -> Answer<()> {
    state.engine.remove_job(&id).map_err(say)
}

#[tauri::command]
pub fn move_job(state: State<AppState>, id: JobId, to_index: usize) -> Answer<()> {
    state.engine.move_job(&id, to_index).map_err(say)
}

#[tauri::command]
pub fn set_paused(state: State<AppState>, id: JobId, paused: bool) -> Answer<()> {
    state.engine.set_paused(&id, paused).map_err(say)
}

#[tauri::command]
pub fn set_all_paused(state: State<AppState>, paused: bool) -> Answer<()> {
    state.engine.set_all_paused(paused).map_err(say)
}

#[tauri::command]
pub fn suggest_name(source: PathBuf, destination: PathBuf, mode: SyncMode) -> String {
    JobSetup::suggested_name(&source, &destination, mode)
}

// ---------------------------------------------------------------- running

#[tauri::command]
pub fn sync_now(state: State<AppState>, id: JobId) -> Answer<()> {
    state.engine.sync_now(&id).map_err(say)
}

#[tauri::command]
pub fn sync_all(state: State<AppState>) {
    state.engine.sync_all();
}

#[tauri::command]
pub fn cancel(state: State<AppState>, id: JobId) {
    state.engine.cancel(&id);
}

#[tauri::command]
pub fn check(state: State<AppState>, id: JobId) {
    state.engine.check(&id);
}

/// A dry run: what the next sync would do.
#[tauri::command]
pub async fn preview(state: State<'_, AppState>, id: JobId) -> Answer<Preview> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.preview(&id))
        .await
        .map_err(say)?
        .map_err(say)
}

#[tauri::command]
pub async fn resolve_conflict(state: State<'_, AppState>, id: JobId, path: String, choice: ConflictChoice) -> Answer<()> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.resolve_conflict(&id, &path, choice))
        .await
        .map_err(say)?
        .map_err(say)
}

#[tauri::command]
pub fn job_log(state: State<AppState>, id: JobId) -> Vec<LogLine> {
    state.engine.log(&id, 300)
}

// ---------------------------------------------------------------- settings

#[tauri::command]
pub fn set_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> Answer<()> {
    let login_changed = state.engine.settings().launch_at_login != settings.launch_at_login;
    if login_changed {
        LaunchAtLogin::apply(&app, settings.launch_at_login);
    }
    state.engine.set_settings(settings).map_err(say)
}

// ---------------------------------------------------------------- the desktop

/// Opens the system's folder picker. `None` when cancelled.
#[tauri::command]
pub async fn pick_folder(app: AppHandle, start: Option<PathBuf>) -> Option<PathBuf> {
    let mut dialog = app.dialog().file().set_title("Choose a folder");
    if let Some(start) = start.filter(|p| p.is_dir()) {
        dialog = dialog.set_directory(start);
    }
    dialog.blocking_pick_folder().and_then(|folder| folder.into_path().ok())
}

#[tauri::command]
pub fn reveal_path(path: PathBuf) -> Answer<()> {
    Desktop::reveal(&path)
}

#[tauri::command]
pub fn open_url(url: String) -> Answer<()> {
    if !About::may_open(&url) {
        return Err("Charon only opens its own website and GitHub.".into());
    }
    Desktop::open_url(&url)
}
