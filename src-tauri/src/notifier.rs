//! System notifications: only when something needs the person, or a big sync is done.
//! Small routine syncs stay silent.

use crate::state::AppState;
use charon_core::{human_bytes, plural, Finished, WaitingJob};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// A sync this big is worth a "finished" notification.
const BIG_SYNC_BYTES: u64 = 1_000_000_000;
const BIG_SYNC_FILES: u64 = 1_000;

pub struct Notifier;

impl Notifier {
    /// Tells the person about a finished run, if it's worth it.
    pub fn announce(app: &AppHandle, finished: &Finished) {
        if let Some((title, body)) = Self::finished(finished) {
            Self::show(app, &title, &body);
        }
    }

    /// Tells the person that changes wait for their OK (one notification for all jobs due
    /// a mention; the engine decides how often).
    pub fn waiting(app: &AppHandle, jobs: &[WaitingJob]) {
        if let Some((title, body)) = Self::waiting_message(jobs) {
            Self::show(app, &title, &body);
        }
    }

    /// Tells the person that a run of a job with full automation was held back.
    pub fn held(app: &AppHandle, name: &str, reason: &str) {
        let title = format!("“{name}” was held back for your OK");
        let body = format!("{reason} Nothing was changed. Open Charon to look at the list.");
        Self::show(app, &title, &body);
    }

    fn show(app: &AppHandle, title: &str, body: &str) {
        let enabled = app.try_state::<AppState>().is_some_and(|s| s.engine.settings().notifications);
        if !enabled {
            return;
        }
        if let Err(e) = app.notification().builder().title(title).body(body).show() {
            log::warn!("couldn't show a notification: {e}");
        }
    }

    /// The title and text for a run, or `None` if it isn't worth interrupting for.
    fn finished(finished: &Finished) -> Option<(String, String)> {
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
        if run.left_out > 0 {
            let what = if run.left_out == 1 { "it" } else { "them" };
            return Some((
                format!("{name}: {} left out", plural(run.left_out, "item")),
                format!("A drive couldn't hold {what} (a name, a path or a size it refused, or a symbolic link). Open Charon to see which ones and why."),
            ));
        }
        if run.conflicts > 0 {
            return Some((
                format!("{name}: {} to resolve", plural(run.conflicts, "conflict")),
                "A file changed on both sides. Open Charon to choose which version to keep.".into(),
            ));
        }
        if run.settled > 0 && !run.approved {
            return Some((
                format!("{name}: {} settled by Newest wins", plural(run.settled, "conflict")),
                "The version saved last was kept on both sides; the other one is in .charon-trash. The job's log lists them.".into(),
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

    fn waiting_message(jobs: &[WaitingJob]) -> Option<(String, String)> {
        match jobs {
            [] => None,
            [job] => {
                let what = Self::what_waits(job.items, job.removals, job.conflicts);
                let title = if job.items == 0 && job.conflicts > 0 {
                    format!("“{}” has {} to settle", job.name, plural(job.conflicts, "conflict"))
                } else {
                    format!("“{}” waits for your OK", job.name)
                };
                let next = if job.drive_connected {
                    "Nothing has been copied yet: look at the list before you eject the drive."
                } else {
                    "Open Charon to look at the list and confirm it."
                };
                Some((title, format!("{what} {next}")))
            }
            jobs => {
                let names: Vec<String> = jobs.iter().map(|job| format!("“{}”", job.name)).collect();
                let (last, rest) = names.split_last()?;
                let total = |f: fn(&WaitingJob) -> u64| jobs.iter().map(f).sum::<u64>();
                let what = Self::what_waits(total(|j| j.items), total(|j| j.removals), total(|j| j.conflicts));
                Some((
                    format!("{} jobs wait for your OK", jobs.len()),
                    format!("{} and {last}. {what} Open Charon to look at them and confirm.", rest.join(", ")),
                ))
            }
        }
    }

    /// "23 changes are ready to sync, 3 of them removals, and 1 file changed on both sides."
    fn what_waits(items: u64, removals: u64, conflicts: u64) -> String {
        let mut text = match items {
            0 => String::new(),
            1 => "1 change is ready to sync".into(),
            n => format!("{} are ready to sync", plural(n, "change")),
        };
        match removals {
            0 => {}
            1 if items == 1 => text.push_str(": a removal"),
            1 => text.push_str(", 1 of them a removal"),
            n => text.push_str(&format!(", {} of them removals", charon_core::human_count(n))),
        }
        if conflicts > 0 {
            let both = if conflicts == 1 { "1 file changed on both sides".to_string() } else { format!("{} files changed on both sides", charon_core::human_count(conflicts)) };
            if text.is_empty() {
                text = format!("{both}: you choose which version to keep");
            } else {
                text.push_str(&format!(", and {both}"));
            }
        }
        if text.is_empty() {
            text = "A sync was held back".into();
        }
        text.push('.');
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(name: &str, items: u64, removals: u64, conflicts: u64) -> WaitingJob {
        WaitingJob { id: name.into(), name: name.into(), items, removals, conflicts, drive_connected: false }
    }

    #[test]
    fn waiting_messages_read_well() {
        let (title, body) = Notifier::waiting_message(&[waiting("Photos", 23, 3, 0)]).unwrap();
        assert_eq!(title, "“Photos” waits for your OK");
        assert_eq!(body, "23 changes are ready to sync, 3 of them removals. Open Charon to look at the list and confirm it.");
        let (_, body) = Notifier::waiting_message(&[waiting("Notes", 1, 0, 0)]).unwrap();
        assert_eq!(body, "1 change is ready to sync. Open Charon to look at the list and confirm it.");
        let (title, body) = Notifier::waiting_message(&[waiting("Notes", 0, 0, 2)]).unwrap();
        assert_eq!(title, "“Notes” has 2 conflicts to settle");
        assert_eq!(body, "2 files changed on both sides: you choose which version to keep. Open Charon to look at the list and confirm it.");
        let mut drive = waiting("Backup", 4, 0, 0);
        drive.drive_connected = true;
        let (_, body) = Notifier::waiting_message(&[drive]).unwrap();
        assert_eq!(body, "4 changes are ready to sync. Nothing has been copied yet: look at the list before you eject the drive.");
        let (title, body) = Notifier::waiting_message(&[waiting("A", 1, 0, 0), waiting("B", 2, 0, 1), waiting("C", 3, 1, 0)]).unwrap();
        assert_eq!(title, "3 jobs wait for your OK");
        assert_eq!(
            body,
            "“A”, “B” and “C”. 6 changes are ready to sync, 1 of them a removal, and 1 file changed on both sides. Open Charon to look at them and confirm."
        );
        assert!(Notifier::waiting_message(&[]).is_none());
    }
}
