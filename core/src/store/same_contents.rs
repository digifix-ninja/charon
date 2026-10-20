//! Two-way: pairs of files a full read found identical, with how both sides looked then.
//!
//! A background look reads only so much to compare files by their contents (see
//! `Engine::compare_contents`), and a pair it leaves unread is not compared yet: never "in
//! sync". A large file whose details alone changed on one side (a permission, a tag, an app
//! noting that it was opened) would then be "not compared yet" after every launch, though a
//! list read it and found both sides identical, until some other change was synced.
//!
//! So Charon keeps what full reads found: each pair found identical, with both sides' items
//! as they were then, change times included. A background look that can't read a pair goes
//! by this record only while both sides are still exactly as recorded: any write to either
//! file moves its change time, and the record no longer matches. Only pairs whose drives both
//! keep change times are recorded (see `Volume::keeps_change_times`): elsewhere an edit that
//! puts the old time back couldn't be told from no change at all.
//!
//! The record only ever says "these two files were identical when both looked like this". It
//! never decides a removal or a copy: lists and runs read the files themselves. It belongs to
//! the folders it was made with, and it goes with the job's memory of the last sync.

use crate::error::Result;
use crate::model::{JobId, JobSetup};
use crate::plan::{Baseline, Pair};
use crate::store::atomic;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

const VERSION: u32 = 1;

/// What's inside a record file.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSameContents {
    version: u32,
    source: PathBuf,
    destination: PathBuf,
    /// Whether the keys ignore upper and lower case (see `Keys::fold`).
    folded: bool,
    pairs: Baseline,
}

/// The records of the pairs each job's reads found identical, one file per job.
pub struct SameContents {
    folder: PathBuf,
    /// Saves and removals happen one at a time.
    writing: Mutex<()>,
}

impl SameContents {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder, writing: Mutex::new(()) }
    }

    /// Whether a pair found identical can be kept for later looks: two files whose change
    /// times are both known, so any write to either one makes the record miss.
    pub fn can_keep(pair: &Pair) -> bool {
        let lasting = |entry: &crate::scan::Entry| entry.is_file() && entry.changed != 0;
        lasting(&pair.source) && lasting(&pair.destination)
    }

    /// The pairs found identical in the job's current folders, keyed `folded` or not. Empty
    /// when there is no record, or one Charon can't use: the pairs are then read again, or
    /// are not compared yet.
    pub fn load(&self, job: &JobId, setup: &JobSetup, folded: bool) -> Baseline {
        let path = self.file(job);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Baseline::new(),
            Err(e) => {
                log::warn!("couldn't read {}: {e}", path.display());
                return Baseline::new();
            }
        };
        match serde_json::from_slice::<StoredSameContents>(&bytes) {
            Ok(stored) if stored.version == VERSION && stored.source == setup.source && stored.destination == setup.destination && stored.folded == folded => {
                stored.pairs
            }
            Ok(_) => Baseline::new(),
            Err(e) => {
                log::warn!("{} is damaged ({e}); not used", path.display());
                Baseline::new()
            }
        }
    }

    /// Saves the pairs found identical, keyed `folded` or not, keeping only those `can_keep`
    /// lets through. None removes the file.
    pub fn save(&self, job: &JobId, setup: &JobSetup, folded: bool, pairs: &Baseline) -> Result<()> {
        let mut pairs = pairs.clone();
        pairs.retain(|_, pair| Self::can_keep(pair));
        if pairs.is_empty() {
            return self.forget(job);
        }
        let stored = StoredSameContents { version: VERSION, source: setup.source.clone(), destination: setup.destination.clone(), folded, pairs };
        let bytes = serde_json::to_vec(&stored)?;
        let _one_at_a_time = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        atomic::write(&self.file(job), &bytes)
    }

    /// Forgets the record (the job was removed, its folders or mode changed, or Charon forgot
    /// its memory of the last sync).
    pub fn forget(&self, job: &JobId) -> Result<()> {
        let _one_at_a_time = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_file(self.file(job)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(crate::Error::file(self.file(job), e)),
        }
    }

    fn file(&self, job: &JobId) -> PathBuf {
        self.folder.join(format!("{job}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConflictRule, SyncMode, Trigger};
    use crate::scan::Entry;

    fn setup(source: &str, destination: &str) -> JobSetup {
        JobSetup {
            name: "t".into(),
            source: source.into(),
            destination: destination.into(),
            mode: SyncMode::TwoWay,
            conflict: ConflictRule::Ask,
            trigger: Trigger::Manual,
            excludes: vec![],
            safe_delete: true,
            full_automation: false,
        }
    }

    /// A large file found identical on both sides, read after its permissions changed on one.
    fn pairs() -> Baseline {
        let mut pairs = Baseline::new();
        let vault = |changed: i64| Entry::file(1 << 31, 1_700_000_000_000).changed_at(changed);
        pairs.insert("vault.hc", Pair::new(vault(1_700_000_100_000_000_000), vault(1_700_000_000_000_000_000)));
        pairs
    }

    #[test]
    fn a_record_only_serves_its_own_folders_and_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let same = SameContents::new(tmp.path().to_path_buf());
        let job = "job".to_string();
        same.save(&job, &setup("/a", "/b"), false, &pairs()).unwrap();
        assert_eq!(same.load(&job, &setup("/a", "/b"), false), pairs());
        assert!(same.load(&job, &setup("/a", "/c"), false).is_empty());
        assert!(same.load(&job, &setup("/x", "/b"), false).is_empty());
        assert!(same.load(&job, &setup("/a", "/b"), true).is_empty(), "keys made the other way");
        same.forget(&job).unwrap();
        assert!(same.load(&job, &setup("/a", "/b"), false).is_empty());
        same.forget(&job).unwrap();
    }

    /// Where a drive keeps no change time, an edit that puts the old time back looks like no
    /// change at all: a record of such a pair could stand for a file that was edited since.
    #[test]
    fn only_pairs_whose_change_times_are_known_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let same = SameContents::new(tmp.path().to_path_buf());
        let job = "job".to_string();
        let file = Entry::file(1 << 31, 1_700_000_000_000);
        let timed = file.changed_at(1_700_000_000_000_000_000);
        let mut found = pairs();
        found.insert("on exFAT.mov", Pair::new(timed, file));
        found.insert("on a share.mov", Pair::new(file, file));
        same.save(&job, &setup("/a", "/b"), false, &found).unwrap();
        assert_eq!(same.load(&job, &setup("/a", "/b"), false), pairs());

        let mut untimed = Baseline::new();
        untimed.insert("on a share.mov", Pair::new(file, file));
        same.save(&job, &setup("/a", "/b"), false, &untimed).unwrap();
        assert!(!tmp.path().join("job.json").exists(), "nothing to keep leaves no file");
    }

    #[test]
    fn a_damaged_record_is_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        let same = SameContents::new(tmp.path().to_path_buf());
        std::fs::write(tmp.path().join("job.json"), "{not json").unwrap();
        assert!(same.load(&"job".to_string(), &setup("/a", "/b"), false).is_empty());
    }
}
