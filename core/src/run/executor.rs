//! Runs a plan step by step. One failed file doesn't stop the run: it's counted and
//! reported, and the rest carries on.

use crate::error::{Error, Result};
use crate::plan::{Action, Plan, Side};
use crate::run::{Copier, ProgressMeter, Trash};
use crate::scan::ExcludeFilter;
use chrono::Local;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The two folders of a job.
#[derive(Debug, Clone)]
pub struct Shores {
    pub source: PathBuf,
    pub destination: PathBuf,
}

impl Shores {
    pub fn root(&self, side: Side) -> &Path {
        match side {
            Side::Source => &self.source,
            Side::Destination => &self.destination,
        }
    }

    pub fn path(&self, side: Side, relative: &str) -> PathBuf {
        self.root(side).join(relative)
    }
}

/// What a run achieved.
#[derive(Debug, Default)]
pub struct Outcome {
    pub copied: u64,
    pub deleted: u64,
    pub bytes: u64,
    /// `(path, reason)` for every step that failed.
    pub failed: Vec<(String, String)>,
    pub cancelled: bool,
}

pub struct Executor<'a> {
    shores: &'a Shores,
    /// `None` deletes for good.
    trash: Option<Trash>,
    filter: &'a ExcludeFilter,
    cancel: &'a AtomicBool,
}

impl<'a> Executor<'a> {
    pub fn new(shores: &'a Shores, safe_delete: bool, filter: &'a ExcludeFilter, cancel: &'a AtomicBool) -> Self {
        Self {
            shores,
            trash: safe_delete.then(|| Trash::for_run(Local::now())),
            filter,
            cancel,
        }
    }

    /// Runs every step of `plan` in order.
    pub fn run(&self, plan: &Plan, meter: &mut ProgressMeter) -> Outcome {
        let mut outcome = Outcome::default();
        for action in plan.ordered() {
            if self.cancel.load(Ordering::Relaxed) {
                outcome.cancelled = true;
                break;
            }
            if matches!(action, Action::Conflict { .. }) {
                continue;
            }
            meter.start(action.path());
            match self.apply(action, meter) {
                Ok(()) => match action {
                    Action::Copy { bytes, .. } => {
                        outcome.copied += 1;
                        outcome.bytes += bytes;
                    }
                    Action::KeepBoth { .. } => {
                        outcome.copied += 2;
                        outcome.bytes += action.bytes();
                    }
                    Action::Delete { .. } => outcome.deleted += 1,
                    _ => {}
                },
                Err(Error::Cancelled) => {
                    outcome.cancelled = true;
                    break;
                }
                Err(e) => {
                    log::warn!("{}: {e}", action.path());
                    outcome.failed.push((action.path().to_string(), e.to_string()));
                }
            }
            meter.finish_item();
        }
        meter.finishing();
        outcome
    }

    /// Carries out one step.
    pub fn apply(&self, action: &Action, meter: &mut ProgressMeter) -> Result<()> {
        match action {
            Action::CreateDir { side, path } => {
                let target = self.shores.path(*side, path);
                fs::create_dir_all(&target).map_err(|e| Error::file(target, e))
            }
            Action::Copy { to, path, .. } => self.copy(to.opposite(), *to, path, path, meter),
            Action::Delete { side, path, dir, whole } => self.delete(*side, path, *dir, *whole),
            Action::KeepBoth { path, .. } => self.keep_both(path, meter),
            Action::Conflict { .. } => Ok(()),
        }
    }

    /// Copies `from_side/from_path` over `to_side/to_path`; the replaced file goes to the trash.
    fn copy(&self, from_side: Side, to_side: Side, from_path: &str, to_path: &str, meter: &mut ProgressMeter) -> Result<()> {
        let from = self.shores.path(from_side, from_path);
        let to = self.shores.path(to_side, to_path);
        let partial = Copier::new(self.cancel).copy(&from, &to, &mut |n| meter.add_bytes(n))?;
        if to.exists() {
            if let Err(e) = self.remove(to_side, to_path, false) {
                let _ = fs::remove_file(&partial);
                return Err(e);
            }
        }
        Copier::commit(&partial, &to)
    }

    /// Removes an item. A folder that isn't `whole` is only removed when nothing but
    /// excluded clutter (like `.DS_Store`) is left in it, so new files are never lost.
    fn delete(&self, side: Side, path: &str, dir: bool, whole: bool) -> Result<()> {
        let target = self.shores.path(side, path);
        if !target.exists() {
            return Ok(());
        }
        if dir && !whole && !self.only_clutter_inside(&target, path) {
            log::info!("kept {} on the {side:?} side: it still holds files", path);
            return Ok(());
        }
        self.remove(side, path, dir)
    }

    /// Moves to the trash (safe deletes) or deletes for good.
    fn remove(&self, side: Side, path: &str, dir: bool) -> Result<()> {
        let root = self.shores.root(side);
        let target = root.join(path);
        let result = match &self.trash {
            Some(trash) => trash.put(root, path).map(|_| ()),
            None if dir => fs::remove_dir_all(&target),
            None => fs::remove_file(&target),
        };
        result.map_err(|e| Error::file(target, e))
    }

    /// Both versions survive: the destination's version is renamed to a conflict name and
    /// copied to the source; the source's version takes the original name on both sides.
    fn keep_both(&self, path: &str, meter: &mut ProgressMeter) -> Result<()> {
        let conflict_path = Self::conflict_name(path);
        let destination_file = self.shores.path(Side::Destination, path);
        let renamed = self.shores.path(Side::Destination, &conflict_path);
        fs::rename(&destination_file, &renamed).map_err(|e| Error::file(&destination_file, e))?;
        self.copy(Side::Source, Side::Destination, path, path, meter)?;
        self.copy(Side::Destination, Side::Source, &conflict_path, &conflict_path, meter)
    }

    /// `notes/plan.txt` → `notes/plan (conflict 2026-10-16 16.20).txt`.
    pub fn conflict_name(path: &str) -> String {
        let stamp = Local::now().format("%Y-%m-%d %H.%M");
        let (folder, name) = match path.rfind('/') {
            Some(i) => (&path[..=i], &path[i + 1..]),
            None => ("", path),
        };
        let (stem, extension) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        format!("{folder}{stem} (conflict {stamp}){extension}")
    }

    /// Whether everything inside `folder` is excluded clutter.
    fn only_clutter_inside(&self, folder: &Path, relative: &str) -> bool {
        walkdir::WalkDir::new(folder)
            .min_depth(1)
            .into_iter()
            .filter_map(|item| item.ok())
            .all(|item| {
                let inner = item.path().strip_prefix(folder).unwrap_or(item.path());
                let rel = format!("{relative}/{}", inner.to_string_lossy().replace('\\', "/"));
                item.file_type().is_dir() || self.filter.excludes(&rel)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_names_keep_the_extension() {
        let name = Executor::conflict_name("notes/plan.txt");
        assert!(name.starts_with("notes/plan (conflict "));
        assert!(name.ends_with(").txt"));
        assert!(Executor::conflict_name(".env").starts_with(".env (conflict "));
    }
}
