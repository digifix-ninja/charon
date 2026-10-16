//! Safe deletes: removed and overwritten items are moved into `.charon-trash/<run time>/`
//! at the root of their folder, and cleared out after a number of days.

use crate::scan::TRASH_FOLDER;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const STAMP_FORMAT: &str = "%Y-%m-%d_%H-%M-%S";

/// The trash of one run. Every item removed during the run goes into the same dated folder.
pub struct Trash {
    stamp: String,
}

impl Trash {
    /// A trash for a run starting now.
    pub fn for_run(started: DateTime<Local>) -> Self {
        Self { stamp: started.format(STAMP_FORMAT).to_string() }
    }

    /// Moves `root/relative` into `root/.charon-trash/<stamp>/relative`. Moving stays on the
    /// same drive, so it's instant even for big folders.
    pub fn put(&self, root: &Path, relative: &str) -> io::Result<PathBuf> {
        let from = root.join(relative);
        let mut to = root.join(TRASH_FOLDER).join(&self.stamp).join(relative);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        // The same path removed twice in one run (replaced, then replaced again): number it.
        let mut n = 2;
        while to.exists() {
            to = root.join(TRASH_FOLDER).join(&self.stamp).join(format!("{relative} ({n})"));
            n += 1;
        }
        fs::rename(&from, &to)?;
        Ok(to)
    }

    /// Permanently removes trash folders older than `days` (0 keeps everything).
    pub fn sweep(root: &Path, days: u32) {
        if days == 0 {
            return;
        }
        let Ok(items) = fs::read_dir(root.join(TRASH_FOLDER)) else { return };
        let cutoff = Local::now() - chrono::Duration::days(days as i64);
        for item in items.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let Some(when) = Self::parse_stamp(&name) else { continue };
            if when < cutoff {
                if let Err(e) = fs::remove_dir_all(item.path()) {
                    log::warn!("couldn't clear old trash {}: {e}", item.path().display());
                }
            }
        }
        // Leave no empty trash folder behind.
        let _ = fs::remove_dir(root.join(TRASH_FOLDER));
    }

    fn parse_stamp(name: &str) -> Option<DateTime<Local>> {
        let naive = NaiveDateTime::parse_from_str(name, STAMP_FORMAT).ok()?;
        Local.from_local_datetime(&naive).earliest()
    }
}
