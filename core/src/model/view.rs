//! What the window receives: one row per job, live progress and run summaries.

use crate::model::{Job, JobId};
use crate::plan::{NoteKind, Side, Unit};
use crate::scan::{Entry, EntryKind, Volume};
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
    /// Charon hasn't looked at the folders since it started (or since a folder came back),
    /// and a look is on its way: whether they match isn't known yet.
    Looking,
    /// Amber: changes are waiting to cross, and the job will carry them by itself
    /// (full automation).
    Pending,
    /// Changes wait for the person's OK, or a run nobody looked at was held back for one.
    Review,
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

/// An item that changed on both sides and waits for a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictItem {
    /// The item's key in the plan (see `scan::Keys`).
    pub key: String,
    /// The path as spelled on disk, for the person.
    pub path: String,
    /// What each side has: a file, a folder or a link (`None`: deleted there).
    pub source_kind: Option<EntryKind>,
    pub destination_kind: Option<EntryKind>,
    pub source_modified: Option<i64>,
    pub destination_modified: Option<i64>,
    pub source_size: Option<u64>,
    pub destination_size: Option<u64>,
    /// "an app library or document" or "a database": the item's files only work together,
    /// so one side's copy is kept as a whole (never both).
    #[serde(default)]
    pub whole: Option<String>,
}

impl ConflictItem {
    pub fn new(key: &str, path: &str, source: Option<Entry>, destination: Option<Entry>) -> Self {
        Self {
            key: key.to_string(),
            path: path.to_string(),
            source_kind: source.map(|e| e.kind),
            destination_kind: destination.map(|e| e.kind),
            source_modified: source.map(|e| e.modified),
            destination_modified: destination.map(|e| e.modified),
            source_size: source.map(|e| e.size),
            destination_size: destination.map(|e| e.size),
            whole: None,
        }
    }

    /// The same conflict, about a whole app library or database (see `plan::Unit`).
    pub fn whole(mut self, unit: Option<&Unit>) -> Self {
        self.whole = unit.map(|unit| unit.describe().to_string());
        self
    }
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
    /// Steps left out because the item changed after the scan (or the review). The next
    /// look picks them up again.
    #[serde(default)]
    pub skipped: u64,
    /// Folders kept because something is still in them.
    #[serde(default)]
    pub kept: u64,
    /// Places that couldn't be read, so nothing in them was synced.
    #[serde(default)]
    pub unreadable: u64,
    /// Conflicts settled by Newest wins: the older version went to .charon-trash.
    #[serde(default)]
    pub settled: u64,
    /// Files copied without some of their details (Finder tags, a resource fork, extended
    /// attributes), because the receiving drive can't keep them.
    #[serde(default)]
    pub unkept: u64,
    pub cancelled: bool,
    /// Why the whole run failed, if it did.
    pub error: Option<String>,
    /// The person confirmed this run's list (rather than a run by itself).
    #[serde(default)]
    pub approved: bool,
}

impl RunSummary {
    /// An empty summary of a run that started at `started_at`.
    pub fn starting(started_at: DateTime<Utc>, approved: bool) -> Self {
        Self {
            started_at,
            finished_at: started_at,
            copied: 0,
            deleted: 0,
            bytes: 0,
            conflicts: 0,
            failed: 0,
            skipped: 0,
            kept: 0,
            unreadable: 0,
            settled: 0,
            unkept: 0,
            cancelled: false,
            error: None,
            approved,
        }
    }

    /// Whether the run finished without a problem. Places that couldn't be read are not a
    /// problem of the run (nothing there was touched); the row shows them on their own.
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
        if self.settled > 0 {
            parts.push(format!("{} settled by Newest wins", plural(self.settled, "conflict")));
        }
        if self.conflicts > 0 {
            parts.push(format!("{} to resolve", plural(self.conflicts, "conflict")));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", plural(self.failed, "file")));
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped (changed meanwhile)", plural(self.skipped, "item")));
        }
        if self.kept > 0 {
            parts.push(format!("kept {} that still hold files", plural(self.kept, "folder")));
        }
        if self.unreadable > 0 {
            parts.push(format!("{} couldn't be read", plural(self.unreadable, "place")));
        }
        if self.unkept > 0 {
            let their = if self.unkept == 1 { "its" } else { "their" };
            parts.push(format!("{} arrived without {their} Finder tags or other details", plural(self.unkept, "file")));
        }
        if parts.is_empty() {
            "already in sync".into()
        } else {
            parts.join(", ")
        }
    }
}

/// A number for each of the two folders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerSide {
    pub source: u64,
    pub destination: u64,
}

impl PerSide {
    pub fn on(&self, side: Side) -> u64 {
        match side {
            Side::Source => self.source,
            Side::Destination => self.destination,
        }
    }

    pub fn add(&mut self, side: Side, n: u64) {
        match side {
            Side::Source => self.source += n,
            Side::Destination => self.destination += n,
        }
    }

    pub fn total(&self) -> u64 {
        self.source + self.destination
    }
}

/// Changes that wait for the person's OK. Shown on the row whatever its status is (a paused
/// job, or one with conflicts or errors, can have changes waiting too).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    pub items: u64,
    pub bytes: u64,
    /// Files and links that would be removed on each side.
    pub removals: PerSide,
    /// Files and links that would be replaced on each side.
    pub replacements: PerSide,
    pub conflicts: u64,
    /// Why a run nobody looked at was held back for a look, if one was.
    pub held: Option<String>,
    /// Since when changes have been waiting.
    pub since: DateTime<Utc>,
    /// False when the numbers above aren't known yet: changes waited for an OK when Charon
    /// last looked, before it was closed (or before a folder went away), and it hasn't
    /// looked again since.
    pub counted: bool,
}

/// A place the last look couldn't read or had to leave out: nothing there is synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    /// Unreadable, left out (the other drive can't hold it, a special file), a name clash,
    /// or blocked (a file meeting a folder).
    pub kind: NoteKind,
    pub side: Side,
    /// As spelled on disk, relative to the job's folder.
    pub path: String,
    pub reason: String,
    /// macOS refused to let Charon read it (Privacy & Security settings).
    pub privacy: bool,
}

/// The most places a row lists (unreadable, and not synced); the rest are counted.
pub const PLACES_SHOWN: usize = 100;

/// A drive, the way the person knows it: "Backup SSD", "exFAT".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeLabel {
    pub name: String,
    pub kind: String,
}

impl VolumeLabel {
    pub fn of(volume: &Volume) -> Self {
        Self { name: volume.name.clone(), kind: volume.kind_label() }
    }
}

/// The row the window shows for one job.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    #[serde(flatten)]
    pub job: Job,
    /// Grows with every view the engine makes; the window ignores a view older than the one
    /// it has.
    pub rev: u64,
    pub status: JobStatus,
    /// Full automation is on and nothing waits for a look first.
    pub runs_by_itself: bool,
    /// Charon is looking at the folders right now (a background look, or a list being
    /// made); what the row says may change when it's done.
    pub looking: bool,
    pub waiting: Option<Waiting>,
    /// Changes the job will carry by itself (full automation).
    pub pending: Option<Pending>,
    pub progress: Option<Progress>,
    pub conflicts: Vec<ConflictItem>,
    /// Places the last look couldn't read (at most `PLACES_SHOWN`).
    pub unreadable: Vec<Place>,
    /// Items the last look had to leave out, name clashes and blocked items: they aren't
    /// synced (at most `PLACES_SHOWN`; `not_synced_count` counts them all).
    pub not_synced: Vec<Place>,
    pub not_synced_count: u64,
    /// What's wrong, in words, when the status is Error or Offline.
    pub problem: Option<String>,
    pub source_online: bool,
    pub destination_online: bool,
    pub source_volume: Option<VolumeLabel>,
    pub destination_volume: Option<VolumeLabel>,
    /// When the schedule next comes due: a sync if the job runs by itself, else a look.
    pub next_run: Option<DateTime<Utc>>,
    /// "Not synced for 3 days", "Changes have waited for your OK since Monday".
    pub stale: Option<String>,
    /// A sync is due but waits for this other job, which is syncing the same folders.
    pub waiting_for: Option<String>,
}

/// One line per state for the tray icon and the header.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub jobs: usize,
    pub syncing: usize,
    pub pending: usize,
    /// Jobs with changes waiting for the person's OK.
    pub waiting: usize,
    pub attention: usize,
    pub offline: usize,
    /// Jobs Charon hasn't looked at since it started or since a folder came back.
    pub looking: usize,
    /// Jobs that sync, but leave places out (unreadable, left out, name clashes).
    pub incomplete: usize,
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
            waiting: views.iter().filter(|v| v.waiting.is_some()).count(),
            attention: count(&[JobStatus::Error, JobStatus::Conflict]),
            offline: count(&[JobStatus::Offline]),
            looking: count(&[JobStatus::Looking]),
            incomplete: views.iter().filter(|v| !v.unreadable.is_empty() || v.not_synced_count > 0).count(),
            paused_all,
        }
    }

    /// A short sentence, e.g. "Syncing 2 jobs" or "All in sync".
    pub fn headline(&self) -> String {
        if self.jobs == 0 {
            "No sync jobs yet".into()
        } else if self.syncing > 0 {
            format!("Syncing {}", plural(self.syncing as u64, "job"))
        } else if self.attention > 0 && self.waiting > 0 {
            // Changes that wait for an OK are never hidden behind a job that needs attention.
            format!(
                "{} need{} attention, {} wait{} for your OK",
                plural(self.attention as u64, "job"),
                if self.attention == 1 { "s" } else { "" },
                human_count(self.waiting as u64),
                if self.waiting == 1 { "s" } else { "" }
            )
        } else if self.attention > 0 {
            format!("{} need{} attention", plural(self.attention as u64, "job"), if self.attention == 1 { "s" } else { "" })
        } else if self.waiting > 0 {
            format!("{} wait{} for your OK", plural(self.waiting as u64, "job"), if self.waiting == 1 { "s" } else { "" })
        } else if self.paused_all {
            "Everything paused".into()
        } else if self.pending > 0 {
            format!("{} with changes waiting", plural(self.pending as u64, "job"))
        } else if self.incomplete > 0 {
            format!("{} {} sync everything", plural(self.incomplete as u64, "job"), if self.incomplete == 1 { "doesn't" } else { "don't" })
        } else if self.looking > 0 {
            "Looking for changes…".into()
        } else {
            "All in sync".into()
        }
    }
}

/// "1 file", "3 files", "1,240 files".
pub fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{} {word}s", human_count(n))
    }
}

/// "1,240".
pub fn human_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
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
