//! Where Charon keeps its jobs, settings and logs.

use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// The app's identifier before 0.1.1, when Charon was published by DigiFix.
#[cfg_attr(debug_assertions, allow(dead_code))]
const OLD_IDENTIFIER: &str = "ninja.digifix.charon";

/// A development build (`cargo tauri dev`) keeps its own data next to the installed app's,
/// so trying something out never touches the real jobs and their two-way memory.
#[cfg(debug_assertions)]
const DEV_FOLDER: &str = "dev.stean.charon-dev";

pub struct DataFolder;

impl DataFolder {
    /// The data folder (macOS: ~/Library/Application Support/dev.stean.charon/).
    /// The first time 0.1.1 or later starts, it moves the folder used by 0.1.0 into
    /// place, so jobs and settings carry over to the new identifier.
    #[cfg(not(debug_assertions))]
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

    /// A development build's own data folder (macOS:
    /// ~/Library/Application Support/dev.stean.charon-dev/). Nothing is moved into it.
    #[cfg(debug_assertions)]
    pub fn open(app: &AppHandle) -> tauri::Result<PathBuf> {
        let installed = app.path().app_data_dir()?;
        Ok(installed.parent().map(|parent| parent.join(DEV_FOLDER)).unwrap_or(installed))
    }

    /// The name of the log file (in the system's log folder for Charon).
    pub fn log_name() -> &'static str {
        if cfg!(debug_assertions) {
            "charon-dev"
        } else {
            "charon"
        }
    }
}
