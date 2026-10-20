//! Items found missing on a drive that may show changes late, and since when.
//!
//! A network share or a drive run by a program can list a folder before it has loaded, or
//! from an old picture of it, so a run nobody looks at removes what follows from an item's
//! absence only once looks have found it missing for a while (see `engine::Missing`). When
//! each item was first found missing is kept here, by the clock on the wall: a job that
//! only syncs when its drive is connected, or a computer that restarts every day, would
//! otherwise start the wait over at every launch, and the removals would never happen.
//!
//! The record belongs to the folders it was made with: for other folders it isn't used.

use crate::error::Result;
use crate::model::{JobId, JobSetup};
use crate::plan::Side;
use crate::store::atomic;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

const VERSION: u32 = 1;

/// One item a job's looks found missing on a drive that may show changes late.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Absence {
    /// The side it's missing on.
    pub side: Side,
    /// The item's key (see `scan::Keys`).
    pub key: String,
    /// When a look first found it missing.
    pub since: DateTime<Utc>,
    /// A run nobody looked at left the removal that follows it for later.
    #[serde(default)]
    pub left_for_later: bool,
}

/// What's inside a record file.
#[derive(Serialize, Deserialize)]
struct StoredAbsences {
    version: u32,
    source: PathBuf,
    destination: PathBuf,
    items: Vec<Absence>,
}

/// The records of what each job's looks found missing, one file per job.
pub struct Absences {
    folder: PathBuf,
    /// Saves and removals happen one at a time.
    writing: Mutex<()>,
}

impl Absences {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder, writing: Mutex::new(()) }
    }

    /// The items the job's looks found missing, for its current folders. Empty when there is
    /// no record, or one Charon can't read (the waits then start over, the careful way).
    pub fn load(&self, job: &JobId, setup: &JobSetup) -> Vec<Absence> {
        let path = self.file(job);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                log::warn!("couldn't read {}: {e}", path.display());
                return Vec::new();
            }
        };
        match serde_json::from_slice::<StoredAbsences>(&bytes) {
            Ok(stored) if stored.version == VERSION && stored.source == setup.source && stored.destination == setup.destination => stored.items,
            Ok(_) => Vec::new(),
            Err(e) => {
                log::warn!("{} is damaged ({e}); not used", path.display());
                Vec::new()
            }
        }
    }

    /// Saves the items the job's looks found missing. None removes the file.
    pub fn save(&self, job: &JobId, setup: &JobSetup, items: &[Absence]) -> Result<()> {
        if items.is_empty() {
            return self.forget(job);
        }
        let stored = StoredAbsences { version: VERSION, source: setup.source.clone(), destination: setup.destination.clone(), items: items.to_vec() };
        let bytes = serde_json::to_vec(&stored)?;
        let _one_at_a_time = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        atomic::write(&self.file(job), &bytes)
    }

    /// Forgets the record (the job was removed, or its folders or mode changed).
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

    fn setup(source: &str, destination: &str) -> JobSetup {
        JobSetup {
            name: "t".into(),
            source: source.into(),
            destination: destination.into(),
            mode: SyncMode::Mirror,
            conflict: ConflictRule::Ask,
            trigger: Trigger::DriveConnected,
            excludes: vec![],
            safe_delete: true,
            full_automation: true,
        }
    }

    fn items() -> Vec<Absence> {
        let since = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        vec![Absence { side: Side::Source, key: "photos/cat.jpg".into(), since, left_for_later: true }]
    }

    #[test]
    fn a_record_only_serves_its_own_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let absences = Absences::new(tmp.path().to_path_buf());
        let job = "job".to_string();
        absences.save(&job, &setup("/nas/photos", "/backup"), &items()).unwrap();
        assert_eq!(absences.load(&job, &setup("/nas/photos", "/backup")), items());
        assert!(absences.load(&job, &setup("/nas/other", "/backup")).is_empty());
        // Nothing missing anymore: the record goes.
        absences.save(&job, &setup("/nas/photos", "/backup"), &[]).unwrap();
        assert!(!tmp.path().join("job.json").exists());
        absences.forget(&job).unwrap();
    }

    #[test]
    fn a_damaged_record_starts_every_wait_over() {
        let tmp = tempfile::tempdir().unwrap();
        let absences = Absences::new(tmp.path().to_path_buf());
        std::fs::write(tmp.path().join("job.json"), "{not json").unwrap();
        assert!(absences.load(&"job".to_string(), &setup("/a", "/b")).is_empty());
    }
}
