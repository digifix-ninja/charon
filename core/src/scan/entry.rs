//! A file or folder as Charon sees it, and a whole folder as a sorted map of them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Some file systems (FAT, exFAT, SMB shares) store modification times in 2-second steps,
/// so times this close count as equal.
pub const MTIME_TOLERANCE_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Dir,
}

/// Size and modification time of one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: EntryKind,
    pub size: u64,
    /// Milliseconds since 1970.
    pub modified: i64,
}

impl Entry {
    pub fn file(size: u64, modified: i64) -> Self {
        Self { kind: EntryKind::File, size, modified }
    }

    pub fn dir() -> Self {
        Self { kind: EntryKind::Dir, size: 0, modified: 0 }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// Whether two items count as the same content: folders always do, files when size
    /// and modification time match.
    pub fn same_as(&self, other: &Entry) -> bool {
        match (self.kind, other.kind) {
            (EntryKind::Dir, EntryKind::Dir) => true,
            (EntryKind::File, EntryKind::File) => {
                self.size == other.size
                    && (self.modified - other.modified).abs() <= MTIME_TOLERANCE_MS
            }
            _ => false,
        }
    }

    /// `same_as` for items that may be missing: two missing items are the same too.
    pub fn same(a: Option<&Entry>, b: Option<&Entry>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same_as(b),
            _ => false,
        }
    }
}

/// Every item in a folder, keyed by its path relative to the folder, with `/` separators.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Snapshot {
    entries: BTreeMap<String, Entry>,
}

impl Snapshot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: impl Into<String>, entry: Entry) {
        self.entries.insert(path.into(), entry);
    }

    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.get(path)
    }

    pub fn remove(&mut self, path: &str) {
        self.entries.remove(path);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Entry)> {
        self.entries.iter()
    }

    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of files (not folders).
    pub fn file_count(&self) -> usize {
        self.entries.values().filter(|e| !e.is_dir()).count()
    }

    /// The items present and equal in both snapshots: the state two folders agree on.
    pub fn agreement(a: &Snapshot, b: &Snapshot) -> Snapshot {
        let entries = a
            .entries
            .iter()
            .filter(|(path, entry)| b.get(path).is_some_and(|other| entry.same_as(other)))
            .map(|(path, entry)| (path.clone(), *entry))
            .collect();
        Snapshot { entries }
    }
}

impl FromIterator<(String, Entry)> for Snapshot {
    fn from_iter<T: IntoIterator<Item = (String, Entry)>>(iter: T) -> Self {
        Snapshot { entries: iter.into_iter().collect() }
    }
}
