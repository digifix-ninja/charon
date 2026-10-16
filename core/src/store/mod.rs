//! What Charon keeps on disk, all inside the app's data folder.
//!
//! * `state_file.rs` – `StateFile`: settings and jobs (`charon.json`), saved atomically
//! * `baselines.rs`  – `Baselines`: the last agreed state of each two-way job
//! * `journal.rs`    – `Journal`: one activity log per job
//! * `atomic.rs`     – writing a file so a crash never leaves it half-written

mod atomic;
mod baselines;
mod journal;
mod state_file;

pub use baselines::Baselines;
pub use journal::{Journal, LogLine};
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
        self.root.join("charon.json")
    }

    pub fn baselines(&self) -> PathBuf {
        self.root.join("baselines")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
}
