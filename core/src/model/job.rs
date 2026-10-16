//! A sync job: two folders and the rules for carrying files between them.

use crate::error::{Error, Result};
use crate::model::RunSummary;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type JobId = String;

/// How files cross between the two folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
    /// One-way: the destination becomes an exact copy, deletions carry over.
    Mirror,
    /// One-way: new and changed files are copied, nothing is ever deleted on the destination.
    Backup,
    /// Changes flow both ways; Charon remembers the last synced state to tell
    /// "deleted here" apart from "new there".
    TwoWay,
}

impl SyncMode {
    pub fn is_two_way(self) -> bool {
        self == Self::TwoWay
    }
}

/// What happens when a file changed on both sides since the last sync (two-way only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictRule {
    /// Keep both: the destination's version is kept next to it as "name (conflict …)".
    KeepBoth,
    /// The most recently modified version wins.
    NewestWins,
    /// Leave the file alone and ask in the window.
    Ask,
}

/// When a job runs by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Trigger {
    /// Only when "Sync now" is pressed.
    Manual,
    /// A few seconds after changes in the folder settle down.
    OnChange { settle_seconds: u32 },
    /// Every N minutes.
    Interval { minutes: u32 },
    /// Once a day at a local time.
    Daily { hour: u8, minute: u8 },
    /// As soon as both folders become reachable (an external drive is plugged in).
    DriveConnected,
}

impl Trigger {
    /// Whether this trigger wants the folders watched for changes.
    pub fn syncs_on_change(self) -> bool {
        matches!(self, Self::OnChange { .. })
    }
}

/// Everything the person chooses when creating or editing a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSetup {
    pub name: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub mode: SyncMode,
    pub conflict: ConflictRule,
    pub trigger: Trigger,
    /// Patterns like `.DS_Store`, `node_modules` or `*.tmp`. Without a `/` a pattern
    /// matches a name anywhere; with one, a path from the folder's root.
    pub excludes: Vec<String>,
    /// Deleted and overwritten files go to a `.charon-trash` folder instead of vanishing.
    pub safe_delete: bool,
}

impl JobSetup {
    /// Exclude patterns a new job starts with: system clutter nobody wants copied.
    pub fn default_excludes() -> Vec<String> {
        [
            ".DS_Store",
            "._*",
            ".Spotlight-V100",
            ".Trashes",
            ".fseventsd",
            "Thumbs.db",
            "desktop.ini",
            "$RECYCLE.BIN",
            "~$*",
            "*.tmp",
        ]
        .map(String::from)
        .to_vec()
    }

    /// A name built from the folder names, like "Photos → Backup SSD".
    pub fn suggested_name(source: &Path, destination: &Path, mode: SyncMode) -> String {
        let arrow = if mode.is_two_way() { "⇄" } else { "→" };
        format!("{} {arrow} {}", Self::folder_name(source), Self::folder_name(destination))
    }

    /// Checks that the setup makes sense before it's saved.
    pub fn validate(&mut self) -> Result<()> {
        self.name = self.name.trim().to_string();
        self.excludes = self
            .excludes
            .iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();

        if self.source.as_os_str().is_empty() || self.destination.as_os_str().is_empty() {
            return Err(Error::Invalid("Choose both a source and a destination folder.".into()));
        }
        if !self.source.is_absolute() || !self.destination.is_absolute() {
            return Err(Error::Invalid("Folders must be full paths.".into()));
        }
        if self.source == self.destination {
            return Err(Error::Invalid("Source and destination are the same folder.".into()));
        }
        if self.destination.starts_with(&self.source) || self.source.starts_with(&self.destination) {
            return Err(Error::Invalid(
                "One folder is inside the other. Charon would end up copying its own copies.".into(),
            ));
        }
        self.trigger = match self.trigger {
            Trigger::OnChange { settle_seconds } => Trigger::OnChange {
                settle_seconds: settle_seconds.clamp(1, 3600),
            },
            Trigger::Interval { minutes } => Trigger::Interval {
                minutes: minutes.clamp(1, 7 * 24 * 60),
            },
            Trigger::Daily { hour, minute } => Trigger::Daily {
                hour: hour.min(23),
                minute: minute.min(59),
            },
            other => other,
        };
        if self.name.is_empty() {
            self.name = Self::suggested_name(&self.source, &self.destination, self.mode);
        }
        Ok(())
    }

    fn folder_name(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }
}

/// A stored sync job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: JobId,
    #[serde(flatten)]
    pub setup: JobSetup,
    /// Paused jobs don't run by themselves ("Sync now" still works).
    pub paused: bool,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<RunSummary>,
    /// When the last *successful* sync finished.
    pub last_synced: Option<DateTime<Utc>>,
}

impl Job {
    /// A new job with a fresh id.
    pub fn new(setup: JobSetup) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            setup,
            paused: false,
            created_at: Utc::now(),
            last_run: None,
            last_synced: None,
        }
    }

    /// The time automatic schedules count from: the last run, or the job's creation.
    pub fn schedule_reference(&self) -> DateTime<Utc> {
        self.last_run
            .as_ref()
            .map(|run| run.finished_at)
            .unwrap_or(self.created_at)
    }
}
