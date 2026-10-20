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
use crate::plan::{Action, Pair, Plan, Settled, Side, Unit};
use crate::run::durable;
use crate::run::names;
use crate::run::rename::{rename_new, swap};
use crate::run::{Copier, ProgressMeter, Room, Trash};
use crate::scan::{is_clutter, Entry, EntryKind, FsKind, Keys, RootId, Scanner, Snapshot};
use chrono::Local;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::ops::ControlFlow;
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
    /// `(path, what)` for every copy that arrived without some of the original's details
    /// ("Finder tags", "resource fork"…), because the receiving drive can't keep them.
    pub unkept: Vec<(String, String)>,
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

/// A copy written next to its target, waiting to take its place (see `Executor::stage`).
struct Staged {
    to: Side,
    key: String,
    from: Entry,
    existing: Option<Entry>,
    settles: bool,
    relative: String,
    target: PathBuf,
    partial: PathBuf,
    /// The original's details the copy couldn't take along.
    unkept: BTreeSet<&'static str>,
}

/// Which file an item is on its drive (its device and inode numbers), to tell it from
/// another file saved under the same name. Known on macOS and Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity(u64, u64);

impl Identity {
    fn of(meta: &fs::Metadata) -> Option<Identity> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Identity(meta.dev(), meta.ino()))
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            None
        }
    }

    fn at(path: &Path) -> Option<Identity> {
        fs::symlink_metadata(path).ok().as_ref().and_then(Self::of)
    }
}

/// The two files of a swap (see `Executor::replace`): the one checked at the target a
/// moment before, and the copy taking its place.
#[derive(Debug, Clone, Copy)]
struct Swapped {
    checked: Option<Identity>,
    copy: Option<Identity>,
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
    unkept: RefCell<Vec<(String, String)>>,
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
            unkept: RefCell::default(),
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

    /// Runs every step of `plan` in order. The files of a database are carried together,
    /// when the first of them comes up (see `carry_database`).
    pub fn run(&self, plan: &Plan, meter: &mut ProgressMeter) -> Outcome {
        let mut outcome = Outcome::default();
        // A side where copies failed keeps everything this run would remove there: the
        // removal could take the last copy of something that didn't arrive.
        let mut copies_failed: HashSet<Side> = HashSet::new();
        let databases = self.databases(plan);
        let mut carried: HashSet<String> = HashSet::new();
        'steps: for (action, makes_room) in plan.ordered_steps() {
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
            if let Some((database, steps)) = self.database_of(action, makes_room).and_then(|database| databases.get_key_value(&database)) {
                if carried.insert(database.clone()) {
                    for (step, result) in self.carry_database(steps, &copies_failed, meter) {
                        if self.record(&mut outcome, &mut copies_failed, step, result).is_break() {
                            break 'steps;
                        }
                    }
                }
                continue;
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
            if self.record(&mut outcome, &mut copies_failed, action, result).is_break() {
                break;
            }
            meter.finish_item();
        }
        meter.finishing();
        outcome.settled = self.settled.take();
        outcome.destroyed = self.destroyed.take();
        outcome.unkept = self.unkept.take();
        outcome
    }

    /// Adds what one step did to the outcome. `Break` when the whole run stops there.
    fn record(&self, outcome: &mut Outcome, copies_failed: &mut HashSet<Side>, action: &Action, result: Result<Done>) -> ControlFlow<()> {
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
                return ControlFlow::Break(());
            }
            Err(Error::LandedElsewhere(reason)) => {
                outcome.stopped = Some(reason);
                return ControlFlow::Break(());
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
        ControlFlow::Continue(())
    }

    // ---------------------------------------------------------------- databases

    /// The database a step's file belongs to, when the step can be carried with the rest of
    /// the database: copies and plain removals of files.
    fn database_of(&self, action: &Action, makes_room: bool) -> Option<String> {
        let groupable = match action {
            Action::Copy { item: EntryKind::File, .. } => true,
            Action::Delete { item: EntryKind::File, other: None, .. } => !makes_room,
            _ => false,
        };
        let exists = |key: &str| self.spellings.snapshot(Side::Source).contains(key) || self.spellings.snapshot(Side::Destination).contains(key);
        groupable.then(|| Unit::database_of(action.path(), exists)).flatten()
    }

    /// The databases with more than one file to carry in this plan, with their steps.
    fn databases<'p>(&self, plan: &'p Plan) -> HashMap<String, Vec<&'p Action>> {
        let mut databases: HashMap<String, Vec<&'p Action>> = HashMap::new();
        for (action, makes_room) in plan.ordered_steps() {
            if let Some(database) = self.database_of(action, makes_room) {
                databases.entry(database).or_default().push(action);
            }
        }
        databases.retain(|_, steps| steps.len() > 1);
        databases
    }

    /// Carries the files of one database (an SQLite file and its journals) together. Its
    /// journals only fit the database they were written with, so a mix of two versions can
    /// damage it: every new version is written next to its target first, then everything
    /// is checked again, and only then do the old journals go and the new files take their
    /// places, one right after the other. If any file can't be carried or changed meanwhile,
    /// none is, and the next sync tries again.
    fn carry_database<'p>(&self, steps: &[&'p Action], copies_failed: &HashSet<Side>, meter: &mut ProgressMeter) -> Vec<(&'p Action, Result<Done>)> {
        let all_wait = |why: &str| steps.iter().map(|step| (*step, Ok(Done::Skipped(why.to_string())))).collect::<Vec<_>>();
        let removals: Vec<&Action> = steps.iter().copied().filter(|step| matches!(step, Action::Delete { .. })).collect();
        if removals.iter().any(|step| matches!(step, Action::Delete { side, .. } if copies_failed.contains(side))) {
            return all_wait("kept for now: other copies failed in this run, and a database's files are only synced together");
        }

        // Every new version, written and checked next to its target.
        let mut staged: Vec<(&Action, Staged)> = Vec::new();
        for step in steps {
            let Action::Copy { to, path, from, existing, settles_conflict, .. } = step else { continue };
            meter.start(&self.spellings.disk(to.opposite(), path));
            let result = self.stage(*to, path, from, existing.as_ref(), *settles_conflict, meter);
            meter.finish_item();
            match result {
                Ok(Some(ready)) => staged.push((*step, ready)),
                failed => {
                    Self::discard_all(staged);
                    let failed = failed.map(|_| Done::Skipped(format!("changed on the {} side since the scan", to.word())));
                    return Self::one_failed(steps, step, failed);
                }
            }
        }

        // Nothing may have changed while the others were written.
        let unchanged = staged.iter().all(|(_, ready)| {
            let source = self.path(ready.to.opposite(), &ready.key);
            matches!(self.look_fresh(ready.to.opposite(), &source), Ok(now) if Entry::still(now.as_ref(), Some(&ready.from)))
                && matches!(self.look_fresh(ready.to, &ready.target), Ok(now) if Entry::still(now.as_ref(), ready.existing.as_ref()))
        }) && removals.iter().all(|step| match step {
            Action::Delete { side, path, entry, .. } => {
                let (here, there) = (self.path(*side, path), self.path(side.opposite(), path));
                matches!(self.look_fresh(*side, &here), Ok(now) if Entry::still(now.as_ref(), Some(entry)))
                    && matches!(self.look_fresh(side.opposite(), &there), Ok(None))
            }
            _ => true,
        });
        if !unchanged {
            Self::discard_all(staged);
            return all_wait("left for the next sync: a file of its database changed while the others were being copied");
        }

        // The old journals go first, then the new files take their places.
        let mut results: Vec<(&Action, Result<Done>)> = Vec::new();
        let mut broken = false;
        for step in removals {
            meter.start(&self.spellings.disk(Self::shown_side(step), step.path()));
            let result = if broken { Ok(Done::Skipped(Self::NOT_MIXED.into())) } else { self.apply(step, meter) };
            meter.finish_item();
            broken |= !matches!(result, Ok(Done::Deleted | Done::Nothing));
            results.push((step, result));
        }
        for (step, ready) in staged {
            if broken {
                Copier::discard(&ready.partial);
                results.push((step, Ok(Done::Skipped(Self::NOT_MIXED.into()))));
                continue;
            }
            let (to, key, from) = (ready.to, ready.key.clone(), ready.from);
            let result = match self.commit(ready) {
                Ok(Some((bytes, landed))) => {
                    self.settle(&key, Some(Self::pair(to, landed, from)));
                    Ok(Done::Copied(1, bytes))
                }
                Ok(None) => Ok(Done::Skipped(format!("changed on the {} side since the scan", to.word()))),
                Err(e) => Err(e),
            };
            broken |= !matches!(result, Ok(Done::Copied(..)));
            results.push((step, result));
        }
        results
    }

    /// Why a database file waits when another file of it couldn't be carried.
    const NOT_MIXED: &'static str = "left for the next sync: another file of its database couldn't be synced, and a database's files only go together";

    /// `failing`'s own result, and every other step of its database waiting (the failing
    /// one last, so a stop of the whole run comes after the others are counted).
    fn one_failed<'p>(steps: &[&'p Action], failing: &&'p Action, result: Result<Done>) -> Vec<(&'p Action, Result<Done>)> {
        let mut results: Vec<(&Action, Result<Done>)> = steps
            .iter()
            .filter(|step| !std::ptr::eq(**step, *failing))
            .map(|step| (*step, Ok(Done::Skipped(Self::NOT_MIXED.into()))))
            .collect();
        results.push((*failing, result));
        results
    }

    fn discard_all(staged: Vec<(&Action, Staged)>) {
        for (_, ready) in staged {
            Copier::discard(&ready.partial);
        }
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

    /// Makes everything this run changed durable: every folder it touched (and the trash's
    /// lists of what went in), then each drive. Until this succeeds, nothing about the run
    /// may be recorded as done.
    pub fn flush(&self) -> Result<()> {
        self.trash.write_lists();
        self.touched.borrow_mut().extend(self.trash.folders());
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
        match self.stage(to, key, from, existing, settles, meter)? {
            Some(staged) => self.commit(staged),
            None => Ok(None),
        }
    }

    /// The first half of a copy: the new version, written and checked next to its target.
    /// `None` when the target changed since the scan.
    fn stage(&self, to: Side, key: &str, from: &Entry, existing: Option<&Entry>, settles: bool, meter: &mut ProgressMeter) -> Result<Option<Staged>> {
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

        let (partial, unkept) = match from.kind {
            EntryKind::File => {
                self.room_for(to, from.size)?;
                let coarse = self.spellings.snapshot(to.opposite()).volume().is_some_and(|v| v.coarse_times());
                let mut copier = Copier::new(self.cancel).coarse_source_times(coarse);
                if !self.holds_attributes(to) {
                    copier = copier.without_attributes();
                }
                let written = copier.file(&from_path, &target, from, &mut |n| meter.add_bytes(n))?;
                (written.partial, written.unkept)
            }
            EntryKind::Link => (Copier::link(&from_path, &target, from)?, BTreeSet::new()),
            EntryKind::Dir => return Err(Error::Invalid("a folder can't be copied as a file".into())),
        };
        self.touch(&target);
        // One last look, fresh from the drive: the target must still be what the plan saw.
        if !Entry::still(self.look_fresh(to, &target)?.as_ref(), existing) {
            Copier::discard(&partial);
            return Ok(None);
        }
        Ok(Some(Staged { to, key: key.to_string(), from: *from, existing: existing.copied(), settles, relative, target, partial, unkept }))
    }

    /// The second half of a copy: the new version takes the target's place. Returns the
    /// bytes carried and the item as it landed, or `None` when the target changed meanwhile.
    fn commit(&self, staged: Staged) -> Result<Option<(u64, Entry)>> {
        let Staged { to, key, from, existing, settles, relative, target, partial, unkept } = staged;
        let root = self.shores.root(to);
        let keep_old = self.care.safe_delete || self.care.keep_replaced || settles;
        match existing {
            None => Copier::commit_new(&partial, &target)?,
            Some(old) if old.is_dir() => {
                Copier::discard(&partial);
                return Err(Error::Invalid(format!("a folder named {} is in the way", target.display())));
            }
            Some(old) if keep_old && old.is_file() => {
                // Keep a copy of the old version, then replace it in place (see `Trash`).
                let kept = match self.room_for_kept_copy(to, old.size).and_then(|()| self.trash.preserve(root, &relative).map_err(|e| Error::file(&target, e))) {
                    Ok(kept) => kept,
                    Err(e) => {
                        Copier::discard(&partial);
                        return Err(e);
                    }
                };
                self.touch(&kept);
                if !self.replace(to, &partial, &target, &relative, &old)? {
                    let _ = fs::remove_file(&kept);
                    return Ok(None);
                }
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
            Some(old) => {
                if !self.replace(to, &partial, &target, &relative, &old)? {
                    return Ok(None);
                }
            }
        }
        if existing.is_some() {
            self.destroyed_on(to, &key);
        }
        let landed = Self::look(&target)?.ok_or_else(|| Error::Invalid(format!("{} vanished right after it was copied", target.display())))?;
        self.on_its_drive(to, &target)?;
        if !unkept.is_empty() {
            self.unkept.borrow_mut().push((key, unkept.into_iter().collect::<Vec<_>>().join(", ")));
        }
        Ok(Some((if from.is_file() { from.size } else { 0 }, landed)))
    }

    /// Puts the finished copy `partial` in the place of `target` (`relative` in `to`'s
    /// folder), which must still be `expected`. Returns `false`, with the copy thrown away,
    /// when it isn't: an app saved over it a moment ago.
    ///
    /// Where the drive can swap two names in one step, the copy and the old item swap, and
    /// the old item, now under the copy's temporary name, is checked: if it isn't the one
    /// that was checked a moment before (an app saved a new version at that very instant),
    /// the two swap back. Elsewhere the old item is renamed aside and checked first, and
    /// only then does the copy take the name, which it never takes from anything that
    /// appeared there meanwhile. Either way, nothing an app saved is ever written over: it's
    /// put back where it was, or kept in the trash.
    fn replace(&self, to: Side, partial: &Path, target: &Path, relative: &str, expected: &Entry) -> Result<bool> {
        // One last look, fresh from the drive, and which file it is.
        let checked = match self.look_fresh(to, target) {
            Ok(now) if Entry::still(now.as_ref(), Some(expected)) => fs::symlink_metadata(target).ok().filter(|meta| {
                Self::entry_of(target, meta).is_ok_and(|now| now.unchanged(expected))
            }),
            Ok(_) => None,
            Err(e) => {
                Copier::discard(partial);
                return Err(e);
            }
        };
        let Some(checked) = checked else {
            Copier::discard(partial);
            return Ok(false);
        };
        Self::make_writable(target);
        let swapped = Swapped { checked: Identity::of(&checked), copy: Identity::at(partial) };
        match swap(partial, target) {
            Ok(()) => self.check_swapped(to, partial, target, relative, expected, swapped),
            Err(e) if e.kind() == io::ErrorKind::Unsupported => self.replace_aside(to, partial, target, relative, expected),
            Err(e) => {
                Copier::discard(partial);
                Err(Error::file(target, e))
            }
        }
    }

    /// After a swap, `partial` holds what was at `target`: removed if it's what was checked
    /// (the same file, as it was), swapped back otherwise.
    fn check_swapped(&self, to: Side, partial: &Path, target: &Path, relative: &str, expected: &Entry, swapped: Swapped) -> Result<bool> {
        let swapped_out = fs::symlink_metadata(partial).ok();
        let same = swapped_out.as_ref().is_some_and(|meta| {
            swapped.checked.is_none_or(|checked| Identity::of(meta) == Some(checked))
                && Self::entry_of(partial, meta).is_ok_and(|found| Self::same_after_rename(&found, expected))
        });
        if same {
            if let Err(e) = Self::remove_item(partial) {
                // It stays under the temporary name, and the next sync moves it into the trash.
                log::warn!("couldn't remove the replaced version of {}: {e}", target.display());
            }
            return Ok(true);
        }
        log::info!("{} was saved again while Charon replaced it; that version stays", target.display());
        match swap(partial, target) {
            Ok(()) if swapped.copy.is_some() && Identity::at(partial) == swapped.copy => {
                Copier::discard(partial);
                Ok(false)
            }
            // Yet another version took the name in between: the one swapped out is kept too.
            Ok(()) => {
                self.keep_aside(to, partial, relative);
                Ok(false)
            }
            Err(e) => {
                self.keep_aside(to, partial, relative);
                Err(Error::Changed(format!("an app saved a new version while Charon replaced it ({e}); that version is in .charon-trash")))
            }
        }
    }

    /// `replace` on a drive that can't swap names: the old item is renamed aside and checked,
    /// then the copy takes the free name.
    fn replace_aside(&self, to: Side, partial: &Path, target: &Path, relative: &str, expected: &Entry) -> Result<bool> {
        let aside = match Self::move_aside(target) {
            Ok(Some(aside)) => aside,
            // Removed meanwhile: not what the plan saw anymore.
            Ok(None) => {
                Copier::discard(partial);
                return Ok(false);
            }
            Err(e) => {
                Copier::discard(partial);
                return Err(e);
            }
        };
        let moved = self.look_fresh(to, &aside);
        if !matches!(&moved, Ok(Some(found)) if Self::same_after_rename(found, expected)) {
            Copier::discard(partial);
            self.put_back(to, &aside, target, relative);
            return moved.map(|_| false);
        }
        match rename_new(partial, target) {
            Ok(()) => {
                if let Err(e) = Self::remove_item(&aside) {
                    log::warn!("couldn't remove the replaced version of {}: {e}", target.display());
                }
                Ok(true)
            }
            Err(e) => {
                Copier::discard(partial);
                self.put_back(to, &aside, target, relative);
                if e.kind() == io::ErrorKind::AlreadyExists {
                    Ok(false)
                } else {
                    Err(Error::file(target, e))
                }
            }
        }
    }

    /// Renames the item at `path` to a fresh temporary name next to it, and returns that
    /// name; `None` if nothing is there anymore.
    fn move_aside(path: &Path) -> Result<Option<PathBuf>> {
        for _ in 0..5 {
            let aside = Copier::partial_path(path);
            match rename_new(path, &aside) {
                Ok(()) => return Ok(Some(aside)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(None),
                Err(e) => return Err(Error::file(path, e)),
            }
        }
        Err(Error::Invalid(format!("no free temporary name next to {}", path.display())))
    }

    /// Puts an item Charon renamed aside back under its name, or, when something new took
    /// the name meanwhile, into the trash: never lost, never written over.
    fn put_back(&self, side: Side, aside: &Path, target: &Path, relative: &str) {
        if let Err(e) = rename_new(aside, target) {
            log::info!("couldn't put {} back ({e}); it goes into the trash", target.display());
            self.keep_aside(side, aside, relative);
        }
    }

    /// Moves an item Charon renamed aside into the trash, under its own name.
    fn keep_aside(&self, side: Side, aside: &Path, relative: &str) {
        match self.trash.put_from(aside, self.shores.root(side), relative) {
            Ok(kept) => self.touch(&kept),
            // Under its temporary name, the next sync moves it into the trash.
            Err(e) => log::error!("couldn't move {} into the trash: {e}; it stays at {}", relative, aside.display()),
        }
    }

    /// Whether an item found after it was renamed is still the one seen before: the same
    /// in everything but its change time, which a rename moves.
    fn same_after_rename(found: &Entry, before: &Entry) -> bool {
        found.changed_at(0).unchanged(before)
    }

    /// Removes a file or link for good.
    fn remove_item(path: &Path) -> io::Result<()> {
        Self::make_writable(path);
        fs::remove_file(path)
    }

    /// The room for a new file of `bytes` on `side`'s drive.
    fn room_for(&self, side: Side, bytes: u64) -> Result<()> {
        match Room::shortage(self.shores.root(side), bytes, &format!("the {}'s drive", side.word())) {
            Some(why) => Err(Error::NoRoom(why)),
            None => Ok(()),
        }
    }

    /// The room for keeping a copy of a replaced version of `bytes`: none on a drive where
    /// that copy is a clone.
    fn room_for_kept_copy(&self, side: Side, bytes: u64) -> Result<()> {
        if self.clones(side) {
            Ok(())
        } else {
            self.room_for(side, bytes).map_err(|e| Error::NoRoom(format!("{e} (to keep the replaced version in .charon-trash)")))
        }
    }

    /// Whether copies on `side`'s drive are clones, instant and taking no room (APFS).
    fn clones(&self, side: Side) -> bool {
        self.spellings.snapshot(side).volume().is_some_and(|v| v.kind == FsKind::Apfs)
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
            self.remove_folder(side, &relative, &target)?
        } else if self.remove(side, &relative, &target, self.care.safe_delete, Some(planned))? {
            self.destroyed_on(side, key);
            Done::Deleted
        } else {
            return Ok(Done::Skipped(format!("changed on the {} side since the scan", side.word())));
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

    /// Into the trash (`keep`), or deleted for good. Links are removed as links. With the
    /// item the plan saw (`planned`), it's renamed aside before it goes, and checked once
    /// more: a version an app saved there at that very moment stays. Returns `false` when
    /// that happened.
    ///
    /// On APFS a kept file isn't moved into the trash but cloned there (instant, and it takes
    /// no room), and then removed: an app that has it open follows the file itself, not its
    /// name, so a moved file would take the person's next saves into the trash with it.
    /// Removed, the app saves again where the file was, and the next sync sees it.
    fn remove(&self, side: Side, relative: &str, target: &Path, keep: bool, planned: Option<&Entry>) -> Result<bool> {
        let root = self.shores.root(side);
        let Some(planned) = planned else {
            if keep {
                let kept = self.trash.put(root, relative).map_err(|e| Error::file(target, e))?;
                self.touch(&kept);
            } else {
                Self::remove_item(target).map_err(|e| Error::file(target, e))?;
            }
            return Ok(true);
        };
        if keep && planned.is_file() && self.clones(side) {
            let kept = self.trash.preserve(root, relative).map_err(|e| Error::file(target, e))?;
            self.touch(&kept);
            let removed = self.remove_checked(side, relative, target, planned);
            if !matches!(removed, Ok(true)) {
                let _ = fs::remove_file(&kept);
            }
            return removed;
        }
        if keep {
            let Some(aside) = Self::move_aside(target)? else { return Ok(true) };
            let moved = self.look_fresh(side, &aside);
            if !matches!(&moved, Ok(Some(found)) if Self::same_after_rename(found, planned)) {
                self.put_back(side, &aside, target, relative);
                return moved.map(|_| false);
            }
            let kept = self.trash.put_from(&aside, root, relative).map_err(|e| {
                self.put_back(side, &aside, target, relative);
                Error::file(target, e)
            })?;
            self.touch(&kept);
            return Ok(true);
        }
        self.remove_checked(side, relative, target, planned)
    }

    /// Removes the file or link at `target` for good, if it's still `planned`: renamed aside,
    /// checked, then removed. `false` when it isn't (it's put back).
    fn remove_checked(&self, side: Side, relative: &str, target: &Path, planned: &Entry) -> Result<bool> {
        Self::make_writable(target);
        // Gone already: what the plan wanted.
        let Some(aside) = Self::move_aside(target)? else { return Ok(true) };
        let moved = self.look_fresh(side, &aside);
        if !matches!(&moved, Ok(Some(found)) if Self::same_after_rename(found, planned)) {
            self.put_back(side, &aside, target, relative);
            return moved.map(|_| false);
        }
        match Self::remove_item(&aside) {
            Ok(()) => Ok(true),
            Err(e) => {
                self.put_back(side, &aside, target, relative);
                Err(Error::file(target, e))
            }
        }
    }

    /// Windows refuses to replace or delete a read-only file or folder (Explorer marks
    /// customised folders so); the item was checked to be exactly what the plan saw, so its
    /// read-only mark is lifted first.
    fn make_writable(path: &Path) {
        #[cfg(windows)]
        if let Ok(meta) = fs::symlink_metadata(path) {
            let mut permissions = meta.permissions();
            if (meta.is_file() || meta.is_dir()) && permissions.readonly() {
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
    fn remove_folder(&self, side: Side, relative: &str, target: &Path) -> Result<Done> {
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
        let root = self.shores.root(side);
        for item in &clutter {
            self.remove(side, item, &root.join(item), true, None)?;
        }
        self.real_folders.borrow_mut().clear();
        Self::make_writable(target);
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

    /// `look`, but on a drive that may show changes late (a network share, a cloud drive) a
    /// file is opened and asked directly: its listing can show another computer's change late.
    fn look_fresh(&self, side: Side, path: &Path) -> Result<Option<Entry>> {
        let seen = Self::look(path)?;
        let late = self.spellings.snapshot(side).volume().is_some_and(|v| v.shows_changes_late());
        match seen {
            Some(entry) if late && entry.is_file() => match fs::File::open(path).and_then(|file| file.metadata()) {
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
        match fs::symlink_metadata(path) {
            Ok(meta) => Self::entry_of(path, &meta).map(Some),
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(None),
            Err(e) => Err(Error::file(path, e)),
        }
    }

    /// The item at `path`, from its details `meta`.
    fn entry_of(path: &Path, meta: &fs::Metadata) -> Result<Entry> {
        let kind = meta.file_type();
        if kind.is_dir() {
            Ok(Entry::dir())
        } else if kind.is_file() {
            Ok(Entry::file(meta.len(), Scanner::modified_ms(meta)).changed_at(Scanner::change_time(meta)))
        } else if kind.is_symlink() {
            Self::link_entry(path, meta)
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

    const KEEP: Care = Care { safe_delete: true, keep_replaced: true };

    /// Two empty folders for an executor to work in.
    fn shores() -> (tempfile::TempDir, Shores) {
        let tmp = tempfile::tempdir().unwrap();
        let shores = Shores { source: tmp.path().join("source"), destination: tmp.path().join("destination") };
        fs::create_dir_all(&shores.source).unwrap();
        fs::create_dir_all(&shores.destination).unwrap();
        (tmp, shores)
    }

    fn write(path: &Path, text: &str, seconds: i64) {
        fs::write(path, text).unwrap();
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(seconds, 0)).unwrap();
    }

    fn seen(path: &Path) -> Entry {
        Executor::look(path).unwrap().unwrap()
    }

    /// The names in a folder, Charon's own trash left out.
    fn names_in(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder).unwrap().flatten().map(|i| i.file_name().to_string_lossy().into_owned()).filter(|n| n != crate::scan::TRASH_FOLDER).collect();
        names.sort();
        names
    }

    /// An app saves a new version (a new file under the same name, even with the same size
    /// and time) at the very instant Charon swaps its copy in: the app's version goes back.
    #[test]
    fn a_version_saved_while_replacing_is_swapped_back() {
        let (_tmp, shores) = shores();
        let empty = Snapshot::new();
        let cancel = AtomicBool::new(false);
        let executor = Executor::new(&shores, &empty, &empty, KEEP, &cancel);
        let target = shores.destination.join("notes.txt");
        write(&target, "old", 1_600_000_000);
        let (planned, checked) = (seen(&target), Identity::at(&target));
        let saved = shores.destination.join("saved-by-the-app");
        write(&saved, "app", 1_600_000_000);
        fs::rename(&saved, &target).unwrap();
        let partial = Copier::partial_path(&target);
        write(&partial, "new version", 1_700_000_000);
        let swapped = Swapped { checked, copy: Identity::at(&partial) };
        if swap(&partial, &target).is_err() {
            return; // a drive that can't swap: `replace_aside` covers it
        }
        assert!(!executor.check_swapped(Side::Destination, &partial, &target, "notes.txt", &planned, swapped).unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap(), "app");
        assert_eq!(names_in(&shores.destination), vec!["notes.txt"], "the copy is gone, nothing else is left");
    }

    #[test]
    fn a_checked_version_is_replaced_and_nothing_is_left_behind() {
        let (_tmp, shores) = shores();
        let empty = Snapshot::new();
        let cancel = AtomicBool::new(false);
        let executor = Executor::new(&shores, &empty, &empty, KEEP, &cancel);
        let target = shores.destination.join("notes.txt");
        write(&target, "old", 1_600_000_000);
        let planned = seen(&target);
        let partial = Copier::partial_path(&target);
        write(&partial, "new version", 1_700_000_000);
        assert!(executor.replace(Side::Destination, &partial, &target, "notes.txt", &planned).unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap(), "new version");
        assert_eq!(names_in(&shores.destination), vec!["notes.txt"]);

        // Where the drive can't swap names, the old version goes aside first.
        let partial = Copier::partial_path(&target);
        write(&partial, "newest", 1_800_000_000);
        assert!(executor.replace_aside(Side::Destination, &partial, &target, "notes.txt", &seen(&target)).unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap(), "newest");
        assert_eq!(names_in(&shores.destination), vec!["notes.txt"]);
    }

    #[test]
    fn a_changed_file_is_never_replaced_aside_either() {
        let (_tmp, shores) = shores();
        let empty = Snapshot::new();
        let cancel = AtomicBool::new(false);
        let executor = Executor::new(&shores, &empty, &empty, KEEP, &cancel);
        let target = shores.destination.join("notes.txt");
        write(&target, "old", 1_600_000_000);
        let planned = seen(&target);
        write(&target, "saved by an app", 1_600_000_100);
        let partial = Copier::partial_path(&target);
        write(&partial, "new version", 1_700_000_000);
        assert!(!executor.replace_aside(Side::Destination, &partial, &target, "notes.txt", &planned).unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap(), "saved by an app");
        assert_eq!(names_in(&shores.destination), vec!["notes.txt"]);
        assert!(!executor.replace(Side::Destination, &Copier::partial_path(&target), &target, "notes.txt", &planned).unwrap());
    }

    /// Something new took the name while the old item was aside: the old item goes into the
    /// trash rather than over it.
    #[test]
    fn an_item_put_back_never_replaces_a_newer_one() {
        let (_tmp, shores) = shores();
        let empty = Snapshot::new();
        let cancel = AtomicBool::new(false);
        let executor = Executor::new(&shores, &empty, &empty, KEEP, &cancel);
        let target = shores.destination.join("notes.txt");
        write(&target, "old", 1_600_000_000);
        let aside = Executor::move_aside(&target).unwrap().unwrap();
        write(&target, "newer", 1_700_000_000);
        executor.put_back(Side::Destination, &aside, &target, "notes.txt");
        assert_eq!(fs::read_to_string(&target).unwrap(), "newer");
        assert_eq!(names_in(&shores.destination), vec!["notes.txt"]);
        let trash = Trash::look(&shores.destination, 0, true);
        assert_eq!(trash.bytes, 3, "the old one is in the trash");
    }

    #[test]
    fn a_removal_leaves_a_file_that_changed_at_the_last_moment() {
        let empty = Snapshot::new();
        let cancel = AtomicBool::new(false);
        for (care, keep) in [(KEEP, true), (Care { safe_delete: false, keep_replaced: false }, false)] {
            let (_tmp, shores) = shores();
            let executor = Executor::new(&shores, &empty, &empty, care, &cancel);
            let target = shores.destination.join("plan.txt");
            write(&target, "old", 1_600_000_000);
            let planned = seen(&target);
            write(&target, "saved again", 1_600_000_100);
            assert!(!executor.remove(Side::Destination, "plan.txt", &target, keep, Some(&planned)).unwrap());
            assert_eq!(fs::read_to_string(&target).unwrap(), "saved again");
            assert_eq!(names_in(&shores.destination), vec!["plan.txt"]);

            let planned = seen(&target);
            assert!(executor.remove(Side::Destination, "plan.txt", &target, keep, Some(&planned)).unwrap());
            assert!(names_in(&shores.destination).is_empty());
            assert_eq!(Trash::look(&shores.destination, 0, true).bytes > 0, keep, "kept in the trash only with safe deletes");
        }
    }

    /// Windows: a path of over 300 characters and read-only files and folders are replaced,
    /// kept and removed like any others.
    #[cfg(windows)]
    #[test]
    fn long_paths_and_read_only_items_work_on_windows() {
        fn read_only(path: &Path) {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(path, permissions).unwrap();
        }
        let empty = Snapshot::new();
        let cancel = AtomicBool::new(false);
        for care in [KEEP, Care { safe_delete: false, keep_replaced: false }] {
            let (_tmp, shores) = shores();
            let executor = Executor::new(&shores, &empty, &empty, care, &cancel);
            let folder = (0..6).map(|i| format!("{i}{}", "x".repeat(50))).collect::<Vec<_>>().join("/");
            let relative = format!("{folder}/notes.txt");
            let target = shores.destination.join(&relative);
            assert!(target.as_os_str().len() > 300);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            write(&target, "old", 1_600_000_000);
            read_only(&target);

            if care.keep_replaced {
                let kept = executor.trash.preserve(&shores.destination, &relative).unwrap();
                assert_eq!(fs::read_to_string(kept).unwrap(), "old");
            }
            let partial = Copier::partial_path(&target);
            write(&partial, "new", 1_700_000_000);
            assert!(executor.replace(Side::Destination, &partial, &target, &relative, &seen(&target)).unwrap());
            assert_eq!(fs::read_to_string(&target).unwrap(), "new");

            read_only(&target);
            assert!(executor.remove(Side::Destination, &relative, &target, care.safe_delete, Some(&seen(&target))).unwrap());
            assert!(!target.exists());

            let emptied = shores.destination.join("0".to_string() + &"x".repeat(50));
            let inner = folder.split('/').skip(1).collect::<Vec<_>>();
            for depth in (0..inner.len()).rev() {
                let path = emptied.join(inner[..=depth].join("/"));
                read_only(&path);
                let relative = path.strip_prefix(&shores.destination).unwrap().to_string_lossy().replace('\\', "/");
                assert!(matches!(executor.remove_folder(Side::Destination, &relative, &path).unwrap(), Done::Deleted));
            }
        }
    }
}
