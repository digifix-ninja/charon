//! What the window shows when it asks for the person's OK: the list of changes a sync would
//! make (a *review*), and the notice shown once after the move to 0.3.

use crate::model::{SyncMode, Trigger, VolumeLabel};
use crate::plan::{NoteKind, Side};
use crate::scan::EntryKind;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a review is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewKind {
    /// The job's next sync.
    Sync,
    /// Settling one conflict the way the person chose.
    Resolve,
}

/// The list of changes a sync would make, waiting for the person's OK. Nothing changes until
/// it is confirmed, and then exactly this list is carried out (each step only if its items
/// are still as they were seen).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub review_id: String,
    pub job_id: String,
    pub job_name: String,
    pub kind: ReviewKind,
    /// Resolve: "Settle “notes/plan.txt”: keep the source's version".
    pub title: Option<String>,
    pub mode: SyncMode,
    pub safe_delete: bool,
    pub full_automation: bool,
    pub prepared_at: DateTime<Utc>,
    /// The list can't be confirmed after this; the person looks again.
    pub expires_at: DateTime<Utc>,
    pub source: Shore,
    pub destination: Shore,
    pub tally: Tally,
    pub warnings: Vec<Warning>,
    /// Why this list can't be confirmed at all, if it can't (one side looks empty…).
    pub blocked: Option<String>,
    /// Confirming needs "I've looked at the removals and replacements" ticked.
    pub needs_tick: bool,
    /// Whole folders removed, biggest first.
    pub clusters: Vec<Cluster>,
    /// What else the sync does besides the steps (emptying old trash…).
    pub housekeeping: Vec<String>,
    pub total_steps: usize,
    pub total_notes: usize,
    /// The first page of the list.
    pub steps: Vec<ReviewStep>,
    /// The first page of the notes.
    pub notes: Vec<ReviewNote>,
}

/// One of the two folders, as the review shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shore {
    pub path: String,
    pub volume: Option<VolumeLabel>,
    /// Files and links in it.
    pub files: u64,
}

/// The counts at the top of a review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tally {
    pub source: SideTally,
    pub destination: SideTally,
    pub keep_both: u64,
    /// Conflicts left for the person.
    pub conflicts: u64,
    /// Conflicts the newest version settles.
    pub newest_wins: u64,
    /// Files found identical on both sides apart from their dates: treated as in sync.
    pub agreed: u64,
    /// Bytes copied in all.
    pub bytes: u64,
}

impl Tally {
    pub fn side_mut(&mut self, side: Side) -> &mut SideTally {
        match side {
            Side::Source => &mut self.source,
            Side::Destination => &mut self.destination,
        }
    }
}

/// What happens on one side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideTally {
    pub new_files: u64,
    pub replaced: u64,
    /// Files and links.
    pub removed: u64,
    pub removed_folders: u64,
    pub new_folders: u64,
    pub bytes_in: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    /// Red: something may be wrong; confirming needs the tick.
    Danger,
    /// Amber: worth knowing before confirming.
    Caution,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub level: Level,
    pub text: String,
}

impl Warning {
    pub fn danger(text: impl Into<String>) -> Self {
        Self { level: Level::Danger, text: text.into() }
    }

    pub fn caution(text: impl Into<String>) -> Self {
        Self { level: Level::Caution, text: text.into() }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self { level: Level::Info, text: text.into() }
    }
}

/// A whole folder that goes, with what's in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cluster {
    pub side: Side,
    pub path: String,
    pub items: u64,
    pub bytes: u64,
}

/// What a step does, as the review shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepKind {
    /// A file or link that's new on this side.
    New,
    /// A newer version replaces this side's.
    Replace,
    Remove,
    /// A new folder.
    Folder,
    /// Both versions are kept (the destination's under a conflict name).
    KeepBoth,
    /// Left for the person.
    Conflict,
    /// A conflict settled by the newest version.
    Settle,
}

/// One step of a review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStep {
    pub kind: StepKind,
    /// The side it changes (`None`: both).
    pub side: Option<Side>,
    /// As spelled on disk.
    pub path: String,
    pub item: EntryKind,
    pub bytes: u64,
    /// Why, in words: "removed on the destination since the last sync".
    pub reason: String,
    /// It removes or replaces one of the person's files.
    pub danger: bool,
}

/// Something the person should know that isn't a step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNote {
    pub kind: NoteKind,
    pub side: Option<Side>,
    pub path: String,
    pub reason: String,
    /// "Leave this folder out" can add an exclude for it.
    pub can_leave_out: bool,
}

/// Which steps `review_steps` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepFilter {
    All,
    Removals,
    Replacements,
    Conflicts,
    New,
}

impl StepFilter {
    pub fn admits(self, step: &ReviewStep) -> bool {
        match self {
            StepFilter::All => true,
            StepFilter::Removals => step.kind == StepKind::Remove,
            StepFilter::Replacements => matches!(step.kind, StepKind::Replace | StepKind::Settle),
            StepFilter::Conflicts => matches!(step.kind, StepKind::Conflict | StepKind::KeepBoth | StepKind::Settle),
            StepFilter::New => matches!(step.kind, StepKind::New | StepKind::Folder),
        }
    }
}

/// One page of a list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
}

/// Shown once after the move to 0.3: jobs that used to sync by themselves now ask first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeNotice {
    pub version: String,
    pub jobs: Vec<UpgradedJob>,
    /// The change was written to each job's activity log.
    #[serde(default)]
    pub journaled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradedJob {
    pub id: String,
    pub name: String,
    pub trigger: Trigger,
}

/// A job whose changes wait for the person's OK, for a notification.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingJob {
    pub id: String,
    pub name: String,
    pub items: u64,
    pub removals: u64,
    /// Items changed on both sides, waiting for the person to choose.
    pub conflicts: u64,
    /// The job looks when its drive is connected: the person may be about to eject it.
    pub drive_connected: bool,
}
