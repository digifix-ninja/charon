//! The steps of a sync and the plan that holds them.

use crate::model::ConflictItem;
use crate::scan::Entry;
use serde::Serialize;

/// One of the two folders of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Source,
    Destination,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Source => Side::Destination,
            Side::Destination => Side::Source,
        }
    }
}

/// One step of a sync. Paths are relative to the job's folders.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Action {
    /// Create a folder on `side`.
    CreateDir { side: Side, path: String },
    /// Copy a file from the opposite side to `to`, replacing what's there if `replaces`.
    Copy { to: Side, path: String, bytes: u64, replaces: bool },
    /// Remove an item from `side` (into the trash, with safe deletes). A folder is removed
    /// with its contents when `whole`; otherwise only once nothing of value is left in it.
    Delete { side: Side, path: String, dir: bool, whole: bool },
    /// Both sides changed the file: keep the destination's version under a conflict name on
    /// both sides, and the source's version under the original name.
    KeepBoth { path: String, source_bytes: u64, destination_bytes: u64 },
    /// Both sides changed the file and the job asks what to do: nothing happens to it.
    Conflict { path: String, source: Option<Entry>, destination: Option<Entry> },
}

impl Action {
    pub fn path(&self) -> &str {
        match self {
            Action::CreateDir { path, .. }
            | Action::Copy { path, .. }
            | Action::Delete { path, .. }
            | Action::KeepBoth { path, .. }
            | Action::Conflict { path, .. } => path,
        }
    }

    /// Bytes this step carries.
    pub fn bytes(&self) -> u64 {
        match self {
            Action::Copy { bytes, .. } => *bytes,
            Action::KeepBoth { source_bytes, destination_bytes, .. } => source_bytes + destination_bytes,
            _ => 0,
        }
    }

    /// The order steps run in: removals first (they make room), then folders from the top
    /// down, then the copies.
    fn stage(&self) -> (u8, isize) {
        let depth = self.path().matches('/').count() as isize;
        match self {
            Action::Delete { whole: true, .. } | Action::Delete { dir: false, .. } => (0, 0),
            Action::Delete { .. } => (1, -depth), // deepest first
            Action::CreateDir { .. } => (2, depth),
            Action::Copy { .. } | Action::KeepBoth { .. } => (3, 0),
            Action::Conflict { .. } => (4, 0),
        }
    }
}

/// Everything one run would do.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Plan {
    pub actions: Vec<Action>,
}

impl Plan {
    pub fn push(&mut self, action: Action) {
        self.actions.push(action);
    }

    /// The steps in the order they must run (see `Action::stage`).
    pub fn ordered(&self) -> Vec<&Action> {
        let mut steps: Vec<&Action> = self.actions.iter().collect();
        steps.sort_by_key(|a| a.stage()); // stable: keeps path order within a stage
        steps
    }

    /// The steps that change something (conflicts left for the person don't).
    pub fn work(&self) -> impl Iterator<Item = &Action> {
        self.actions.iter().filter(|a| !matches!(a, Action::Conflict { .. }))
    }

    /// Whether the folders are already in sync (apart from conflicts waiting for a decision).
    pub fn is_settled(&self) -> bool {
        self.work().next().is_none()
    }

    /// The conflicts waiting for a decision, as the window shows them.
    pub fn conflicts(&self) -> Vec<ConflictItem> {
        self.actions
            .iter()
            .filter_map(|a| match a {
                Action::Conflict { path, source, destination } => Some(ConflictItem {
                    path: path.clone(),
                    source_modified: source.map(|e| e.modified),
                    destination_modified: destination.map(|e| e.modified),
                    source_size: source.map(|e| e.size),
                    destination_size: destination.map(|e| e.size),
                }),
                _ => None,
            })
            .collect()
    }

    /// Counts for the preview: "will copy 1,240 files (8.2 GB), delete 3".
    pub fn summary(&self) -> PlanSummary {
        let mut summary = PlanSummary::default();
        for action in &self.actions {
            match action {
                Action::CreateDir { .. } => summary.new_folders += 1,
                Action::Copy { to, bytes, .. } => {
                    summary.copies += 1;
                    summary.copy_bytes += bytes;
                    match to {
                        Side::Destination => summary.to_destination += 1,
                        Side::Source => summary.to_source += 1,
                    }
                }
                Action::Delete { side, .. } => {
                    summary.deletes += 1;
                    match side {
                        Side::Destination => summary.deletes_on_destination += 1,
                        Side::Source => summary.deletes_on_source += 1,
                    }
                }
                Action::KeepBoth { .. } => {
                    summary.kept_both += 1;
                    summary.copy_bytes += action.bytes();
                }
                Action::Conflict { .. } => summary.conflicts += 1,
            }
        }
        summary
    }
}

/// The counts a preview shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSummary {
    pub copies: u64,
    pub copy_bytes: u64,
    pub to_destination: u64,
    pub to_source: u64,
    pub new_folders: u64,
    pub deletes: u64,
    pub deletes_on_destination: u64,
    pub deletes_on_source: u64,
    pub kept_both: u64,
    pub conflicts: u64,
}

impl PlanSummary {
    /// Items that would change (what "pending" counts).
    pub fn changes(&self) -> u64 {
        self.copies + self.new_folders + self.deletes + self.kept_both
    }
}
