//! Hears about changes inside the watched folders (FSEvents on macOS, ReadDirectoryChanges
//! on Windows, inotify on Linux) and reports which job they belong to, and where.

use crate::model::JobId;
use crate::scan::{is_partial, MARKER_FOLDER, TRASH_FOLDER};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A folder that couldn't be watched is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(60);

type Routes = Arc<Mutex<Vec<(PathBuf, JobId)>>>;

pub struct FolderWatcher {
    watcher: RecommendedWatcher,
    routes: Routes,
    watched: HashSet<PathBuf>,
    failed: HashMap<PathBuf, Instant>,
}

impl FolderWatcher {
    /// A watcher that sends the id of the job whose folder changed, and the changed path,
    /// to `changes`.
    pub fn new(changes: Sender<(JobId, PathBuf)>) -> notify::Result<Self> {
        let routes: Routes = Arc::default();
        let lookup = routes.clone();
        let watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let Ok(event) = result else { return };
            if event.kind.is_access() {
                return;
            }
            let routes = lookup.lock().expect("routes lock");
            let mut told: HashSet<&JobId> = HashSet::new();
            for path in event.paths.iter().filter(|p| !Self::is_own_file(p)) {
                for (root, id) in routes.iter() {
                    if path.starts_with(root) && told.insert(id) {
                        let _ = changes.send((id.clone(), path.clone()));
                    }
                }
            }
        })?;
        Ok(Self { watcher, routes, watched: HashSet::new(), failed: HashMap::new() })
    }

    /// Watches exactly the folders in `routes` (folder → job), starting and stopping
    /// watches as needed.
    pub fn follow(&mut self, routes: Vec<(PathBuf, JobId)>) {
        let wanted: HashSet<PathBuf> = routes.iter().map(|(root, _)| root.clone()).collect();

        for root in self.watched.difference(&wanted).cloned().collect::<Vec<_>>() {
            let _ = self.watcher.unwatch(&root);
            self.watched.remove(&root);
        }
        for root in wanted.difference(&self.watched).cloned().collect::<Vec<_>>() {
            if self.failed.get(&root).is_some_and(|t| t.elapsed() < RETRY_AFTER) {
                continue;
            }
            match self.watcher.watch(&root, RecursiveMode::Recursive) {
                Ok(()) => {
                    self.failed.remove(&root);
                    self.watched.insert(root);
                }
                Err(e) => {
                    log::warn!("couldn't watch {}: {e}", root.display());
                    self.failed.insert(root, Instant::now());
                }
            }
        }
        *self.routes.lock().expect("routes lock") = routes;
    }

    /// Charon's trash, markers and half-copied files: never a reason to look again.
    fn is_own_file(path: &Path) -> bool {
        path.components().any(|c| c.as_os_str() == TRASH_FOLDER || c.as_os_str() == MARKER_FOLDER)
            || path.file_name().is_some_and(|n| is_partial(&n.to_string_lossy()))
    }
}
