//! What Charon keeps on disk, all inside the app's data folder.
//!
//! * `state_file.rs` – `StateFile`: settings and jobs (`charon-state.json`), saved atomically
//! * `baselines.rs`  – `Baselines`: the two-way memory of each job
//! * `landings.rs`   – `Landings`: Mirror and Backup copies whose dates didn't stick
//! * `journal.rs`    – `Journal`: one activity log per job
//! * `lock.rs`       – `DataLock`: only one Charon works with the data folder at a time
//! * `atomic.rs`     – writing a file so a crash never leaves it half-written

pub(crate) mod atomic;
mod baselines;
mod journal;
mod landings;
mod lock;
mod state_file;

pub use baselines::{Baselines, Memory};
pub use journal::{Journal, LogLine};
pub use landings::Landings;
pub use lock::DataLock;
pub use state_file::{StateFile, StoredState};

use std::path::{Path, PathBuf};

/// Where everything lives inside the data folder.
#[derive(Debug, Clone)]
pub struct DataFolder {
    root: PathBuf,
}

impl DataFolder {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state_file(&self) -> PathBuf {
        self.root.join("charon-state.json")
    }

    /// Where Charon 0.2 kept its settings and jobs.
    pub fn legacy_state_file(&self) -> PathBuf {
        self.root.join("charon.json")
    }

    pub fn baselines(&self) -> PathBuf {
        self.root.join("baselines")
    }

    /// Mirror and Backup: the copies whose dates didn't stick (see `Landings`).
    pub fn landings(&self) -> PathBuf {
        self.root.join("landings")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
}
