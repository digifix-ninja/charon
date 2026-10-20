//! The two-way memory of each job (see `plan::baseline`), with what ties it to its folders.
//!
//! A memory only means something for the two folders it was made with, so the file names
//! them, and it carries the token and counter of the markers Charon left in both folders
//! (see `run::marker`). A memory for other folders is never used: a job whose folders
//! changed starts fresh, and a fresh two-way run only merges, it never deletes.

use crate::error::Result;
use crate::model::{JobId, JobSetup};
use crate::plan::{Baseline, Side};
use crate::store::atomic;
use serde::{Deserialize, Serialize};
use crate::scan::Keys;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const VERSION: u32 = 2;

/// The memory of one job, as loaded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Memory {
    pub baseline: Baseline,
    /// The marker tokens of the source and the destination.
    pub tokens: Option<(String, String)>,
    /// The markers' counter at the last finished sync.
    pub generation: u64,
    /// Whether the keys ignore upper and lower case (see `Keys::fold`). A memory made with
    /// the other kind of keys can't be compared with today's scans.
    pub folded: bool,
    /// Why the stored memory can't be used, if it can't. The baseline is then empty.
    pub distrust: Option<String>,
    /// The items an older record Charon can't use (0.2's) had on both sides at the last
    /// sync. They decide nothing: they only tell which of the items a fresh comparison
    /// brings back were most likely removed since.
    pub earlier: BTreeSet<String>,
}

impl Memory {
    pub fn token(&self, side: Side) -> Option<&str> {
        self.tokens.as_ref().map(|(source, destination)| match side {
            Side::Source => source.as_str(),
            Side::Destination => destination.as_str(),
        })
    }

    /// Nothing remembered (the first sync), with the reason when it was thrown away.
    fn fresh(distrust: Option<String>) -> Self {
        Self { distrust, ..Self::default() }
    }
}

/// What's inside a memory file (version 2).
#[derive(Serialize, Deserialize)]
struct StoredMemory {
    version: u32,
    source: PathBuf,
    destination: PathBuf,
    source_token: String,
    destination_token: String,
    generation: u64,
    #[serde(default)]
    folded: bool,
    pairs: Baseline,
}

/// The file as it may be on disk: 0.2.0 and earlier stored a bare map of items.
#[derive(Deserialize)]
#[serde(untagged)]
enum AnyMemory {
    Current(StoredMemory),
    Legacy(serde_json::Map<String, serde_json::Value>),
}

pub struct Baselines {
    folder: PathBuf,
    /// Saves and removals happen one at a time.
    writing: Mutex<()>,
}

impl Baselines {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder, writing: Mutex::new(()) }
    }

    /// The job's memory for its current folders: empty before its first sync, and empty
    /// with a reason when the stored one can't be trusted.
    pub fn load(&self, job: &JobId, setup: &JobSetup) -> Memory {
        let path = self.file(job);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Memory::fresh(None),
            Err(e) => {
                log::error!("couldn't read {}: {e}", path.display());
                return Memory::fresh(Some(format!("Charon couldn't read what it remembered about the last sync ({e}).")));
            }
        };
        match serde_json::from_slice::<AnyMemory>(&bytes) {
            Ok(AnyMemory::Current(stored)) => {
                if Self::same_folder(&stored.source, &setup.source) && Self::same_folder(&stored.destination, &setup.destination) {
                    Memory {
                        baseline: stored.pairs,
                        tokens: Some((stored.source_token, stored.destination_token)),
                        generation: stored.generation,
                        folded: stored.folded,
                        distrust: None,
                        earlier: BTreeSet::new(),
                    }
                } else {
                    log::warn!("the two-way memory of job {job} belongs to other folders; starting fresh");
                    Memory::fresh(None)
                }
            }
            // An older Charon could save its memory for the wrong folders (when a job was
            // edited or two copies of the app ran at once), and nothing in the file says
            // which folders it was made with. It's never used to decide anything; its items
            // only tell what a fresh comparison brings back (see `Memory::earlier`).
            Ok(AnyMemory::Legacy(items)) => Memory {
                earlier: items.iter().filter(|(_, item)| item.get("kind").is_some()).map(|(path, _)| Keys::normalize(path)).collect(),
                ..Memory::fresh(Some("This job was last synced by an older version of Charon, whose memory of that sync can't be fully trusted.".into()))
            },
            Err(e) => {
                log::error!("{} is damaged: {e}", path.display());
                Memory::fresh(Some("Charon's memory of the last sync is damaged.".into()))
            }
        }
    }

    /// Saves the memory of a finished sync. Needs the marker tokens it goes with.
    pub fn save(&self, job: &JobId, setup: &JobSetup, memory: &Memory) -> Result<()> {
        let (source_token, destination_token) =
            memory.tokens.clone().ok_or_else(|| crate::Error::Invalid("a two-way memory needs its markers".into()))?;
        let stored = StoredMemory {
            version: VERSION,
            source: setup.source.clone(),
            destination: setup.destination.clone(),
            source_token,
            destination_token,
            generation: memory.generation,
            folded: memory.folded,
            pairs: memory.baseline.clone(),
        };
        let bytes = serde_json::to_vec(&stored)?;
        let _one_at_a_time = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        atomic::write(&self.file(job), &bytes)
    }

    /// Forgets the memory (the job was removed, or its folders or mode changed).
    pub fn forget(&self, job: &JobId) -> Result<()> {
        let _one_at_a_time = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_file(self.file(job)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(crate::Error::file(self.file(job), e)),
        }
    }

    fn same_folder(stored: &Path, current: &Path) -> bool {
        stored == current
    }

    fn file(&self, job: &JobId) -> PathBuf {
        self.folder.join(format!("{job}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConflictRule, SyncMode, Trigger};
    use crate::plan::Pair;
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

    fn memory() -> Memory {
        let mut baseline = Baseline::new();
        baseline.insert("a.txt", Pair::new(Entry::file(1, 10_000), Entry::file(1, 11_000)));
        Memory { baseline, tokens: Some(("s".into(), "d".into())), generation: 4, folded: true, distrust: None, earlier: BTreeSet::new() }
    }

    #[test]
    fn a_memory_only_serves_its_own_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let baselines = Baselines::new(tmp.path().to_path_buf());
        let id = "job".to_string();
        baselines.save(&id, &setup("/a", "/b"), &memory()).unwrap();
        assert_eq!(baselines.load(&id, &setup("/a", "/b")), memory());
        assert!(baselines.load(&id, &setup("/a", "/c")).baseline.is_empty());
        assert!(baselines.load(&id, &setup("/x", "/b")).baseline.is_empty());
    }

    #[test]
    fn the_old_format_is_never_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let baselines = Baselines::new(tmp.path().to_path_buf());
        let legacy = "{\"cafe\u{301}.txt\":{\"kind\":\"file\",\"size\":1,\"modified\":10000}}";
        std::fs::write(tmp.path().join("job.json"), legacy).unwrap();
        let loaded = baselines.load(&"job".to_string(), &setup("/a", "/b"));
        assert!(loaded.baseline.is_empty());
        assert!(loaded.distrust.is_some());
        assert_eq!(loaded.earlier, BTreeSet::from(["caf\u{e9}.txt".to_string()]), "only to tell what comes back");
    }

    #[test]
    fn a_damaged_file_means_a_fresh_start_with_a_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let baselines = Baselines::new(tmp.path().to_path_buf());
        std::fs::write(tmp.path().join("job.json"), "{not json").unwrap();
        let loaded = baselines.load(&"job".to_string(), &setup("/a", "/b"));
        assert!(loaded.baseline.is_empty());
        assert!(loaded.distrust.is_some());
    }

    #[test]
    fn forgetting_is_quiet_when_nothing_is_there() {
        let tmp = tempfile::tempdir().unwrap();
        let baselines = Baselines::new(tmp.path().to_path_buf());
        baselines.forget(&"nothing".to_string()).unwrap();
    }
}
