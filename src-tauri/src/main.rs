//! Charon for macOS, Windows and Linux: the window, the tray icon and the autopilot
//! around `charon-core`.
//!
//! * `main.rs`     – starts the app and wires everything together
//! * `state.rs`    – what every command can reach (`AppState`)
//! * `commands.rs` – everything the window can ask for
//! * `events.rs`   – forwards engine events to the window, the tray and notifications
//! * `tray.rs`     – the menu-bar / system-tray icon and its menu
//! * `notifier.rs` – system notifications: errors, conflicts, big syncs, changes waiting for an OK
//! * `quit.rs`     – quitting: asks first when changes wait, lets running syncs stop cleanly
//! * `login.rs`    – launch at login
//! * `desktop.rs`  – showing folders and opening links, per operating system
//! * `about.rs`    – name, version, developer and licence for the About panel
//! * `updates.rs`  – asks GitHub whether a newer version is out
//! * `data_folder.rs` – where jobs and settings live (and moving them from 0.1.0)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod commands;
mod data_folder;
mod desktop;
mod events;
mod login;
mod notifier;
mod quit;
mod state;
mod tray;
mod updates;

use charon_core::{Autopilot, Engine, Error};
use data_folder::DataFolder;
use quit::Quit;
use state::AppState;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};
use tray::Tray;
use updates::Updates;

/// The log file grows to this size before a new one starts; the newest few are kept.
const LOG_FILE_SIZE: u128 = 5_000_000;
const LOG_FILES_KEPT: usize = 4;

fn main() {
    let builder = tauri::Builder::default();
    // A second launch of the installed app brings the first one's window up instead. A
    // development build keeps its own data, so it may run next to the installed app (the
    // engine's lock on the data folder still keeps two of the same kind apart).
    #[cfg(not(debug_assertions))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main_window(app)));
    builder
        .plugin(logger())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let handle = app.handle().clone();
            log::info!("Charon {} starting", env!("CARGO_PKG_VERSION"));
            let data_folder = DataFolder::open(&handle)?;
            let engine = match Engine::open(data_folder, events::forward_to(&handle)) {
                Ok(engine) => engine,
                Err(e) => {
                    could_not_start(&handle, e);
                    return Ok(());
                }
            };
            app.manage(AppState::new(engine.clone()));
            app.manage(Quit::new());
            Tray::install(&handle, &engine.overview())?;
            login::LaunchAtLogin::apply(&handle, engine.settings().launch_at_login);
            Autopilot::start(engine);
            Updates::start(&handle);
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
            commands::check_updates,
            commands::update_info,
            commands::add_job,
            commands::update_job,
            commands::duplicate_job,
            commands::remove_job,
            commands::move_job,
            commands::set_paused,
            commands::set_all_paused,
            commands::suggest_name,
            commands::leave_out,
            commands::sync_now,
            commands::sync_all,
            commands::cancel,
            commands::check,
            commands::prepare_review,
            commands::cancel_review,
            commands::review_steps,
            commands::review_notes,
            commands::approve,
            commands::discard_review,
            commands::prepare_resolution,
            commands::job_log,
            commands::upgrade_notice,
            commands::dismiss_upgrade_notice,
            commands::set_settings,
            commands::pick_folder,
            commands::reveal_path,
            commands::open_url,
            commands::open_privacy_settings,
            commands::quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Charon")
        .run(on_run_event);
}

/// Logs go to a file in the system's log folder (macOS: ~/Library/Logs/dev.stean.charon/),
/// kept across restarts, and to the terminal in development builds.
fn logger() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let mut targets = vec![Target::new(TargetKind::LogDir { file_name: Some(DataFolder::log_name().into()) })];
    if cfg!(debug_assertions) {
        targets.push(Target::new(TargetKind::Stdout));
    }
    tauri_plugin_log::Builder::new()
        .targets(targets)
        .level(log::LevelFilter::Info)
        .level_for("notify", log::LevelFilter::Warn)
        .max_file_size(LOG_FILE_SIZE)
        .rotation_strategy(RotationStrategy::KeepSome(LOG_FILES_KEPT))
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .build()
}

/// Charon couldn't open its jobs: says so in plain words and quits. Nothing in any synced
/// folder has been touched at this point.
fn could_not_start(app: &AppHandle, error: Error) {
    log::error!("couldn't open Charon's data: {error}");
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    let (title, text) = match error {
        Error::AlreadyRunning => (
            "Charon is already running",
            "Another Charon is already looking after your sync jobs. Use that one: its boat is in the menu bar (or the system tray).".to_string(),
        ),
        e => ("Charon couldn't start", format!("Charon couldn't open its list of jobs: {e}\n\nNothing in your folders was changed.")),
    };
    let handle = app.clone();
    app.dialog().message(text).title(title).kind(MessageDialogKind::Error).show(move |_| handle.exit(1));
}

fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        // Clicking the Dock icon brings a hidden window back.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main_window(app),
        // Running syncs stop between two steps before Charon goes.
        RunEvent::ExitRequested { api, .. } => {
            if !Quit::may_exit(app) {
                api.prevent_exit();
                Quit::now(app);
            }
        }
        RunEvent::Exit => Quit::stop_syncs(app),
        _ => {}
    }
}

/// Shows, un-minimises and focuses the main window.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
