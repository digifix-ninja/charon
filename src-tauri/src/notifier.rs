//! System notifications: only when something needs the person, or a big sync is done.
//! Small routine syncs stay silent.

use crate::state::AppState;
use charon_core::{human_bytes, plural, Finished, JobId, WaitingJob};
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// A sync this big is worth a "finished" notification.
const BIG_SYNC_BYTES: u64 = 1_000_000_000;
const BIG_SYNC_FILES: u64 = 1_000;

pub struct Notifier;

/// The last problem told about each job. A job that runs by itself and fails the same way
/// every hour is told about once, until a run of it goes well again.
#[derive(Default)]
pub struct Repeats {
    told: Mutex<HashMap<JobId, String>>,
}

impl Repeats {
    /// Whether `problem` (title and text) about `job` is news. `None`: the run went well, so
    /// the next problem is news again.
    fn is_news(&self, job: &JobId, problem: Option<&(String, String)>) -> bool {
        let mut told = self.told.lock().unwrap_or_else(|e| e.into_inner());
        match problem {
            Some((title, body)) => {
                let said = format!("{title}\n{body}");
                told.insert(job.clone(), said.clone()).as_ref() != Some(&said)
            }
            None => {
                told.remove(job);
                true
            }
        }
    }
}

impl Notifier {
    /// Tells the person about a finished run, if it's worth it, and not about the same
    /// problem again and again.
    pub fn announce(app: &AppHandle, finished: &Finished) {
        let message = Self::finished(finished);
        let run = &finished.summary;
        if !run.cancelled {
            let failed = run.error.is_some() || run.failed > 0 || run.left_out > 0;
            let news = app.try_state::<Repeats>().is_none_or(|repeats| repeats.is_news(&finished.id, message.as_ref().filter(|_| failed)));
            if !news {
                log::info!("didn't tell again about {}: {}", finished.name, run.describe());
                return;
            }
        }
        if let Some((title, body)) = message {
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
        // Said once: the looks after the run list the file, so the next runs don't find it again.
        if run.found_unreadable > 0 {
            let body = if run.found_unreadable == 1 {
                "Another program has it open, or Charon isn't allowed to read it, so it isn't synced. Open Charon to see which one, and to leave it out or let Charon read it."
            } else {
                "Other programs have them open, or Charon isn't allowed to read them, so they aren't synced. Open Charon to see which ones, and to leave them out or let Charon read them."
            };
            return Some((format!("{name}: {} couldn't be read", plural(run.found_unreadable, "file")), body.into()));
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

    /// A run of the job “Documents”, as the engine records it, with `fields` set.
    fn documents(fields: serde_json::Value) -> Finished {
        let mut run = serde_json::json!({
            "startedAt": "2026-10-20T08:00:00Z",
            "finishedAt": "2026-10-20T08:01:00Z",
            "copied": 0,
            "deleted": 0,
            "bytes": 0,
            "conflicts": 0,
            "failed": 0,
            "cancelled": false,
            "error": null,
        });
        if let (Some(run), Some(fields)) = (run.as_object_mut(), fields.as_object()) {
            run.extend(fields.clone());
        }
        Finished { id: "documents".into(), name: "Documents".into(), summary: serde_json::from_value(run).unwrap() }
    }

    #[test]
    fn a_file_a_run_found_it_cannot_read_is_mentioned_once() {
        let (title, body) = Notifier::finished(&documents(serde_json::json!({ "unreadable": 1, "foundUnreadable": 1 }))).unwrap();
        assert_eq!(title, "Documents: 1 file couldn't be read");
        assert_eq!(
            body,
            "Another program has it open, or Charon isn't allowed to read it, so it isn't synced. Open Charon to see which one, and to leave it out or let Charon read it."
        );
        let (title, _) = Notifier::finished(&documents(serde_json::json!({ "unreadable": 3, "foundUnreadable": 2 }))).unwrap();
        assert_eq!(title, "Documents: 2 files couldn't be read");
        // Listed before this run: nothing new to tell.
        assert!(Notifier::finished(&documents(serde_json::json!({ "unreadable": 1 }))).is_none());
    }

    fn waiting(name: &str, items: u64, removals: u64, conflicts: u64) -> WaitingJob {
        WaitingJob { id: name.into(), name: name.into(), items, removals, conflicts, drive_connected: false }
    }

    #[test]
    fn the_same_problem_is_told_once_until_a_run_goes_well() {
        let repeats = Repeats::default();
        let job = "job".to_string();
        let failed = ("Photos didn't sync".to_string(), "The drive is full.".to_string());
        assert!(repeats.is_news(&job, Some(&failed)));
        assert!(!repeats.is_news(&job, Some(&failed)));
        assert!(repeats.is_news(&"other".to_string(), Some(&failed)), "each job on its own");
        let other = ("Photos didn't sync".to_string(), "A folder can't be read.".to_string());
        assert!(repeats.is_news(&job, Some(&other)), "another problem is news");
        assert!(repeats.is_news(&job, None));
        assert!(repeats.is_news(&job, Some(&other)), "after a good run, the problem is news again");
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
