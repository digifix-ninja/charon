//! Charon for macOS, Windows and Linux: the window, the tray icon and the autopilot
//! around `charon-core`.
//!
//! * `main.rs`     – starts the app and wires everything together
//! * `state.rs`    – what every command can reach (`AppState`)
//! * `commands.rs` – everything the window can ask for
//! * `events.rs`   – forwards engine events to the window, the tray and notifications
//! * `tray.rs`     – the menu-bar / system-tray icon and its menu
//! * `notifier.rs` – system notifications for errors, conflicts and big syncs
//! * `login.rs`    – launch at login
//! * `desktop.rs`  – showing folders and opening links, per operating system
//! * `about.rs`    – name, version, developer and licence for the About panel
//! * `data_folder.rs` – where jobs and settings live (and moving them from 0.1.0)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod commands;
mod data_folder;
mod desktop;
mod events;
mod login;
mod notifier;
mod state;
mod tray;

use charon_core::{Autopilot, Engine};
use data_folder::DataFolder;
use state::AppState;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tray::Tray;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let handle = app.handle().clone();
            let data_folder = DataFolder::open(&handle)?;
            let engine = Engine::open(data_folder, events::forward_to(&handle))?;
            app.manage(AppState::new(engine.clone()));
            Tray::install(&handle, &engine.overview())?;
            login::LaunchAtLogin::apply(&handle, engine.settings().launch_at_login);
            Autopilot::start(engine);
            Ok(())
        })
        .on_window_event(|window, event| {
            // With "keep running in the background", closing only hides the window.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if let Some(state) = window.try_state::<AppState>() {
                    if state.engine.settings().run_in_background {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::about,
            commands::add_job,
            commands::update_job,
            commands::duplicate_job,
            commands::remove_job,
            commands::move_job,
            commands::set_paused,
            commands::set_all_paused,
            commands::sync_now,
            commands::sync_all,
            commands::cancel,
            commands::check,
            commands::preview,
            commands::resolve_conflict,
            commands::job_log,
            commands::set_settings,
            commands::suggest_name,
            commands::pick_folder,
            commands::reveal_path,
            commands::open_url,
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Charon")
        .run(on_run_event);
}

/// Clicking the Dock icon brings a hidden window back.
fn on_run_event(app: &AppHandle, event: RunEvent) {
    #[cfg(target_os = "macos")]
    if let RunEvent::Reopen { .. } = event {
        show_main_window(app);
    }
    let _ = (app, &event);
}

/// Shows, un-minimises and focuses the main window.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
