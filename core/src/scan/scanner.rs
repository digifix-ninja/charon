//! Walks a folder and records every item that isn't excluded.

use crate::error::{Error, Result};
use crate::scan::{Entry, ExcludeFilter, Snapshot};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// Reads folders into snapshots. Symbolic links are skipped, never followed.
pub struct Scanner<'a> {
    filter: &'a ExcludeFilter,
    cancel: Option<&'a AtomicBool>,
}

impl<'a> Scanner<'a> {
    pub fn new(filter: &'a ExcludeFilter) -> Self {
        Self { filter, cancel: None }
    }

    /// Stops the scan early when `cancel` becomes true.
    pub fn cancellable(mut self, cancel: &'a AtomicBool) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Every item under `root`. Fails if `root` itself isn't reachable; unreadable items
    /// inside it are skipped (and logged).
    pub fn scan(&self, root: &Path) -> Result<Snapshot> {
        if !root.is_dir() {
            return Err(Error::Offline(root.to_path_buf()));
        }
        let mut snapshot = Snapshot::new();
        let walker = WalkDir::new(root)
            .follow_links(false)
            .min_depth(1)
            .into_iter()
            .filter_entry(|item| match Self::relative(root, item.path()) {
                Some(path) => !self.filter.excludes(&path),
                None => false,
            });

        for item in walker {
            if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(Error::Cancelled);
            }
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    log::warn!("skipped while scanning {}: {e}", root.display());
                    continue;
                }
            };
            let Some(path) = Self::relative(root, item.path()) else { continue };
            let file_type = item.file_type();
            if file_type.is_dir() {
                snapshot.insert(path, Entry::dir());
            } else if file_type.is_file() {
                match item.metadata() {
                    Ok(meta) => snapshot.insert(path, Entry::file(meta.len(), Self::modified_ms(&meta))),
                    Err(e) => log::warn!("skipped {path}: {e}"),
                }
            }
        }
        Ok(snapshot)
    }

    /// `path` relative to `root`, with `/` separators on every system.
    fn relative(root: &Path, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(root).ok()?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    fn modified_ms(meta: &std::fs::Metadata) -> i64 {
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}
