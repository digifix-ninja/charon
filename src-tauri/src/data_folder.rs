//! Where Charon keeps its jobs, settings and logs.

use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// The app's identifier before 0.1.1, when Charon was published by DigiFix.
const OLD_IDENTIFIER: &str = "ninja.digifix.charon";

pub struct DataFolder;

impl DataFolder {
    /// The data folder (macOS: ~/Library/Application Support/dev.stean.charon/).
    /// The first time 0.1.1 or later starts, it moves the folder used by 0.1.0 into
    /// place, so jobs and settings carry over to the new identifier.
    pub fn open(app: &AppHandle) -> tauri::Result<PathBuf> {
        let folder = app.path().app_data_dir()?;
        if let Some(old) = folder.parent().map(|parent| parent.join(OLD_IDENTIFIER)) {
            if !folder.exists() && old.is_dir() {
                match std::fs::rename(&old, &folder) {
                    Ok(()) => log::info!("moved the data folder from {}", old.display()),
                    Err(e) => log::warn!("couldn't move the old data folder: {e}"),
                }
            }
        }
        Ok(folder)
    }
}
