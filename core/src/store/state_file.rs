//! `charon.json`: the settings and every job.

use crate::error::{Error, Result};
use crate::model::{Job, Settings};
use crate::store::atomic;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const VERSION: u32 = 1;

/// What's inside the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredState {
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub jobs: Vec<Job>,
}

impl Default for StoredState {
    fn default() -> Self {
        Self { version: VERSION, settings: Settings::default(), jobs: Vec::new() }
    }
}

pub struct StateFile {
    path: PathBuf,
}

impl StateFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Reads the file; a missing file means a fresh start. A damaged file is kept aside as
    /// `charon.json.damaged` rather than overwritten.
    pub fn load(&self) -> Result<StoredState> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StoredState::default()),
            Err(e) => return Err(Error::file(&self.path, e)),
        };
        match serde_json::from_slice(&bytes) {
            Ok(state) => Ok(state),
            Err(e) => {
                log::error!("{} is damaged ({e}); starting fresh", self.path.display());
                let _ = std::fs::rename(&self.path, self.path.with_extension("json.damaged"));
                Ok(StoredState::default())
            }
        }
    }

    pub fn save(&self, state: &StoredState) -> Result<()> {
        atomic::write(&self.path, &serde_json::to_vec_pretty(state)?)
    }
}
