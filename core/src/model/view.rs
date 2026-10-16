//! What the window receives: one row per job, live progress and run summaries.

use crate::model::{Job, JobId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The colour and icon of a job row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    /// Never synced yet.
    New,
    /// Green check: both folders match.
    InSync,
    /// Amber: changes are waiting to cross.
    Pending,
    /// Blue, spinning: a sync is running.
    Syncing,
    /// Grey: won't run by itself.
    Paused,
    /// Purple: a file changed on both sides and needs a decision.
    Conflict,
    /// Red: the last run failed or some files couldn't be copied.
    Error,
    /// Dimmed: a folder (or its drive) isn't reachable.
    Offline,
}

/// The stage a running sync is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Scanning,
    Copying,
    Finishing,
}

/// Live progress of a running sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: Phase,
    pub done_items: u64,
    pub total_items: u64,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// The file being carried right now.
    pub current: Option<String>,
}

impl Progress {
    /// Progress at the very start of a run.
    pub fn scanning() -> Self {
        Self {
            phase: Phase::Scanning,
            done_items: 0,
            total_items: 0,
            done_bytes: 0,
            total_bytes: 0,
            current: None,
        }
    }
}

/// Changes waiting to cross, found by the last check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub items: u64,
    pub bytes: u64,
    pub checked_at: DateTime<Utc>,
}

/// A file that changed on both sides and waits for a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictItem {
    pub path: String,
    pub source_modified: Option<i64>,
    pub destination_modified: Option<i64>,
    pub source_size: Option<u64>,
    pub destination_size: Option<u64>,
}

/// How the person settles a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictChoice {
    KeepSource,
    KeepDestination,
    KeepBoth,
}

/// The result of one run, stored with the job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub copied: u64,
    pub deleted: u64,
    pub bytes: u64,
    pub conflicts: u64,
    pub failed: u64,
    pub cancelled: bool,
    /// Why the whole run failed, if it did.
    pub error: Option<String>,
}

impl RunSummary {
    /// Whether the run finished without any problem.
    pub fn is_clean(&self) -> bool {
        self.error.is_none() && self.failed == 0 && !self.cancelled
    }

    /// One line for the log and notifications: "copied 12 files (8.2 MB), deleted 1".
    pub fn describe(&self) -> String {
        if let Some(error) = &self.error {
            return format!("failed: {error}");
        }
        if self.cancelled {
            return "stopped".into();
        }
        let mut parts = Vec::new();
        if self.copied > 0 {
            parts.push(format!(
                "copied {} ({})",
                plural(self.copied, "file"),
                human_bytes(self.bytes)
            ));
        }
        if self.deleted > 0 {
            parts.push(format!("removed {}", plural(self.deleted, "item")));
        }
        if self.conflicts > 0 {
            parts.push(format!("{} to resolve", plural(self.conflicts, "conflict")));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", plural(self.failed, "file")));
        }
        if parts.is_empty() {
            "already in sync".into()
        } else {
            parts.join(", ")
        }
    }
}

/// The row the window shows for one job.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    #[serde(flatten)]
    pub job: Job,
    pub status: JobStatus,
    pub pending: Option<Pending>,
    pub progress: Option<Progress>,
    pub conflicts: Vec<ConflictItem>,
    /// What's wrong, in words, when the status is Error or Offline.
    pub problem: Option<String>,
    pub source_online: bool,
    pub destination_online: bool,
    /// When the job will run by itself next, if it has a schedule.
    pub next_run: Option<DateTime<Utc>>,
}

/// One line per state for the tray icon and the header.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub jobs: usize,
    pub syncing: usize,
    pub pending: usize,
    pub attention: usize,
    pub offline: usize,
    pub paused_all: bool,
}

impl Overview {
    /// Counts the statuses of all rows.
    pub fn of(views: &[JobView], paused_all: bool) -> Self {
        let count = |wanted: &[JobStatus]| views.iter().filter(|v| wanted.contains(&v.status)).count();
        Self {
            jobs: views.len(),
            syncing: count(&[JobStatus::Syncing]),
            pending: count(&[JobStatus::Pending]),
            attention: count(&[JobStatus::Error, JobStatus::Conflict]),
            offline: count(&[JobStatus::Offline]),
            paused_all,
        }
    }

    /// A short sentence, e.g. "Syncing 2 jobs" or "All in sync".
    pub fn headline(&self) -> String {
        if self.jobs == 0 {
            "No sync jobs yet".into()
        } else if self.syncing > 0 {
            format!("Syncing {}", plural(self.syncing as u64, "job"))
        } else if self.attention > 0 {
            format!("{} need{} attention", plural(self.attention as u64, "job"), if self.attention == 1 { "s" } else { "" })
        } else if self.paused_all {
            "Everything paused".into()
        } else if self.pending > 0 {
            format!("{} with changes waiting", plural(self.pending as u64, "job"))
        } else {
            "All in sync".into()
        }
    }
}

/// "1 file", "3 files".
pub fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// "8.2 MB".
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A notification-worthy event: what the app shell may tell the person about.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finished {
    pub id: JobId,
    pub name: String,
    pub summary: RunSummary,
}
