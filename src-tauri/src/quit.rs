//! Quitting: asks first when changes still wait for the person's OK, and always lets running
//! syncs stop between two steps, so what they carried is recorded and the two-way memory
//! stays trustworthy.

use crate::state::AppState;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// How long a quit waits for running syncs to stop. A sync stops between two files, and
/// inside a big file between two blocks, so this is plenty unless a drive hangs.
const STOP_WAIT: Duration = Duration::from_secs(30);

/// Payload of the "confirm-quit" event.
#[derive(Clone, Serialize)]
struct ConfirmQuit {
    jobs: usize,
}

/// Payload of the "quitting" event: the syncs Charon stops before it goes.
#[derive(Clone, Serialize)]
struct Quitting {
    jobs: Vec<String>,
}

pub struct Quit {
    /// The syncs have stopped: the app may exit now.
    ready: AtomicBool,
}

impl Quit {
    pub fn new() -> Self {
        Self { ready: AtomicBool::new(false) }
    }

    /// The tray's Quit: when jobs have changes waiting for an OK, the window asks first
    /// (it answers with `quit_app`); otherwise Charon quits.
    pub fn ask(app: &AppHandle) {
        let waiting = app.try_state::<AppState>().map(|state| state.engine.overview().waiting).unwrap_or(0);
        if waiting > 0 {
            crate::show_main_window(app);
            let _ = app.emit("confirm-quit", ConfirmQuit { jobs: waiting });
        } else {
            Self::now(app);
        }
    }

    /// Stops the syncs (on a background thread, so the window stays responsive) and then
    /// exits.
    pub fn now(app: &AppHandle) {
        if let Some(state) = app.try_state::<AppState>() {
            let running = state.engine.running();
            if !running.is_empty() {
                // The window says what it waits for, so Charon doesn't look stuck.
                let jobs = running.iter().filter_map(|id| state.engine.job(id).ok()).map(|view| view.job.setup.name).collect();
                let _ = app.emit("quitting", Quitting { jobs });
            }
        }
        let app = app.clone();
        std::thread::spawn(move || {
            Self::stop_syncs(&app);
            app.exit(0);
        });
    }

    /// Whether the app may exit at once. When not, the caller holds the exit back and
    /// calls `now`, which exits once the syncs have stopped.
    pub fn may_exit(app: &AppHandle) -> bool {
        let ready = app.try_state::<Quit>().is_none_or(|quit| quit.ready.load(Ordering::SeqCst));
        let running = app.try_state::<AppState>().is_some_and(|state| !state.engine.running().is_empty());
        ready || !running
    }

    /// Stops every sync and waits for them. Safe to call more than once.
    pub fn stop_syncs(app: &AppHandle) {
        if let Some(state) = app.try_state::<AppState>() {
            if !state.engine.shut_down(STOP_WAIT) {
                log::error!("a sync didn't stop in time; its next look compares both folders afresh");
            }
        }
        if let Some(quit) = app.try_state::<Quit>() {
            quit.ready.store(true, Ordering::SeqCst);
        }
    }
}
