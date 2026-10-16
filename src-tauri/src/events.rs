//! Sends engine events to the window (by name), the tray icon and notifications.

use crate::notifier::Notifier;
use crate::tray::Tray;
use charon_core::{Emitter, EngineEvent, JobId, Progress};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter as _};

/// Payload of the "progress" event.
#[derive(Clone, Serialize)]
struct ProgressEvent {
    id: JobId,
    progress: Progress,
}

/// An emitter that forwards every engine event.
pub fn forward_to(app: &AppHandle) -> Emitter {
    let app = app.clone();
    Arc::new(move |event: EngineEvent| {
        let _ = match event {
            EngineEvent::Job(view) => app.emit("job", view),
            EngineEvent::JobRemoved(id) => app.emit("job-removed", id),
            EngineEvent::Progress { id, progress } => app.emit("progress", ProgressEvent { id, progress }),
            EngineEvent::Finished(finished) => {
                Notifier::announce(&app, &finished);
                app.emit("finished", finished)
            }
            EngineEvent::Overview(overview) => {
                Tray::refresh(&app, &overview);
                app.emit("overview", overview)
            }
        };
    })
}
