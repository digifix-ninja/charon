//! Charon's note that a sync is at work in a folder: `.charon/<id>.busy` at the top of each
//! folder of a running sync, wherever Charon can write there. It names the job, the Charon
//! the sync runs in and the computer, with a heartbeat refreshed while the sync runs.
//!
//! Charon keeps two of its own syncs apart by their folders' paths, and a path can't always
//! tell: a share mounted under two names is one place under two paths. Another Charon (a
//! second computer syncing the same share, a development build next to the installed app)
//! can't see this one's syncs at all. Notes are seen by both. Before a sync changes
//! anything, it leaves its notes and then looks for the notes of other syncs: in its own
//! folders, in the folders above them, and in the folders inside them that hold Charon's
//! own folder. A note of another sync means the folders are busy, and the sync waits. Two
//! syncs that start at the same moment each find the other's note, so at most one of them
//! goes ahead.
//!
//! Whether the sync of a note is still at work is judged by this computer's clock alone,
//! since two computers' clocks can be minutes or hours apart: a note that hasn't changed for
//! two minutes since this Charon first saw it as it is belongs to a Charon that stopped. It
//! counts for nothing, and is tidied away. A note this Charon left itself before it last quit
//! or crashed counts for nothing at once (see `BusyWatch`). A note that can't be read counts
//! as long as its file keeps changing, and is never removed: it's better to wait a little
//! than to sync next to another sync.

use crate::error::Error;
use crate::model::JobId;
use crate::run::Marker;
use crate::scan::MARKER_FOLDER;
use crate::store::atomic;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// What the name of a busy note ends with.
pub const BUSY_EXTENSION: &str = ".busy";
/// How often a running sync refreshes its notes.
const HEARTBEAT: Duration = Duration::from_secs(20);
/// A note that stayed the same this long, by this computer's clock, belongs to a Charon that
/// stopped.
const STALE_AFTER: Duration = Duration::from_secs(120);
/// How many of the Charons that worked with a data folder before are remembered.
const EARLIER_KEPT: usize = 20;

/// What a busy note says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusyNote {
    pub job: JobId,
    /// The job's name, for the person.
    pub name: String,
    /// The Charon the sync runs in: a new one each time Charon starts.
    pub instance: String,
    /// The computer it runs on.
    pub host: String,
    /// When the sync last said it's still at work, by its own computer's clock. Other
    /// computers only look at whether it changes.
    pub heartbeat: DateTime<Utc>,
}

impl BusyNote {
    /// A note for a sync of `job` (named `name`) in the Charon `instance`, on this computer.
    pub fn new(job: &JobId, name: &str, instance: &str) -> Self {
        Self { job: job.clone(), name: name.to_string(), instance: instance.to_string(), host: this_computer(), heartbeat: Utc::now() }
    }

    /// Whether the note comes from a Charon on this computer.
    pub fn from_this_computer(&self) -> bool {
        self.host == this_computer()
    }

    /// Whether the note is one of `me`'s own: left by the same job in the same Charon (by
    /// its sync running now, or by an earlier one that couldn't take it away).
    fn is_of(&self, me: &BusyNote) -> bool {
        self.job == me.job && self.instance == me.instance
    }

    /// A note that can't be read, as the note of a sync nobody can name.
    fn unknown(written: SystemTime) -> Self {
        Self { job: String::new(), name: String::new(), instance: String::new(), host: String::new(), heartbeat: written.into() }
    }
}

/// A busy note as seen: what it says, or, when it can't be read, when its file last changed.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Note(BusyNote),
    Unreadable(SystemTime),
}

impl Seen {
    /// The note as other syncs are told about it.
    fn note(&self) -> BusyNote {
        match self {
            Seen::Note(note) => note.clone(),
            Seen::Unreadable(written) => BusyNote::unknown(*written),
        }
    }
}

/// A busy note as this Charon last saw it, and since when it has looked like that.
struct Sighting {
    seen: Seen,
    since: Instant,
}

/// What this Charon knows of busy notes: when it first saw each note as it is now (by this
/// computer's clock), which notes its own syncs leave now, and which Charons worked with its
/// data folder before.
///
/// Only one Charon on a computer works with a data folder at a time (see `store::DataLock`),
/// so the Charons that worked with it here before have all stopped, and their notes are
/// leftovers: a sync that was cut short by a crash, or by a quit that didn't wait for it.
/// They're known by the list of those Charons kept in the data folder. A copy of the data
/// folder (moved to a new computer while the old one still syncs, or copied into another
/// folder) brings the list along, so the list keeps where each Charon ran: one that ran with
/// another computer or another folder may still be at work there, and its notes count like
/// any other Charon's.
pub struct BusyWatch {
    /// This Charon among others that may sync the same folders: new each time it starts.
    instance: String,
    /// The Charons that worked with this data folder on this computer before this one.
    earlier: HashSet<String>,
    known: Mutex<Known>,
}

/// One start of Charon, as the list of Charons in a data folder keeps it: the Charon, and
/// the computer and the data folder it ran with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Launch {
    instance: String,
    host: String,
    folder: PathBuf,
}

impl Launch {
    /// The start of the Charon `instance` on this computer, with the data folder whose list
    /// of Charons is `list`.
    fn here(instance: &str, list: &Path) -> Self {
        let folder = list.parent().map(Path::to_path_buf).unwrap_or_default();
        Self { instance: instance.to_string(), host: this_computer(), folder }
    }

    /// Whether this Charon ran on the same computer and with the same data folder as `now`:
    /// the data folder's lock then kept them apart, so it has stopped.
    fn ran_like(&self, now: &Launch) -> bool {
        self.host == now.host && self.folder == now.folder
    }
}

#[derive(Default)]
struct Known {
    sightings: HashMap<PathBuf, Sighting>,
    /// The file names of the notes of this Charon's syncs at work now (the same under any
    /// name a folder is reached by).
    mine: HashSet<OsString>,
}

impl BusyWatch {
    /// The watch of a Charon that just started with the data folder whose list of Charons is
    /// `list`: this one is added to it.
    pub fn open(list: &Path) -> Self {
        let instance = uuid::Uuid::new_v4().simple().to_string();
        let now = Launch::here(&instance, list);
        let before: Vec<Launch> = fs::read(list).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let earlier = before.iter().filter(|launch| launch.ran_like(&now)).map(|launch| launch.instance.clone()).collect();
        let mut listed = vec![now];
        listed.extend(before.into_iter().take(EARLIER_KEPT - 1));
        let saved = serde_json::to_vec(&listed).map_err(Error::from).and_then(|bytes| atomic::write(list, &bytes));
        if let Err(e) = saved {
            // Its notes are then told apart from other Charons' only by their heartbeat,
            // after a crash: two minutes later.
            log::warn!("couldn't save the list of Charons in {}: {e}", list.display());
        }
        Self { instance, earlier, known: Mutex::default() }
    }

    /// This Charon among others that may sync the same folders.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// A note for a sync of `job` (named `name`) in this Charon.
    pub fn note(&self, job: &JobId, name: &str) -> BusyNote {
        BusyNote::new(job, name, &self.instance)
    }

    /// The notes of syncs other than `me` around `roots` that are still at work: in their
    /// Charon folders, in those of every folder above them, and in those of the folders
    /// `inside` them. Notes whose Charon stopped are tidied away (only those Charon can
    /// read: a file it can't read may not be a note at all).
    pub fn around(&self, roots: &[&Path], inside: &[PathBuf], me: &BusyNote) -> Vec<BusyNote> {
        self.around_at(roots, inside, me, Instant::now())
    }

    /// `around`, as of `now` by this computer's clock.
    fn around_at(&self, roots: &[&Path], inside: &[PathBuf], me: &BusyNote, now: Instant) -> Vec<BusyNote> {
        // The folders are read before the lock is taken: a share that doesn't answer
        // mustn't hold up the other jobs' syncs.
        let found: Vec<(PathBuf, Vec<(PathBuf, Seen)>)> = Self::places(roots, inside)
            .into_iter()
            .map(|place| {
                let folder = place.join(MARKER_FOLDER);
                let notes = Self::in_folder(&folder);
                (folder, notes)
            })
            .collect();
        let mut busy = Vec::new();
        let mut stopped = Vec::new();
        {
            let mut known = self.known();
            for (folder, notes) in found {
                // A note seen here before and gone now is forgotten.
                known.sightings.retain(|file, _| file.parent() != Some(folder.as_path()) || notes.iter().any(|(note, _)| note == file));
                for (file, seen) in notes {
                    match self.is_at_work(&mut known, &file, &seen, me, now) {
                        Some(true) => busy.push(seen.note()),
                        Some(false) if matches!(seen, Seen::Note(_)) => stopped.push(file),
                        _ => {}
                    }
                }
            }
        }
        for file in stopped {
            let _ = fs::remove_file(&file);
        }
        busy
    }

    /// Whether the sync that left a note (in `file`, seen as `seen`) is still at work, or
    /// `None` for `me`'s own note.
    fn is_at_work(&self, known: &mut Known, file: &Path, seen: &Seen, me: &BusyNote, now: Instant) -> Option<bool> {
        if let Seen::Note(note) = seen {
            if note.is_of(me) {
                return None;
            }
            // This Charon's own syncs are at work exactly while they hold their notes.
            if note.instance == self.instance {
                return Some(file.file_name().is_some_and(|name| known.mine.contains(name)));
            }
            if self.earlier.contains(&note.instance) {
                return Some(false);
            }
        }
        let fresh = match known.sightings.get(file) {
            Some(sighting) if sighting.seen == *seen => now.saturating_duration_since(sighting.since) < STALE_AFTER,
            _ => {
                known.sightings.insert(file.to_path_buf(), Sighting { seen: seen.clone(), since: now });
                true
            }
        };
        Some(fresh)
    }

    /// Where notes are looked for: the roots, every folder above them as they really are
    /// (whatever name they're reached under), and the folders `inside` them. Each folder is
    /// there once, by its real path where it has one: under two names (a link such as macOS'
    /// /var, or Windows' \\?\ form), its notes would count twice.
    fn places(roots: &[&Path], inside: &[PathBuf]) -> BTreeSet<PathBuf> {
        let mut places: BTreeSet<PathBuf> = BTreeSet::new();
        for root in roots {
            match fs::canonicalize(root) {
                Ok(real) => places.extend(real.ancestors().map(Path::to_path_buf)),
                Err(_) => {
                    places.insert(root.to_path_buf());
                }
            }
        }
        places.extend(inside.iter().map(|folder| fs::canonicalize(folder).unwrap_or_else(|_| folder.clone())));
        places
    }

    /// The busy notes in the Charon folder `folder`, with their files. A note that can't be
    /// read is seen by when its file last changed (it's left out when its file can't be
    /// looked at either).
    fn in_folder(folder: &Path) -> Vec<(PathBuf, Seen)> {
        let Ok(items) = fs::read_dir(folder) else { return Vec::new() };
        items
            .flatten()
            .filter(|item| item.file_name().to_str().is_some_and(|name| name.ends_with(BUSY_EXTENSION)))
            .filter_map(|item| {
                let file = item.path();
                match fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
                    Some(note) => Some((file, Seen::Note(note))),
                    None => {
                        let written = fs::metadata(&file).and_then(|metadata| metadata.modified()).ok()?;
                        Some((file, Seen::Unreadable(written)))
                    }
                }
            })
            .collect()
    }

    /// The notes named `name` belong to a sync of this Charon at work now, or not anymore.
    fn hold(&self, name: &OsString, held: bool) {
        let mut known = self.known();
        if held {
            known.mine.insert(name.clone());
        } else {
            known.mine.remove(name);
        }
    }

    fn known(&self) -> MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The notes of one running sync, refreshed until the claim is dropped, which removes them.
pub struct Claim {
    watch: Arc<BusyWatch>,
    /// The name of the claim's notes, the same in every folder.
    name: OsString,
    notes: Arc<Mutex<Notes>>,
    /// Dropped with the claim, which stops the heartbeat.
    _heartbeat: Option<Sender<()>>,
}

/// The notes a claim left, and what they say.
struct Notes {
    files: Vec<PathBuf>,
    note: BusyNote,
}

impl Notes {
    /// Writes the note into every file again, with a new heartbeat. A Charon folder that
    /// went away (its drive was unplugged, its share dropped) is left as it is: making it
    /// again would put it on the computer's own disk. Its note is written again once the
    /// folder is back.
    fn refresh(&mut self) {
        self.note.heartbeat = Utc::now();
        let Ok(bytes) = serde_json::to_vec_pretty(&self.note) else { return };
        for file in &self.files {
            let there = file.parent().is_some_and(|folder| fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir()));
            if !there {
                continue;
            }
            if let Err(e) = atomic::write_into(file, &bytes) {
                log::warn!("couldn't refresh {}: {e}", file.display());
            }
        }
    }

    fn remove(&mut self) {
        for file in self.files.drain(..) {
            let _ = fs::remove_file(&file);
        }
    }
}

impl Claim {
    /// Leaves `note` in each of `roots` where Charon can write, then looks around them (see
    /// `BusyWatch::around`). When another sync is at work there, nothing stays behind and
    /// the error is that sync's note.
    pub fn take(watch: &Arc<BusyWatch>, note: BusyNote, roots: &[&Path], inside: &[PathBuf]) -> std::result::Result<Claim, BusyNote> {
        let name = OsString::from(format!("{}{BUSY_EXTENSION}", uuid::Uuid::new_v4().simple()));
        let bytes = serde_json::to_vec_pretty(&note).unwrap_or_default();
        watch.hold(&name, true);
        let mut files = Vec::new();
        for root in roots {
            let written = Marker::real_folder(root).and_then(|folder| {
                let file = folder.join(&name);
                atomic::write_into(&file, &bytes).map(|()| file)
            });
            match written {
                Ok(file) => files.push(file),
                // A folder Charon can't write to (a card, a read-only share) can't carry a
                // note; its notes from others are still looked for.
                Err(e) => log::info!("no busy note in {} ({e})", root.display()),
            }
        }
        let other = watch.around(roots, inside, &note).into_iter().next();
        let mut claim = Claim { watch: watch.clone(), name, notes: Arc::new(Mutex::new(Notes { files, note })), _heartbeat: None };
        if let Some(other) = other {
            return Err(other);
        }
        claim._heartbeat = claim.beat();
        Ok(claim)
    }

    /// Starts refreshing the notes now and then, on a thread of its own.
    fn beat(&self) -> Option<Sender<()>> {
        let (stop, stopped) = mpsc::channel::<()>();
        let notes = self.notes.clone();
        let spawned = thread::Builder::new().name("charon-busy".into()).spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(HEARTBEAT) {
                notes.lock().unwrap_or_else(|e| e.into_inner()).refresh();
            }
        });
        match spawned {
            Ok(_) => Some(stop),
            Err(e) => {
                // The notes then go stale after a while, and others stop waiting: a sync
                // that runs that long is protected by its checks before every step.
                log::warn!("couldn't start refreshing the busy notes: {e}");
                None
            }
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // Under the lock the heartbeat takes, so a refresh can't put a note back.
        self.notes.lock().unwrap_or_else(|e| e.into_inner()).remove();
        self.watch.hold(&self.name, false);
    }
}

/// This computer's name, as other computers' Charons show it.
fn this_computer() -> String {
    let name = system_name();
    let name = name.trim().trim_end_matches(".local");
    if name.is_empty() {
        "another computer".into()
    } else {
        name.to_string()
    }
}

#[cfg(unix)]
fn system_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: `buffer` is valid for `buffer.len()` bytes for the duration of the call.
    let ok = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    if !ok {
        return String::new();
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

#[cfg(windows)]
fn system_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

#[cfg(not(any(unix, windows)))]
fn system_name() -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watch of its own, as a Charon that just started with the data folder `data` has.
    fn watch(data: &Path) -> Arc<BusyWatch> {
        Arc::new(BusyWatch::open(&data.join("instances.json")))
    }

    fn note(job: &str, instance: &str) -> BusyNote {
        BusyNote::new(&job.to_string(), job, instance)
    }

    /// Leaves `note` in `root` as another Charon would. Returns its file.
    fn leave(root: &Path, note: &BusyNote) -> PathBuf {
        let file = root.join(MARKER_FOLDER).join(format!("{}{BUSY_EXTENSION}", uuid::Uuid::new_v4().simple()));
        atomic::write(&file, &serde_json::to_vec(note).unwrap()).unwrap();
        file
    }

    fn notes_in(root: &Path) -> usize {
        BusyWatch::in_folder(&root.join(MARKER_FOLDER)).len()
    }

    #[test]
    fn a_claim_leaves_its_notes_while_it_lasts() {
        let (data, a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let watch = watch(data.path());
        let claim = Claim::take(&watch, watch.note(&"job".into(), "Job"), &[a.path(), b.path()], &[]).unwrap();
        for root in [a.path(), b.path()] {
            let notes = BusyWatch::in_folder(&root.join(MARKER_FOLDER));
            assert_eq!(notes.len(), 1);
            assert!(matches!(&notes[0].1, Seen::Note(note) if note.job == "job"));
        }
        assert_eq!(watch.around(&[a.path()], &[], &watch.note(&"other".into(), "Other")).len(), 1, "seen by any other sync");
        assert!(watch.around(&[a.path()], &[], &watch.note(&"job".into(), "Job")).is_empty(), "but not by the job's own sync");
        let elsewhere = BusyWatch::open(&data.path().join("other-data.json"));
        assert_eq!(elsewhere.around(&[a.path()], &[], &elsewhere.note(&"job".into(), "Job")).len(), 1, "nor by the same job in another Charon");
        drop(claim);
        assert_eq!(notes_in(a.path()) + notes_in(b.path()), 0);
    }

    #[test]
    fn a_second_sync_in_the_same_folder_waits() {
        let (data, a, b, c) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (mine, theirs) = (watch(&data.path().join("mine")), watch(&data.path().join("theirs")));
        let first = Claim::take(&mine, mine.note(&"one".into(), "One"), &[a.path(), b.path()], &[]).unwrap();
        let refused = Claim::take(&theirs, theirs.note(&"two".into(), "Two"), &[c.path(), b.path()], &[]).err().expect("the folder is busy");
        assert_eq!(refused.job, "one");
        assert_eq!(notes_in(c.path()), 0, "the refused sync leaves nothing behind");
        drop(first);
        assert!(Claim::take(&theirs, theirs.note(&"two".into(), "Two"), &[c.path(), b.path()], &[]).is_ok());
    }

    #[test]
    fn notes_above_and_inside_count() {
        let (data, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let watch = watch(data.path());
        let (outer, inner) = (tmp.path().join("Docs"), tmp.path().join("Docs/Work"));
        fs::create_dir_all(&inner).unwrap();
        let me = watch.note(&"me".into(), "Me");
        let above = leave(&outer, &note("outer", "another"));
        assert_eq!(watch.around(&[&inner], &[], &me).len(), 1, "a sync of a folder above");
        fs::remove_file(above).unwrap();
        leave(&inner, &note("inner", "another"));
        assert!(watch.around(&[&outer], &[], &me).is_empty(), "inside is only looked at where told");
        assert_eq!(watch.around(&[&outer], std::slice::from_ref(&inner), &me).len(), 1);
    }

    #[test]
    fn a_note_counts_until_it_stays_the_same_for_two_minutes_by_this_computers_clock() {
        let (data, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let watch = watch(data.path());
        let me = watch.note(&"me".into(), "Me");
        // Its heartbeat says an hour ago: the other computer's clock may be behind this one's.
        let mut other = note("job", "another");
        other.heartbeat = Utc::now() - chrono::Duration::hours(1);
        let file = leave(tmp.path(), &other);
        let start = Instant::now();
        assert_eq!(watch.around_at(&[tmp.path()], &[], &me, start).len(), 1, "never stale the first time it's seen");
        // Refreshed after a minute and a half: still at work two minutes after that.
        let later = start + Duration::from_secs(90);
        other.heartbeat += chrono::Duration::seconds(20);
        atomic::write(&file, &serde_json::to_vec(&other).unwrap()).unwrap();
        assert_eq!(watch.around_at(&[tmp.path()], &[], &me, later).len(), 1);
        assert_eq!(watch.around_at(&[tmp.path()], &[], &me, later + STALE_AFTER - Duration::from_secs(1)).len(), 1);
        // Unchanged for two minutes: its Charon stopped.
        assert!(watch.around_at(&[tmp.path()], &[], &me, later + STALE_AFTER).is_empty());
        assert!(!file.exists(), "tidied away");
    }

    #[test]
    fn a_note_whose_heartbeat_is_hours_ahead_still_goes_stale() {
        let (data, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let watch = watch(data.path());
        let me = watch.note(&"me".into(), "Me");
        let mut crashed = note("job", "crashed");
        crashed.heartbeat = Utc::now() + chrono::Duration::hours(5);
        let file = leave(tmp.path(), &crashed);
        let start = Instant::now();
        assert_eq!(watch.around_at(&[tmp.path()], &[], &me, start).len(), 1);
        assert!(watch.around_at(&[tmp.path()], &[], &me, start + STALE_AFTER).is_empty());
        assert!(!file.exists());
    }

    #[test]
    fn notes_left_by_this_charon_before_it_stopped_count_for_nothing_at_once() {
        let (data, a) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let before = watch(data.path());
        // A sync cut short: its claim is never dropped (Charon crashed, or quit without
        // waiting for it), so its note stays.
        let claim = Claim::take(&before, before.note(&"job".into(), "Job"), &[a.path()], &[]).unwrap();
        let leftover = BusyWatch::in_folder(&a.path().join(MARKER_FOLDER)).remove(0).0;
        let left = fs::read(&leftover).unwrap();
        drop(claim);
        fs::write(&leftover, &left).unwrap();

        // Charon starts again with the same data folder.
        let after = watch(data.path());
        assert!(after.around(&[a.path()], &[], &after.note(&"other".into(), "Other")).is_empty());
        assert!(!leftover.exists(), "tidied away");
        // Another data folder's Charon doesn't know it for a leftover.
        fs::write(&leftover, &left).unwrap();
        let elsewhere = BusyWatch::open(&data.path().join("other-data.json"));
        assert_eq!(elsewhere.around(&[a.path()], &[], &elsewhere.note(&"other".into(), "Other")).len(), 1);
    }

    #[test]
    fn notes_of_a_charon_that_ran_with_a_copy_of_the_data_folder_still_count() {
        let (data, nas) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Charon syncs the share right now, while its data folder is copied into another
        // folder, and a Charon starts with the copy.
        let (first, copy) = (data.path().join("first"), data.path().join("copy"));
        let old = watch(&first);
        let claim = Claim::take(&old, old.note(&"job".into(), "Job"), &[nas.path()], &[]).unwrap();
        let file = BusyWatch::in_folder(&nas.path().join(MARKER_FOLDER)).remove(0).0;
        fs::create_dir_all(&copy).unwrap();
        fs::copy(first.join("instances.json"), copy.join("instances.json")).unwrap();
        let new = watch(&copy);
        let refused = Claim::take(&new, new.note(&"job".into(), "Job"), &[nas.path()], &[]).err().expect("the folder is busy");
        assert_eq!(refused.instance, old.instance());
        assert!(file.exists(), "the live note stays");
        drop(claim);

        // The data folder was moved to a new computer, to the same place there, while the
        // old computer's Charon still syncs.
        let moved = data.path().join("moved");
        let there = Launch { instance: "old-mac-charon".into(), host: "old-mac".into(), folder: moved.clone() };
        atomic::write(&moved.join("instances.json"), &serde_json::to_vec(&vec![there]).unwrap()).unwrap();
        let mut syncing = note("job", "old-mac-charon");
        syncing.host = "old-mac".into();
        let file = leave(nas.path(), &syncing);
        let new = watch(&moved);
        assert_eq!(new.around(&[nas.path()], &[], &new.note(&"job".into(), "Job")), vec![syncing]);
        assert!(file.exists(), "the live note stays");
    }

    #[test]
    fn this_charons_note_counts_only_while_its_claim_lasts() {
        let (data, a) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let watch = watch(data.path());
        let other = watch.note(&"other".into(), "Other");
        let claim = Claim::take(&watch, watch.note(&"job".into(), "Job"), &[a.path()], &[]).unwrap();
        assert_eq!(watch.around(&[a.path()], &[], &other).len(), 1);
        let file = BusyWatch::in_folder(&a.path().join(MARKER_FOLDER)).remove(0).0;
        let left = fs::read(&file).unwrap();
        drop(claim);
        // A note its claim couldn't take away: nobody is at work anymore.
        fs::write(&file, left).unwrap();
        assert!(watch.around(&[a.path()], &[], &other).is_empty());
        assert!(!file.exists());
    }

    #[test]
    fn a_note_that_cannot_be_read_counts_while_its_file_changes_and_is_never_removed() {
        let (data, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let watch = watch(data.path());
        let me = watch.note(&"me".into(), "Me");
        let file = tmp.path().join(MARKER_FOLDER).join(format!("garbled{BUSY_EXTENSION}"));
        atomic::write(&file, b"{not a note").unwrap();
        let start = Instant::now();
        let found = watch.around_at(&[tmp.path()], &[], &me, start);
        assert_eq!(found.len(), 1);
        assert!(found[0].host.is_empty(), "a sync nobody can name");
        // Its file changes: still at work.
        let changed = SystemTime::now() + Duration::from_secs(30);
        filetime::set_file_mtime(&file, filetime::FileTime::from_system_time(changed)).unwrap();
        assert_eq!(watch.around_at(&[tmp.path()], &[], &me, start + STALE_AFTER).len(), 1);
        // Once its file stays the same, it counts for nothing, and it's left where it is.
        assert!(watch.around_at(&[tmp.path()], &[], &me, start + STALE_AFTER * 2).is_empty());
        assert!(file.exists());
    }

    #[test]
    fn the_heartbeat_never_makes_a_folder_that_went_away() {
        let (data, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let watch = watch(data.path());
        let (laptop, usb) = (tmp.path().join("laptop"), tmp.path().join("Volumes/USB"));
        fs::create_dir_all(&laptop).unwrap();
        fs::create_dir_all(usb.join("Backup")).unwrap();
        let claim = Claim::take(&watch, watch.note(&"job".into(), "Job"), &[&laptop, &usb.join("Backup")], &[]).unwrap();
        // The drive is unplugged during the sync.
        fs::remove_dir_all(&usb).unwrap();
        claim.notes.lock().unwrap().refresh();
        assert!(!usb.exists(), "nothing was made where the drive was");
        assert_eq!(notes_in(&laptop), 1, "the other folder's note is refreshed");
        // The drive comes back: its note is written again.
        fs::create_dir_all(usb.join("Backup/.charon")).unwrap();
        claim.notes.lock().unwrap().refresh();
        assert_eq!(notes_in(&usb.join("Backup")), 1);
        drop(claim);
        assert_eq!(notes_in(&laptop) + notes_in(&usb.join("Backup")), 0);
    }

    #[test]
    fn the_list_of_charons_keeps_the_newest() {
        let data = tempfile::tempdir().unwrap();
        let list = data.path().join("instances.json");
        let first = BusyWatch::open(&list);
        let mut last = BusyWatch::open(&list);
        assert!(last.earlier.contains(first.instance()));
        for _ in 0..EARLIER_KEPT {
            last = BusyWatch::open(&list);
        }
        assert_eq!(last.earlier.len(), EARLIER_KEPT);
        assert!(!last.earlier.contains(first.instance()), "the oldest are forgotten");
    }

    #[test]
    fn the_computer_has_a_name() {
        assert!(!this_computer().is_empty());
        assert!(note("job", "me").from_this_computer());
    }
}
