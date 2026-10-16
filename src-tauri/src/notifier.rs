//! System notifications: only when something needs the person, or a big sync is done.
//! Small routine syncs stay silent.

use crate::state::AppState;
use charon_core::{human_bytes, plural, Finished};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// A sync this big is worth a "finished" notification.
const BIG_SYNC_BYTES: u64 = 1_000_000_000;
const BIG_SYNC_FILES: u64 = 1_000;

pub struct Notifier;

impl Notifier {
    /// Tells the person about a finished run, if it's worth it and notifications are on.
    pub fn announce(app: &AppHandle, finished: &Finished) {
        let enabled = app.try_state::<AppState>().is_some_and(|s| s.engine.settings().notifications);
        if !enabled {
            return;
        }
        if let Some((title, body)) = Self::message(finished) {
            if let Err(e) = app.notification().builder().title(title).body(body).show() {
                log::warn!("couldn't show a notification: {e}");
            }
        }
    }

    /// The title and text for a run, or `None` if it isn't worth interrupting for.
    fn message(finished: &Finished) -> Option<(String, String)> {
        let name = &finished.name;
        let run = &finished.summary;
        if run.cancelled {
            return None;
        }
        if let Some(error) = &run.error {
            return Some((format!("{name} didn't sync"), error.clone()));
        }
        if run.failed > 0 {
            return Some((
                format!("{name}: {} couldn't be copied", plural(run.failed, "file")),
                "Open Charon to see which ones in the job's log.".into(),
            ));
        }
        if run.conflicts > 0 {
            return Some((
                format!("{name}: {} to resolve", plural(run.conflicts, "conflict")),
                "A file changed on both sides. Open Charon to choose which version to keep.".into(),
            ));
        }
        if run.bytes >= BIG_SYNC_BYTES || run.copied >= BIG_SYNC_FILES {
            return Some((
                format!("{name} is in sync"),
                format!("Copied {} ({}).", plural(run.copied, "file"), human_bytes(run.bytes)),
            ));
        }
        None
    }
}
