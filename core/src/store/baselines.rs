//! The last state both folders of a two-way job agreed on (see `plan::two_way`).

use crate::error::Result;
use crate::model::JobId;
use crate::scan::Snapshot;
use crate::store::atomic;
use std::path::PathBuf;

pub struct Baselines {
    folder: PathBuf,
}

impl Baselines {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }

    /// The job's baseline; empty before its first sync.
    pub fn load(&self, job: &JobId) -> Snapshot {
        std::fs::read(self.file(job))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, job: &JobId, snapshot: &Snapshot) -> Result<()> {
        atomic::write(&self.file(job), &serde_json::to_vec(snapshot)?)
    }

    /// Forgets the baseline (the job was removed, or its folders changed).
    pub fn forget(&self, job: &JobId) {
        let _ = std::fs::remove_file(self.file(job));
    }

    fn file(&self, job: &JobId) -> PathBuf {
        self.folder.join(format!("{job}.json"))
    }
}
