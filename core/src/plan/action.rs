//! The steps of a sync, the notes that go with them, and the plan that holds both.

use crate::scan::{Entry, EntryKind, Keys};
use serde::{Deserialize, Serialize};

/// One of the two folders of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

    /// "source" or "destination", for sentences.
    pub fn word(self) -> &'static str {
        match self {
            Side::Source => "source",
            Side::Destination => "destination",
        }
    }
}

/// One step of a sync. Paths are keys relative to the job's folders (see `scan::Keys`).
///
/// Every step remembers the state it was planned against (`from`, `existing`, `entry`).
/// Right before it acts, the executor checks that the disk still looks like that; if
/// anything changed in between, the step is skipped instead of guessing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Action {
    /// Create a folder on `side` (the other side has it).
    CreateDir { side: Side, path: String },
    /// Copy a file or link from the opposite side to `to`, replacing what's there if `replaces`.
    /// `settles_conflict`: both sides had changed and the newer version wins; the version it
    /// replaces always goes to the trash, even without safe deletes.
    Copy {
        to: Side,
        path: String,
        item: EntryKind,
        bytes: u64,
        replaces: bool,
        settles_conflict: bool,
        #[serde(skip)]
        from: Entry,
        #[serde(skip)]
        existing: Option<Entry>,
    },
    /// Remove an item from `side` (into the trash, with safe deletes). A folder is only
    /// removed once nothing but system clutter is left in it. `other` is what the other side
    /// had there (usually nothing): if that changed, the removal no longer makes sense.
    Delete {
        side: Side,
        path: String,
        item: EntryKind,
        #[serde(skip)]
        entry: Entry,
        #[serde(skip)]
        other: Option<Entry>,
    },
    /// Both sides changed the file: the destination's version is kept under a conflict name
    /// on both sides, and the source's version under the original name.
    KeepBoth {
        path: String,
        source_bytes: u64,
        destination_bytes: u64,
        #[serde(skip)]
        source: Entry,
        #[serde(skip)]
        destination: Entry,
    },
    /// Both sides changed the item and it needs the person: nothing happens to it.
    Conflict { path: String, source: Option<Entry>, destination: Option<Entry> },
}

impl Action {
    /// A copy of `from` (from the other side) onto `to`, over `existing` if there is one.
    pub fn copy(to: Side, path: &str, from: Entry, existing: Option<Entry>) -> Self {
        Action::Copy {
            to,
            path: path.to_string(),
            item: from.kind,
            bytes: if from.is_file() { from.size } else { 0 },
            replaces: existing.is_some(),
            settles_conflict: false,
            from,
            existing,
        }
    }

    /// The same copy, marked as deciding a conflict (see `Action::Copy`).
    pub fn settling(mut self, settles: bool) -> Self {
        if let Action::Copy { settles_conflict, .. } = &mut self {
            *settles_conflict = settles;
        }
        self
    }

    /// Removing `entry` from `side`, where the other side has `other` (usually nothing).
    pub fn delete(side: Side, path: &str, entry: Entry, other: Option<Entry>) -> Self {
        Action::Delete { side, path: path.to_string(), item: entry.kind, entry, other }
    }

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

    /// The order steps run in (see `Plan::ordered`). `makes_room`: a removal that clears the
    /// way for an item of another kind.
    fn stage(&self, makes_room: bool) -> (u8, isize) {
        let depth = Keys::depth(self.path()) as isize;
        match self {
            Action::Delete { .. } if makes_room => (0, -depth),
            Action::CreateDir { .. } => (1, depth),
            Action::Copy { .. } | Action::KeepBoth { .. } => (2, 0),
            Action::Delete { .. } => (3, -depth),
            Action::Conflict { .. } => (4, 0),
        }
    }

    /// The order a review lists steps in: what deserves a look first.
    fn review_rank(&self) -> u8 {
        match self {
            Action::Delete { .. } => 0,
            Action::Copy { settles_conflict: true, .. } => 1,
            Action::Copy { replaces: true, .. } => 2,
            Action::KeepBoth { .. } => 3,
            Action::Conflict { .. } => 4,
            Action::CreateDir { .. } => 5,
            Action::Copy { .. } => 6,
        }
    }
}

/// Something the person should know about a plan that isn't a step: places that couldn't
/// be read, items left out, folders kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub kind: NoteKind,
    pub side: Option<Side>,
    /// The path as the person knows it (on-disk spelling where it matters).
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoteKind {
    /// Couldn't be read: nothing at or inside it is synced.
    Unreadable,
    /// Left out on purpose (links on Windows, special files, names that can't be carried).
    LeftOut,
    /// Two names that one of the drives would see as the same name.
    NameClash,
    /// A folder that should go but holds things Charon leaves alone.
    KeptFolder,
    /// Two kinds of item meet (file and folder) in a way this mode doesn't settle.
    Blocked,
}

/// Everything one run would do, and what it won't do.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub notes: Vec<Note>,
}

impl Plan {
    pub fn push(&mut self, action: Action) {
        self.actions.push(action);
    }

    pub fn note(&mut self, kind: NoteKind, side: Option<Side>, path: impl Into<String>, reason: impl Into<String>) {
        self.notes.push(Note { kind, side, path: path.into(), reason: reason.into() });
    }

    /// The steps in the order they must run:
    ///
    /// 1. removals that make room for an item of another kind (a folder that became a file),
    ///    with everything inside them, deepest first;
    /// 2. new folders, from the top down;
    /// 3. copies;
    /// 4. every other removal, deepest first.
    ///
    /// Removing last means that a run which stops halfway (cancelled, a drive pulled out, a
    /// full disk) never leaves an item that was moved or renamed on one side with only one
    /// copy: the new place is filled before the old one is cleared.
    pub fn ordered(&self) -> Vec<&Action> {
        self.ordered_steps().into_iter().map(|(action, _)| action).collect()
    }

    /// `ordered`, with whether each step is a removal that makes room for an item of
    /// another kind (step 1 above).
    pub fn ordered_steps(&self) -> Vec<(&Action, bool)> {
        let clearing: Vec<(Side, &str)> = self
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Delete { side, path, other: Some(_), .. } => Some((*side, path.as_str())),
                _ => None,
            })
            .collect();
        let makes_room = |a: &Action| match a {
            Action::Delete { side, path, .. } => clearing.iter().any(|(s, top)| s == side && Keys::is_within(path, top)),
            _ => false,
        };
        let mut steps: Vec<(u8, isize, &Action, bool)> = self
            .actions
            .iter()
            .map(|a| {
                let room = makes_room(a);
                let (stage, depth) = a.stage(room);
                (stage, depth, a, room)
            })
            .collect();
        steps.sort_by_key(|(stage, depth, _, _)| (*stage, *depth)); // stable: keeps path order within a stage
        steps.into_iter().map(|(_, _, a, room)| (a, room)).collect()
    }

    /// The steps in the order a review shows them.
    pub fn for_review(&self) -> Vec<&Action> {
        let mut steps: Vec<&Action> = self.actions.iter().collect();
        steps.sort_by(|a, b| a.review_rank().cmp(&b.review_rank()).then_with(|| a.path().cmp(b.path())));
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

    /// The conflicts waiting for a decision: key and both sides' items.
    pub fn conflicts(&self) -> Vec<(&str, Option<Entry>, Option<Entry>)> {
        self.actions
            .iter()
            .filter_map(|a| match a {
                Action::Conflict { path, source, destination } => Some((path.as_str(), *source, *destination)),
                _ => None,
            })
            .collect()
    }

    /// Files and links this plan removes from `side`.
    pub fn removed_on(&self, side: Side) -> u64 {
        self.actions
            .iter()
            .filter(|a| matches!(a, Action::Delete { side: s, item, .. } if *s == side && *item != EntryKind::Dir))
            .count() as u64
    }

    /// Files and links this plan removes or replaces on `side`: what could hurt if the plan
    /// were wrong.
    pub fn destructive_on(&self, side: Side) -> u64 {
        self.destructive_paths_on(side).count() as u64
    }

    /// The paths of `destructive_on`.
    pub fn destructive_paths_on(&self, side: Side) -> impl Iterator<Item = &str> {
        self.actions.iter().filter_map(move |a| {
            let destructive = match a {
                Action::Delete { side: s, item, .. } => *s == side && *item != EntryKind::Dir,
                Action::Copy { to, replaces: true, .. } => *to == side,
                Action::KeepBoth { .. } => side == Side::Destination,
                _ => false,
            };
            destructive.then(|| a.path())
        })
    }

    /// Counts for the review: "will copy 1,240 files (8.2 GB), replace 3, remove 3".
    pub fn summary(&self) -> PlanSummary {
        let mut summary = PlanSummary::default();
        for action in &self.actions {
            match action {
                Action::CreateDir { .. } => summary.new_folders += 1,
                Action::Copy { to, bytes, replaces, settles_conflict, .. } => {
                    summary.copies += 1;
                    summary.copy_bytes += bytes;
                    if *replaces {
                        summary.replaces += 1;
                    }
                    if *settles_conflict {
                        summary.newest_wins += 1;
                    }
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
        summary.notes = self.notes.len() as u64;
        summary
    }
}

/// The counts a review shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSummary {
    pub copies: u64,
    pub copy_bytes: u64,
    pub to_destination: u64,
    pub to_source: u64,
    /// Copies that replace an existing file (the old version goes to the trash with safe deletes).
    pub replaces: u64,
    pub new_folders: u64,
    pub deletes: u64,
    pub deletes_on_destination: u64,
    pub deletes_on_source: u64,
    pub kept_both: u64,
    /// Conflicts the newest version wins (counted among the copies and replacements too).
    pub newest_wins: u64,
    pub conflicts: u64,
    /// Things the person should know that aren't steps (unreadable places, kept folders…).
    pub notes: u64,
}

impl PlanSummary {
    /// Items that would change (what "pending" counts).
    pub fn changes(&self) -> u64 {
        self.copies + self.new_folders + self.deletes + self.kept_both
    }
}
