//! Runs a plan step by step.
//!
//! Every step was planned against a scan (or a review the person confirmed). Right before
//! a step acts, the executor looks at the disk again, on *both* sides: the item it is about
//! to copy, replace or remove must be exactly what the plan saw, and so must the other
//! side's item the step relies on (a file restored on the other side stops its removal
//! here). If anything changed, the step is skipped, never forced. Before every step both
//! folders must still be the folders the plan was made for: a drive that went away stops
//! the whole run. Parent folders are checked one by one, so nothing is ever written or
//! removed through a link. One failed or skipped item doesn't stop the run: it's counted
//! and reported.
//!
//! Every finished step is recorded (`Settled`), so the two-way memory can be updated with
//! what really happened.

use crate::error::{Error, Result};
use crate::plan::{Action, Pair, Plan, Settled, Side};
use crate::run::durable;
use crate::run::names;
use crate::run::rename::rename_new;
use crate::run::{Copier, ProgressMeter, Trash};
use crate::scan::{is_clutter, Entry, EntryKind, Keys, RootId, Scanner, Snapshot};
use chrono::Local;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_normalization::UnicodeNormalization;

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
}

/// How a run treats what it removes and replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Care {
    /// Removed items go into the trash instead of being deleted for good.
    pub safe_delete: bool,
    /// Replaced files are kept in the trash even without safe deletes. Two-way jobs always
    /// do this: the replaced version may be someone's last copy of their own work.
    pub keep_replaced: bool,
}

/// What a run achieved.
#[derive(Debug, Default)]
pub struct Outcome {
    pub copied: u64,
    pub deleted: u64,
    pub bytes: u64,
    /// `(path, reason)` for every step that failed.
    pub failed: Vec<(String, String)>,
    /// `(path, reason)` for every step left out because the item changed after the scan
    /// (or can't be carried after all).
    pub skipped: Vec<(String, String)>,
    /// Folders that were to be removed but still hold something.
    pub kept: Vec<String>,
    pub cancelled: bool,
    /// Why the whole run stopped before the end (a folder went away), if it did.
    pub stopped: Option<String>,
    /// What each finished step left behind, in the order they finished.
    pub settled: Vec<Settled>,
    /// Files and links removed or replaced on the source and on the destination (keys).
    pub destroyed: (Vec<String>, Vec<String>),
    /// Sides whose drive turned out not to hold symbolic links.
    pub no_links: Vec<Side>,
}

/// What one step did.
enum Done {
    Copied(u64, u64),
    Deleted,
    Created,
    /// Nothing was needed (the item was already gone).
    Nothing,
    Skipped(String),
    Kept,
}

/// What the folders above an item are.
enum Parents {
    Real,
    /// One of them is gone (and so is the item).
    Missing,
    /// One of them is now a file or a link.
    NotFolders,
}

/// How each side spells paths on disk, from the scans the plan was made from.
pub struct Spellings<'a> {
    source: &'a Snapshot,
    destination: &'a Snapshot,
    /// Names this run made up (conflict copies), per side.
    learned: RefCell<HashMap<(Side, String), String>>,
}

impl<'a> Spellings<'a> {
    pub fn new(source: &'a Snapshot, destination: &'a Snapshot) -> Self {
        Self { source, destination, learned: RefCell::new(HashMap::new()) }
    }

    fn snapshot(&self, side: Side) -> &'a Snapshot {
        match side {
            Side::Source => self.source,
            Side::Destination => self.destination,
        }
    }

    /// The on-disk relative path of `key` on `side`. An item that's new there gets its
    /// folder as spelled on that side and its own name as spelled where it comes from (in
    /// the NFC form most systems write; Macs read either form).
    pub fn disk(&self, side: Side, key: &str) -> String {
        if let Some(disk) = self.learned.borrow().get(&(side, key.to_string())) {
            return disk.clone();
        }
        let here = self.snapshot(side);
        if here.contains(key) || here.has_spelling(key) {
            return here.spelling(key).to_string();
        }
        let name: String = Keys::name(self.snapshot(side.opposite()).spelling(key)).nfc().collect();
        match Keys::parent(key) {
            Some(parent) => format!("{}/{name}", self.disk(side, parent)),
            None => name,
        }
    }

    /// The key of an on-disk path, as the plan's keys are made.
    fn key_for(&self, disk: &str) -> String {
        self.destination.key_for(disk)
    }

    /// Remembers the spelling of an item this run created under a new name.
    fn learn(&self, side: Side, key: &str, disk: String) {
        self.learned.borrow_mut().insert((side, key.to_string()), disk);
    }
}

pub struct Executor<'a> {
    shores: &'a Shores,
    spellings: Spellings<'a>,
    /// Removed and replaced items are kept here (with safe deletes, and always for the
    /// losing side of a conflict).
    trash: Trash,
    care: Care,
    cancel: &'a AtomicBool,
    /// The folders the plan was made for, as found by its scans.
    roots: (Option<RootId>, Option<RootId>),
    /// Folders already checked to be real folders during this run.
    real_folders: RefCell<HashSet<PathBuf>>,
    /// Folders whose list of names this run changed (flushed to the drive at the end).
    touched: RefCell<BTreeSet<PathBuf>>,
    settled: RefCell<Vec<Settled>>,
    destroyed: RefCell<(Vec<String>, Vec<String>)>,
}

impl<'a> Executor<'a> {
    /// An executor for a plan made from `source` and `destination` (whose spellings and
    /// drives it uses, and whose folders it insists on).
    pub fn new(shores: &'a Shores, source: &'a Snapshot, destination: &'a Snapshot, care: Care, cancel: &'a AtomicBool) -> Self {
        Self {
            shores,
            spellings: Spellings::new(source, destination),
            trash: Trash::for_run(Local::now()),
            care,
            cancel,
            roots: (source.root(), destination.root()),
            real_folders: RefCell::new(HashSet::new()),
            touched: RefCell::new(BTreeSet::new()),
            settled: RefCell::new(Vec::new()),
            destroyed: RefCell::default(),
        }
    }

    /// The side whose spelling of a step's path the person sees in the progress line: the
    /// side the item comes from, or the side it's removed from.
    fn shown_side(action: &Action) -> Side {
        match action {
            Action::Copy { to, .. } | Action::CreateDir { side: to, .. } => to.opposite(),
            Action::Delete { side, .. } => *side,
            _ => Side::Source,
        }
    }

    /// Runs every step of `plan` in order.
    pub fn run(&self, plan: &Plan, meter: &mut ProgressMeter) -> Outcome {
        let mut outcome = Outcome::default();
        // A side where copies failed keeps everything this run would remove there: the
        // removal could take the last copy of something that didn't arrive.
        let mut copies_failed: HashSet<Side> = HashSet::new();
        for (action, makes_room) in plan.ordered_steps() {
            if self.cancel.load(Ordering::Relaxed) {
                outcome.cancelled = true;
                break;
            }
            if matches!(action, Action::Conflict { .. }) {
                continue;
            }
            if let Err(reason) = self.check_roots() {
                outcome.stopped = Some(reason);
                break;
            }
            meter.start(&self.spellings.disk(Self::shown_side(action), action.path()));
            let result = match action {
                Action::Delete { side, .. } if !makes_room && copies_failed.contains(side) => Ok(Done::Skipped(format!(
                    "kept for now: some copies to the {} failed in this run",
                    side.word()
                ))),
                Action::Copy { to, item: EntryKind::Link, .. } if outcome.no_links.contains(to) => {
                    Ok(Done::Skipped(format!("left out: the {}'s drive can't hold symbolic links", to.word())))
                }
                _ => self.apply(action, meter),
            };
            match result {
                Ok(Done::Copied(files, bytes)) => {
                    outcome.copied += files;
                    outcome.bytes += bytes;
                }
                Ok(Done::Deleted) => outcome.deleted += 1,
                Ok(Done::Created | Done::Nothing) => {}
                Ok(Done::Skipped(reason)) => {
                    log::info!("skipped {}: {reason}", action.path());
                    outcome.skipped.push((action.path().to_string(), reason));
                }
                Ok(Done::Kept) => outcome.kept.push(action.path().to_string()),
                Err(Error::Cancelled) => {
                    outcome.cancelled = true;
                    break;
                }
                Err(Error::LandedElsewhere(reason)) => {
                    outcome.stopped = Some(reason);
                    break;
                }
                Err(Error::Changed(reason)) => outcome.skipped.push((action.path().to_string(), reason)),
                Err(Error::NoLinks) => {
                    if let Action::Copy { to, .. } = action {
                        if !outcome.no_links.contains(to) {
                            outcome.no_links.push(*to);
                        }
                        outcome.skipped.push((action.path().to_string(), format!("left out: the {}'s drive can't hold symbolic links", to.word())));
                    }
                }
                Err(e) => {
                    log::warn!("{}: {e}", action.path());
                    match action {
                        Action::Copy { to, .. } => {
                            copies_failed.insert(*to);
                        }
                        Action::KeepBoth { .. } => {
                            copies_failed.extend([Side::Source, Side::Destination]);
                        }
                        _ => {}
                    }
                    outcome.failed.push((action.path().to_string(), e.to_string()));
                }
            }
            meter.finish_item();
        }
        meter.finishing();
        outcome.settled = self.settled.take();
        outcome.destroyed = self.destroyed.take();
        outcome
    }

    /// Moves leftover half-copied files from interrupted runs into the trash, each only if
    /// it is still exactly what the scan saw. Returns how many were moved.
    pub fn tidy(&self, side: Side, leftovers: &[(String, Entry)]) -> u64 {
        let root = self.shores.root(side);
        let mut moved = 0;
        for (relative, seen) in leftovers {
            let path = root.join(relative);
            if !matches!(self.parents(root, relative), Ok(Parents::Real)) || !matches!(Self::look(&path), Ok(Some(now)) if now.unchanged(seen)) {
                continue;
            }
            match self.trash.put(root, relative) {
                Ok(kept) => {
                    self.touch(&path);
                    self.touch(&kept);
                    moved += 1;
                }
                Err(e) => log::warn!("couldn't move the leftover {} into the trash: {e}", path.display()),
            }
        }
        moved
    }

    /// Makes everything this run changed durable: every folder it touched, then each drive.
    /// Until this succeeds, nothing about the run may be recorded as done.
    pub fn flush(&self) -> Result<()> {
        for folder in self.touched.borrow().iter() {
            match durable::sync_folder(folder) {
                Ok(()) => {}
                // A folder this run removed afterwards.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::file(folder, e)),
            }
        }
        if let Err(reason) = self.check_roots() {
            return Err(Error::Invalid(reason));
        }
        for side in [Side::Source, Side::Destination] {
            let root = self.shores.root(side);
            durable::sync_drive(root).map_err(|e| Error::file(root, e))?;
        }
        Ok(())
    }

    /// Both folders must still be the ones the plan was made for.
    fn check_roots(&self) -> std::result::Result<(), String> {
        for (side, expected) in [(Side::Source, self.roots.0), (Side::Destination, self.roots.1)] {
            let root = self.shores.root(side);
            let same = match (RootId::of(root), expected) {
                (Ok(Some(now)), Some(expected)) => now.same_folder(&expected),
                (Ok(_), _) => true,
                (Err(_), _) => false,
            };
            if !same {
                return Err(format!(
                    "The {} folder went away or was replaced during the sync (was a drive disconnected?). Charon stopped there.",
                    side.word()
                ));
            }
        }
        Ok(())
    }

    /// Carries out one step.
    fn apply(&self, action: &Action, meter: &mut ProgressMeter) -> Result<Done> {
        match action {
            Action::CreateDir { side, path } => self.create_dir(*side, path),
            Action::Copy { to, path, from, existing, settles_conflict, .. } => {
                match self.copy(*to, path, from, existing.as_ref(), *settles_conflict, meter)? {
                    Some((bytes, landed)) => {
                        self.settle(path, Some(Self::pair(*to, landed, *from)));
                        Ok(Done::Copied(1, bytes))
                    }
                    None => Ok(Done::Skipped(format!("changed on the {} side since the scan", to.word()))),
                }
            }
            Action::Delete { side, path, entry, other, .. } => self.delete(*side, path, entry, other.as_ref()),
            Action::KeepBoth { path, source, destination, .. } => self.keep_both(path, source, destination, meter),
            Action::Conflict { .. } => Ok(Done::Nothing),
        }
    }

    // ---------------------------------------------------------------- the steps

    /// A folder, one level (its parents are checked or created first), as long as the
    /// other side still has it.
    fn create_dir(&self, side: Side, key: &str) -> Result<Done> {
        let there = self.path(side.opposite(), key);
        if !Self::look(&there)?.is_some_and(|e| e.is_dir()) {
            return Ok(Done::Skipped(format!("no longer a folder on the {} side", side.opposite().word())));
        }
        let relative = self.spellings.disk(side, key);
        let root = self.shores.root(side);
        self.prepare_parents(root, &relative)?;
        self.ensure_folder(&root.join(&relative))?;
        self.on_its_drive(side, &root.join(&relative))?;
        self.settle(key, Some(Pair::new(Entry::dir(), Entry::dir())));
        Ok(Done::Created)
    }

    /// Copies `from` (as planned) onto side `to`, over `existing` (as planned). Returns the
    /// bytes carried and the item as it landed, or `None` when the target changed since the
    /// scan. The replaced version goes to the trash with safe deletes, in two-way jobs, and
    /// always when the copy settles a conflict.
    fn copy(&self, to: Side, key: &str, from: &Entry, existing: Option<&Entry>, settles: bool, meter: &mut ProgressMeter) -> Result<Option<(u64, Entry)>> {
        let from_path = self.path(to.opposite(), key);
        let relative = self.spellings.disk(to, key);
        let root = self.shores.root(to);
        let target = root.join(&relative);

        if !Entry::still(self.look_fresh(to.opposite(), &from_path)?.as_ref(), Some(from)) {
            return Err(Error::Changed(format!("changed on the {} side since the scan", to.opposite().word())));
        }
        if !Entry::still(Self::look(&target)?.as_ref(), existing) {
            return Ok(None);
        }
        self.prepare_parents(root, &relative)?;

        let partial = match from.kind {
            EntryKind::File => {
                let coarse = self.spellings.snapshot(to.opposite()).volume().is_some_and(|v| v.coarse_times());
                let mut copier = Copier::new(self.cancel).coarse_source_times(coarse);
                if !self.holds_attributes(to) {
                    copier = copier.without_attributes();
                }
                copier.file(&from_path, &target, from, &mut |n| meter.add_bytes(n))?
            }
            EntryKind::Link => Copier::link(&from_path, &target, from)?,
            EntryKind::Dir => return Err(Error::Invalid("a folder can't be copied as a file".into())),
        };
        self.touch(&target);
        // One last look, fresh from the drive: the target must still be what the plan saw.
        if !Entry::still(self.look_fresh(to, &target)?.as_ref(), existing) {
            Copier::discard(&partial);
            return Ok(None);
        }
        let keep_old = self.care.safe_delete || self.care.keep_replaced || settles;
        match existing {
            None => Copier::commit_new(&partial, &target)?,
            Some(old) if old.is_dir() => {
                Copier::discard(&partial);
                return Err(Error::Invalid(format!("a folder named {} is in the way", target.display())));
            }
            Some(old) if keep_old && old.is_file() => {
                // Keep a copy of the old version, then replace it in place (see `Trash`).
                let kept = match self.trash.preserve(root, &relative) {
                    Ok(kept) => kept,
                    Err(e) => {
                        Copier::discard(&partial);
                        return Err(Error::file(&target, e));
                    }
                };
                self.touch(&kept);
                if !Entry::still(self.look_fresh(to, &target)?.as_ref(), existing) {
                    let _ = fs::remove_file(&kept);
                    Copier::discard(&partial);
                    return Ok(None);
                }
                Self::make_writable(&target);
                Copier::commit_over(&partial, &target)?;
            }
            Some(_) if keep_old => {
                // A link: moved to the trash as it is.
                match self.trash.put(root, &relative) {
                    Ok(kept) => self.touch(&kept),
                    Err(e) => {
                        Copier::discard(&partial);
                        return Err(Error::file(&target, e));
                    }
                }
                Copier::commit_new(&partial, &target)?;
            }
            Some(_) => {
                Self::make_writable(&target);
                Copier::commit_over(&partial, &target)?;
            }
        }
        if existing.is_some() {
            self.destroyed_on(to, key);
        }
        let landed = Self::look(&target)?.ok_or_else(|| Error::Invalid(format!("{} vanished right after it was copied", target.display())))?;
        self.on_its_drive(to, &target)?;
        Ok(Some((if from.is_file() { from.size } else { 0 }, landed)))
    }

    /// What a step just wrote must be on the drive the folder was on at the scan. When a
    /// drive goes away between two checks, the next item lands in the folder it leaves
    /// behind, on another disk: the run stops there, and nothing of it is recorded.
    #[cfg(unix)]
    fn on_its_drive(&self, side: Side, path: &Path) -> Result<()> {
        let expected = match side {
            Side::Source => self.roots.0,
            Side::Destination => self.roots.1,
        };
        let Some(expected) = expected else { return Ok(()) };
        let meta = fs::symlink_metadata(path).map_err(|e| Error::file(path, e))?;
        if RootId::device_of(&meta) != expected.device() {
            return Err(Error::LandedElsewhere(format!(
                "The {} folder's drive went away during the sync (was it disconnected?). Charon stopped there.",
                side.word()
            )));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn on_its_drive(&self, _side: Side, _path: &Path) -> Result<()> {
        Ok(())
    }

    /// Removes an item that must still be what the plan saw, while the other side still has
    /// what the plan saw there (`other`, usually nothing): a file restored on the other side
    /// meanwhile stops the removal. A folder only goes when nothing but system clutter is
    /// left in it.
    fn delete(&self, side: Side, key: &str, planned: &Entry, other: Option<&Entry>) -> Result<Done> {
        let there = self.path(side.opposite(), key);
        if !Entry::still(self.look_fresh(side.opposite(), &there)?.as_ref(), other) {
            return Ok(Done::Skipped(format!("changed on the {} side since the scan", side.opposite().word())));
        }
        // With nothing on the other side, a finished removal means both sides agree: gone.
        // (Removals that make room for another item leave the record to the step after them.)
        let settles = other.is_none();
        let relative = self.spellings.disk(side, key);
        let root = self.shores.root(side);
        match self.parents(root, &relative)? {
            Parents::Real => {}
            Parents::Missing => {
                if settles {
                    self.settle(key, None);
                }
                return Ok(Done::Nothing);
            }
            Parents::NotFolders => return Ok(Done::Skipped("a folder above it was replaced since the scan".into())),
        }
        let target = root.join(&relative);
        match self.look_fresh(side, &target)? {
            None => {
                if settles {
                    self.settle(key, None);
                }
                return Ok(Done::Nothing);
            }
            Some(now) if !now.unchanged(planned) => {
                return Ok(Done::Skipped(format!("changed on the {} side since the scan", side.word())))
            }
            Some(_) => {}
        }
        let done = if planned.is_dir() {
            self.remove_folder(root, &relative, &target)?
        } else {
            self.remove(root, &relative, &target, self.care.safe_delete)?;
            self.destroyed_on(side, key);
            Done::Deleted
        };
        if matches!(done, Done::Deleted) {
            self.touch(&target);
            if settles {
                self.settle(key, None);
            }
        }
        Ok(done)
    }

    /// Both versions survive: the destination's version moves aside under a conflict name
    /// (and is copied to the source under that name); the source's version takes the
    /// original name on the destination.
    fn keep_both(&self, key: &str, source: &Entry, destination: &Entry, meter: &mut ProgressMeter) -> Result<Done> {
        let source_path = self.path(Side::Source, key);
        let destination_path = self.path(Side::Destination, key);
        if !Entry::still(self.look_fresh(Side::Source, &source_path)?.as_ref(), Some(source))
            || !Entry::still(self.look_fresh(Side::Destination, &destination_path)?.as_ref(), Some(destination))
        {
            return Ok(Done::Skipped("changed since the scan".into()));
        }
        let conflict_key = self.free_conflict_name(key)?;
        let aside = self.path(Side::Destination, &conflict_key);
        rename_new(&destination_path, &aside).map_err(|e| Error::file(&destination_path, e))?;
        self.touch(&aside);
        self.touch(&destination_path);
        let moved = Self::look(&aside)?.ok_or_else(|| Error::Invalid(format!("{} vanished", aside.display())))?;

        let carried = self.copy(Side::Destination, key, source, None, false, meter)?;
        if let Some((_, landed)) = carried {
            self.settle(key, Some(Pair::new(*source, landed)));
        }
        let back = self.copy(Side::Source, &conflict_key, &moved, None, false, meter)?;
        if let Some((_, landed)) = back {
            self.settle(&conflict_key, Some(Pair::new(landed, moved)));
        }
        match (carried, back) {
            (Some((a, _)), Some((b, _))) => Ok(Done::Copied(2, a + b)),
            _ => Ok(Done::Skipped("changed while both versions were being kept".into())),
        }
    }

    // ---------------------------------------------------------------- removing

    /// Into the trash (`keep`), or deleted for good. Links are removed as links.
    fn remove(&self, root: &Path, relative: &str, target: &Path, keep: bool) -> Result<()> {
        if keep {
            let kept = self.trash.put(root, relative).map_err(|e| Error::file(target, e))?;
            self.touch(&kept);
            Ok(())
        } else {
            Self::make_writable(target);
            fs::remove_file(target).map_err(|e| Error::file(target, e))
        }
    }

    /// Windows refuses to replace or delete a read-only file; the file was checked to be
    /// exactly what the plan saw, so its read-only mark is lifted first.
    fn make_writable(path: &Path) {
        #[cfg(windows)]
        if let Ok(meta) = fs::symlink_metadata(path) {
            let mut permissions = meta.permissions();
            if meta.is_file() && permissions.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                if let Err(e) = fs::set_permissions(path, permissions) {
                    log::warn!("couldn't lift the read-only mark of {}: {e}", path.display());
                }
            }
        }
        #[cfg(not(windows))]
        let _ = path;
    }

    /// Removes a folder whose contents were removed by earlier steps. If anything besides
    /// system clutter is still inside (a file added meanwhile, an excluded item), it stays.
    fn remove_folder(&self, root: &Path, relative: &str, target: &Path) -> Result<Done> {
        let mut clutter = Vec::new();
        for item in fs::read_dir(target).map_err(|e| Error::file(target, e))? {
            let item = item.map_err(|e| Error::file(target, e))?;
            let is_file = item.file_type().is_ok_and(|t| t.is_file());
            match item.file_name().to_str() {
                Some(name) if is_file && is_clutter(name) => clutter.push(format!("{relative}/{name}")),
                _ => {
                    log::info!("kept {}: it still holds {}", target.display(), item.file_name().to_string_lossy());
                    return Ok(Done::Kept);
                }
            }
        }
        // System files nobody listed (.DS_Store, desktop.ini…) always go to .charon-trash:
        // the person never saw them in the list, so they're never gone for good.
        for item in &clutter {
            self.remove(root, item, &root.join(item), true)?;
        }
        self.real_folders.borrow_mut().clear();
        match fs::remove_dir(target) {
            Ok(()) => Ok(Done::Deleted),
            // Something appeared inside at the last moment: the folder stays.
            Err(e) if fs::read_dir(target).is_ok_and(|mut items| items.next().is_some()) => {
                log::info!("kept {}: {e}", target.display());
                Ok(Done::Kept)
            }
            Err(e) => Err(Error::file(target, e)),
        }
    }

    // ---------------------------------------------------------------- records

    fn settle(&self, key: &str, pair: Option<Pair>) {
        self.settled.borrow_mut().push(Settled { path: key.to_string(), pair });
    }

    /// The pair for a copy onto `to`: what landed there, and what came from the other side.
    fn pair(to: Side, landed: Entry, from: Entry) -> Pair {
        match to {
            Side::Destination => Pair::new(from, landed),
            Side::Source => Pair::new(landed, from),
        }
    }

    fn destroyed_on(&self, side: Side, key: &str) {
        let mut destroyed = self.destroyed.borrow_mut();
        match side {
            Side::Source => destroyed.0.push(key.to_string()),
            Side::Destination => destroyed.1.push(key.to_string()),
        }
    }

    /// Notes that the folder holding `item` changed.
    fn touch(&self, item: &Path) {
        if let Some(parent) = item.parent() {
            self.touched.borrow_mut().insert(parent.to_path_buf());
        }
    }

    // ---------------------------------------------------------------- looking

    /// The full path of `key` on `side`, as spelled there.
    fn path(&self, side: Side, key: &str) -> PathBuf {
        self.shores.root(side).join(self.spellings.disk(side, key))
    }

    /// `look`, but on a network share a file is opened and asked directly: a share's
    /// listing can show another computer's change late.
    fn look_fresh(&self, side: Side, path: &Path) -> Result<Option<Entry>> {
        let seen = Self::look(path)?;
        let network = self.spellings.snapshot(side).volume().is_some_and(|v| v.network);
        match seen {
            Some(entry) if network && entry.is_file() => match fs::File::open(path).and_then(|file| file.metadata()) {
                Ok(meta) if meta.is_file() => Ok(Some(Entry::file(meta.len(), Scanner::modified_ms(&meta)).changed_at(Scanner::change_time(&meta)))),
                Ok(_) => Err(Error::Changed("changed since the scan".into())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(Error::file(path, e)),
            },
            other => Ok(other),
        }
    }

    /// Whether extended attributes are worth copying onto `side`'s drive.
    fn holds_attributes(&self, side: Side) -> bool {
        self.spellings.snapshot(side).volume().is_none_or(|v| v.holds_extended_attributes())
    }

    /// The item at `path` as it is now, without following links; `None` if nothing is there.
    /// Any other answer (no permission, a drive error) is an error: it never counts as "gone".
    fn look(path: &Path) -> Result<Option<Entry>> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(None),
            Err(e) => return Err(Error::file(path, e)),
        };
        let kind = meta.file_type();
        if kind.is_dir() {
            Ok(Some(Entry::dir()))
        } else if kind.is_file() {
            Ok(Some(Entry::file(meta.len(), Scanner::modified_ms(&meta)).changed_at(Scanner::change_time(&meta))))
        } else if kind.is_symlink() {
            Ok(Some(Self::link_entry(path, &meta)?))
        } else {
            Err(Error::Invalid(format!("{} isn't a file or a folder", path.display())))
        }
    }

    #[cfg(unix)]
    fn link_entry(path: &Path, meta: &fs::Metadata) -> Result<Entry> {
        use std::os::unix::ffi::OsStrExt;
        let target = fs::read_link(path).map_err(|e| Error::file(path, e))?;
        Ok(Entry::link(target.as_os_str().as_bytes(), Scanner::modified_ms(meta)))
    }

    /// Windows links are never in a plan; this only keeps them from matching anything.
    #[cfg(not(unix))]
    fn link_entry(_path: &Path, meta: &fs::Metadata) -> Result<Entry> {
        Ok(Entry { kind: EntryKind::Link, size: u64::MAX, modified: Scanner::modified_ms(meta), target: u64::MAX, changed: 0 })
    }

    // ---------------------------------------------------------------- folders on the way

    /// Every folder above `relative` must be a real folder; missing ones are created one
    /// level at a time, below the root only. A file or link in the way stops the step.
    fn prepare_parents(&self, root: &Path, relative: &str) -> Result<()> {
        let mut current = root.to_path_buf();
        let parts: Vec<&str> = relative.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            current.push(part);
            if self.real_folders.borrow().contains(&current) {
                continue;
            }
            self.ensure_folder(&current)?;
            self.real_folders.borrow_mut().insert(current.clone());
        }
        Ok(())
    }

    /// What the folders above `relative` are (nothing is created).
    fn parents(&self, root: &Path, relative: &str) -> Result<Parents> {
        let mut current = root.to_path_buf();
        let parts: Vec<&str> = relative.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            current.push(part);
            if self.real_folders.borrow().contains(&current) {
                continue;
            }
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.is_dir() => {
                    self.real_folders.borrow_mut().insert(current.clone());
                }
                Ok(_) => return Ok(Parents::NotFolders),
                Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(Parents::Missing),
                Err(e) => return Err(Error::file(&current, e)),
            }
        }
        Ok(Parents::Real)
    }

    /// `path` is a real folder afterwards (created if missing), or this fails.
    fn ensure_folder(&self, path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(meta) if meta.file_type().is_symlink() => {
                Err(Error::Invalid(format!("{} is a link; Charon never writes through links", path.display())))
            }
            Ok(_) => Err(Error::Invalid(format!("a file named {} is in the way", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
                Ok(()) => {
                    self.touch(path);
                    Ok(())
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => self.ensure_folder(path),
                Err(e) => Err(Error::file(path, e)),
            },
            Err(e) => Err(Error::file(path, e)),
        }
    }

    // ---------------------------------------------------------------- conflict names

    /// A free name for the destination's version of `key`: `notes/plan.txt` → `notes/plan
    /// (conflict 2026-10-16 16.20).txt`, numbered when that name is taken on either side.
    /// Returns its key; the run remembers how it is spelled on each side.
    fn free_conflict_name(&self, key: &str) -> Result<String> {
        let on_destination = self.spellings.disk(Side::Destination, key);
        let name = Keys::name(&on_destination).to_string();
        let folder_on = |side: Side| Keys::parent(key).map(|parent| self.spellings.disk(side, parent));
        let (folder_destination, folder_source) = (folder_on(Side::Destination), folder_on(Side::Source));
        let join = |folder: &Option<String>, name: &str| match folder {
            Some(folder) => format!("{folder}/{name}"),
            None => name.to_string(),
        };
        for n in 1..1000 {
            let candidate = Self::conflict_name(&name, n);
            let (destination, source) = (join(&folder_destination, &candidate), join(&folder_source, &candidate));
            let taken = Self::look(&self.shores.destination.join(&destination))?.is_some() || Self::look(&self.shores.source.join(&source))?.is_some();
            if !taken {
                let conflict_key = self.spellings.key_for(&destination);
                self.spellings.learn(Side::Destination, &conflict_key, destination);
                self.spellings.learn(Side::Source, &conflict_key, source);
                return Ok(conflict_key);
            }
        }
        Err(Error::Invalid("no free name for the conflict copy".into()))
    }

    /// `plan.txt` → `plan (conflict 2026-10-16 16.20).txt` (`… 16.20 2).txt` for n = 2),
    /// shortened to fit the drive's limit on name length.
    pub fn conflict_name(name: &str, n: u32) -> String {
        let stamp = Local::now().format("%Y-%m-%d %H.%M");
        let (stem, extension) = names::split_extension(name);
        let number = if n > 1 { format!(" {n}") } else { String::new() };
        names::fit(stem, &format!(" (conflict {stamp}{number}){extension}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_names_keep_the_extension() {
        let name = Executor::conflict_name("plan.txt", 1);
        assert!(name.starts_with("plan (conflict "));
        assert!(name.ends_with(").txt"));
        assert!(Executor::conflict_name(".env", 1).starts_with(".env (conflict "));
        assert!(Executor::conflict_name("a.txt", 2).ends_with(" 2).txt"));
        assert!(Executor::conflict_name(&"x".repeat(250), 1).len() <= 255);
    }

    #[test]
    fn new_items_take_the_spelling_of_the_side_they_come_from() {
        let mut source = Snapshot::new();
        source.insert("caf\u{e9}", Entry::dir());
        source.insert("caf\u{e9}/menu.txt", Entry::file(1, 1));
        source.facts_mut().spellings.insert("caf\u{e9}".into(), "cafe\u{301}".into());
        source.facts_mut().spellings.insert("caf\u{e9}/menu.txt".into(), "cafe\u{301}/menu.txt".into());
        let mut destination = Snapshot::new();
        destination.insert("caf\u{e9}", Entry::dir());
        let spell = Spellings::new(&source, &destination);
        assert_eq!(spell.disk(Side::Source, "caf\u{e9}/menu.txt"), "cafe\u{301}/menu.txt");
        // The destination already has the folder (spelled NFC there): the file goes inside it.
        assert_eq!(spell.disk(Side::Destination, "caf\u{e9}/menu.txt"), "caf\u{e9}/menu.txt");
        // A new folder takes the source's name, written in NFC.
        let empty = Snapshot::new();
        let spell = Spellings::new(&source, &empty);
        assert_eq!(spell.disk(Side::Destination, "caf\u{e9}/menu.txt"), "caf\u{e9}/menu.txt");
    }
}
