//! Safe deletes: removed and replaced items go into `.charon-trash/<run time>_<run id>/` at
//! the root of their folder, and are cleared out after a number of days. Clearing out is
//! part of what a review lists ("Also empties 120 items older than 30 days…"): only the
//! dated folders the person saw listed are removed, so runs nobody looked at (full
//! automation) never empty the trash.
//!
//! A removed item is moved there. A replaced file is *copied* there instead and then
//! replaced in place: an app that has the file open (Pages, TextEdit, Preview…) follows the
//! file itself, not its name, so after a move it would go on saving the person's work into
//! the trash. Replaced in place, the app notices the new version instead. (On APFS, where a
//! copy takes no time and no room, a removed file is copied there too and then removed: see
//! `Executor::remove`.)
//!
//! Each dated folder holds a list of what its run put there, with each file's size and time
//! (`.charon`). A file that no longer matches it was changed after it went into the trash:
//! an app may still be saving someone's work into it. A folder holding such a file is never
//! cleared out, and reviews say where the file is.
//!
//! The trash folder must be a real folder: if `.charon-trash` is a link or a file, nothing
//! is moved into it and nothing is cleared out of it.

use crate::run::marker::hide;
use crate::run::names;
use crate::run::rename::{move_to_new_name, rename_new};
use crate::run::{durable, Copier};
use crate::scan::{is_partial, Scanner, TRASH_FOLDER};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const STAMP_FORMAT: &str = "%Y-%m-%d_%H-%M-%S";
/// `2026-10-20_09-00-00` is 19 characters long.
const STAMP_LENGTH: usize = 19;
/// The list of what a run put into its dated folder. No item of a synced folder can have
/// this name: Charon never carries anything named `.charon`.
const LIST: &str = ".charon";
const LIST_VERSION: u32 = 1;
/// Without a list (a run that was interrupted, or a dated folder from before the lists), a
/// file counts as changed in the trash when its time is this much later than its folder's:
/// a network drive's clock may be a little ahead of the computer's.
const UNLISTED_MARGIN_MS: i64 = 60 * 60 * 1000;

/// The files of one dated folder: path inside it (`/`-separated) → size and modification
/// time (milliseconds since 1970) as they landed.
type Listed = BTreeMap<String, (u64, i64)>;

#[derive(Serialize, Deserialize)]
struct StoredList {
    version: u32,
    items: Listed,
}

/// The trash of one run. Every item removed during the run goes into the same dated folder.
pub struct Trash {
    /// The dated folder's name: when the run started, and a short id of the run (two runs
    /// in the same second, from two computers sharing a drive, never share a folder).
    folder: String,
    /// What this run put into the trash of each folder (by the folder's root).
    kept: RefCell<BTreeMap<PathBuf, Listed>>,
    /// The folders whose list of names this run changed, from each root down.
    folders: RefCell<BTreeSet<PathBuf>>,
}

/// A dated trash folder old enough to be cleared out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expired {
    /// The folder's name inside `.charon-trash` (its date and time).
    pub name: String,
    /// Files and links in it, and their size.
    pub items: u64,
    pub bytes: u64,
}

/// What one look at a folder's trash found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrashLook {
    /// Dated folders old enough to be cleared out, with nothing changed in them.
    pub expired: Vec<Expired>,
    /// Files changed after Charon put them into the trash, from the job's folder
    /// (`.charon-trash/2026-10-20_09-00-00_3fa2c1/report.pages`).
    pub revived: Vec<String>,
    /// Bytes in the dated folders looked into (all of them, when the look was thorough).
    pub bytes: u64,
}

/// What one dated folder holds.
#[derive(Debug, Default)]
struct Contents {
    items: u64,
    bytes: u64,
    /// Its files that changed after they went in (paths inside the folder).
    revived: Vec<String>,
    /// Part of it couldn't be read, so nobody can say what's in there.
    unreadable: bool,
}

impl Trash {
    /// A trash for a run starting now.
    pub fn for_run(started: DateTime<Local>) -> Self {
        let id = uuid::Uuid::new_v4().as_u128() as u32 & 0x00ff_ffff;
        Self { folder: format!("{}_{id:06x}", started.format(STAMP_FORMAT)), kept: RefCell::default(), folders: RefCell::default() }
    }

    /// Moves `root/relative` (on-disk spelling) into `root/.charon-trash/<folder>/relative`.
    /// Moving stays on the same drive, so it's instant even for big files.
    pub fn put(&self, root: &Path, relative: &str) -> io::Result<PathBuf> {
        self.put_from(&root.join(relative), root, relative)
    }

    /// Moves the item at `from` (in `root`'s folder) into the trash as `relative`: an item
    /// Charon had moved aside under a temporary name goes in under its own.
    pub fn put_from(&self, from: &Path, root: &Path, relative: &str) -> io::Result<PathBuf> {
        let base = self.dated(root, relative)?;
        // The same path removed twice in one run (replaced, then replaced again): number it.
        for n in 1..=1000 {
            let name = Self::numbered(relative, n);
            match move_to_new_name(from, &base.join(&name)) {
                Ok(()) => return Ok(self.list(root, &base, &name)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name in the trash"))
    }

    /// Copies the file `root/relative` into the trash, leaving it where it is. On drives that
    /// support it (APFS, Btrfs) the copy is a clone: instant, and it takes no room.
    pub fn preserve(&self, root: &Path, relative: &str) -> io::Result<PathBuf> {
        let from = root.join(relative);
        let base = self.dated(root, relative)?;
        // Written under a temporary name first: a half-written copy never takes a name, and
        // nothing already in the trash is ever written over.
        let temp = Copier::partial_path(&base.join(relative));
        Self::copy_file(&from, &temp)?;
        for n in 1..=1000 {
            let name = Self::numbered(relative, n);
            match rename_new(&temp, &base.join(&name)) {
                Ok(()) => return Ok(self.list(root, &base, &name)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    let _ = fs::remove_file(&temp);
                    return Err(e);
                }
            }
        }
        let _ = fs::remove_file(&temp);
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name in the trash"))
    }

    /// Writes down, in each dated folder this run used, what it put there (see the module's
    /// notes). Returns the folders written to, for flushing.
    pub fn write_lists(&self) -> Vec<PathBuf> {
        let mut written = Vec::new();
        for (root, listed) in self.kept.borrow().iter() {
            let Ok(trash) = Self::real_trash(root, false) else { continue };
            let base = trash.join(&self.folder);
            match Self::write_list(&base, listed) {
                Ok(()) => written.push(base),
                Err(e) => log::warn!("couldn't write the list of {}: {e}", base.display()),
            }
        }
        written
    }

    /// The folders this run added names to, from each folder's root down to the items: all
    /// of them must be flushed for what went into the trash to survive a power cut.
    pub fn folders(&self) -> Vec<PathBuf> {
        self.folders.borrow().iter().cloned().collect()
    }

    /// The dated folder of this run in `root`'s trash, with the folders above `relative`
    /// in it.
    fn dated(&self, root: &Path, relative: &str) -> io::Result<PathBuf> {
        let base = Self::real_trash(root, true)?.join(&self.folder);
        if let Some(parent) = base.join(relative).parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(base)
    }

    /// Notes how the item `name` landed in `base` (a copy's time may have been refused by
    /// the drive), and returns where it is.
    fn list(&self, root: &Path, base: &Path, name: &str) -> PathBuf {
        let landed = base.join(name);
        let mut folders = self.folders.borrow_mut();
        for folder in landed.ancestors().skip(1) {
            if !folders.insert(folder.to_path_buf()) || folder == root {
                break;
            }
        }
        drop(folders);
        match fs::symlink_metadata(&landed) {
            Ok(meta) if !meta.is_dir() => {
                self.kept.borrow_mut().entry(root.to_path_buf()).or_default().insert(name.to_string(), (meta.len(), Scanner::modified_ms(&meta)));
            }
            Ok(_) => {}
            Err(e) => log::warn!("couldn't look at {} in the trash: {e}", landed.display()),
        }
        landed
    }

    fn write_list(base: &Path, listed: &Listed) -> io::Result<()> {
        let bytes = serde_json::to_vec(&StoredList { version: LIST_VERSION, items: listed.clone() }).map_err(io::Error::other)?;
        let list = base.join(LIST);
        let temp = Copier::partial_path(&list);
        let written = fs::OpenOptions::new().write(true).create_new(true).open(&temp).and_then(|mut file| {
            file.write_all(&bytes)?;
            durable::sync_file(&file)
        });
        match written.and_then(|()| fs::rename(&temp, &list)) {
            Ok(()) => {
                hide(&list);
                Ok(())
            }
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(e)
            }
        }
    }

    /// The list a dated folder holds, if it has one Charon can read.
    fn read_list(folder: &Path) -> Option<Listed> {
        let bytes = fs::read(folder.join(LIST)).ok()?;
        match serde_json::from_slice::<StoredList>(&bytes) {
            Ok(stored) if stored.version == LIST_VERSION => Some(stored.items),
            Ok(_) => None,
            Err(e) => {
                log::warn!("the list of {} is damaged ({e}); not used", folder.display());
                None
            }
        }
    }

    /// `a/plan.txt` for the first, `a/plan.txt (2)` for the second and so on, shortened to
    /// fit the drive's name limit.
    fn numbered(relative: &str, n: u32) -> String {
        if n == 1 {
            return relative.to_string();
        }
        let (folder, name) = match relative.rfind('/') {
            Some(i) => (&relative[..=i], &relative[i + 1..]),
            None => ("", relative),
        };
        format!("{folder}{}", names::fit(name, &format!(" ({n})")))
    }

    /// The contents, permissions and modification time of a regular file.
    fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
        let meta = fs::symlink_metadata(from)?;
        if !meta.is_file() {
            return Err(io::Error::other("only files are copied into the trash"));
        }
        // A file only in the cloud is refused rather than downloaded (see
        // `scan::Contents::open`); it stays open while it's copied.
        let _reading = crate::scan::Contents::open(from)?;
        // The copy must be on the drive before the original is replaced.
        if let Err(e) = fs::copy(from, to).and_then(|_| Self::flush(to)) {
            let _ = fs::remove_file(to);
            return Err(e);
        }
        let modified = filetime::FileTime::from_last_modification_time(&meta);
        if let Err(e) = filetime::set_file_mtime(to, modified) {
            log::debug!("couldn't keep the time of {} in the trash: {e}", from.display());
        }
        Ok(())
    }

    /// Writes a file's contents to its drive. Windows needs write access for that, which a
    /// read-only copy only gets for the moment of the flush.
    fn flush(path: &Path) -> io::Result<()> {
        match fs::OpenOptions::new().read(true).write(cfg!(windows)).open(path) {
            Ok(file) => durable::sync_file(&file),
            #[cfg(windows)]
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Self::flush_read_only(path),
            Err(e) => Err(e),
        }
    }

    #[cfg(windows)]
    fn flush_read_only(path: &Path) -> io::Result<()> {
        let mut permissions = fs::metadata(path)?.permissions();
        if !permissions.readonly() {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions.clone())?;
        let flushed = fs::OpenOptions::new().write(true).open(path).and_then(|file| durable::sync_file(&file));
        permissions.set_readonly(true);
        let marked = fs::set_permissions(path, permissions);
        flushed.and(marked)
    }

    /// Looks at `root`'s trash: the dated folders older than `days` (none when `days` is 0,
    /// which keeps everything), and the files changed since they went in. Only a `thorough`
    /// look checks the younger folders too (a background look leaves them for a review).
    pub fn look(root: &Path, days: u32, thorough: bool) -> TrashLook {
        let mut look = TrashLook::default();
        let Ok(trash) = Self::real_trash(root, false) else { return look };
        let Ok(items) = fs::read_dir(&trash) else { return look };
        let cutoff = (days > 0).then(|| Local::now() - chrono::Duration::days(days as i64));
        for item in items.flatten().filter(|item| item.file_type().is_ok_and(|t| t.is_dir())) {
            let Some(name) = item.file_name().to_str().map(str::to_owned) else { continue };
            let Some(stamp) = Self::parse_stamp(&name) else { continue };
            let expired = cutoff.is_some_and(|cutoff| stamp < cutoff);
            if !expired && !thorough {
                continue;
            }
            let contents = Self::look_into(&item.path(), stamp, expired);
            look.bytes += contents.bytes;
            look.revived.extend(contents.revived.iter().map(|path| format!("{TRASH_FOLDER}/{name}/{path}")));
            if expired && contents.revived.is_empty() && !contents.unreadable {
                look.expired.push(Expired { name, items: contents.items, bytes: contents.bytes });
            }
        }
        look.expired.sort_by(|a, b| a.name.cmp(&b.name));
        look.revived.sort();
        look
    }

    /// What the dated `folder` (made at `stamp`) holds. With a list and no `walk`, only the
    /// listed files are looked at; otherwise everything in it is.
    fn look_into(folder: &Path, stamp: DateTime<Local>, walk: bool) -> Contents {
        let list = Self::read_list(folder);
        match &list {
            Some(list) if !walk => Self::look_at_listed(folder, list),
            _ => Self::walk(folder, stamp, list.as_ref()),
        }
    }

    fn look_at_listed(folder: &Path, list: &Listed) -> Contents {
        let mut contents = Contents::default();
        for (path, &(size, modified)) in list {
            match fs::symlink_metadata(folder.join(path)) {
                Ok(meta) => {
                    contents.items += 1;
                    contents.bytes += meta.len();
                    if meta.is_dir() || meta.len() != size || Scanner::modified_ms(&meta) != modified {
                        contents.revived.push(path.clone());
                    }
                }
                // Taken out of the trash by the person.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    log::debug!("couldn't look at {path} in {}: {e}", folder.display());
                    contents.unreadable = true;
                }
            }
        }
        contents
    }

    fn walk(folder: &Path, stamp: DateTime<Local>, list: Option<&Listed>) -> Contents {
        let mut contents = Contents::default();
        let made = stamp.timestamp_millis();
        let now = Local::now().timestamp_millis();
        for inner in walkdir::WalkDir::new(folder).min_depth(1).follow_links(false) {
            let inner = match inner {
                Ok(inner) => inner,
                Err(e) => {
                    log::debug!("couldn't look into {}: {e}", folder.display());
                    contents.unreadable = true;
                    continue;
                }
            };
            if inner.file_type().is_dir() {
                continue;
            }
            let Ok(inside) = inner.path().strip_prefix(folder) else { continue };
            let path = inside.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            if path == LIST {
                continue;
            }
            let Ok(meta) = inner.metadata() else {
                contents.unreadable = true;
                continue;
            };
            contents.items += 1;
            contents.bytes += meta.len();
            // Charon's own half-written copies never count as someone's work.
            if is_partial(&inner.file_name().to_string_lossy()) {
                continue;
            }
            let modified = Scanner::modified_ms(&meta);
            let revived = match list {
                Some(list) => match list.get(&path) {
                    Some(&(size, at)) => meta.len() != size || modified != at,
                    None => modified > made,
                },
                None => modified > made + UNLISTED_MARGIN_MS && modified <= now + 60_000,
            };
            if revived {
                contents.revived.push(path);
            }
        }
        contents
    }

    /// Permanently removes the dated folders `names` from `root`'s trash (those a review
    /// listed). Anything else in the trash stays, and so does a folder in which something
    /// changed since the review.
    pub fn sweep(root: &Path, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let Ok(trash) = Self::real_trash(root, false) else { return };
        for name in names {
            let Some(stamp) = Self::parse_stamp(name) else { continue };
            let folder = trash.join(name);
            // Only a real folder: remove_dir_all never follows links inside it either.
            if !fs::symlink_metadata(&folder).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let contents = Self::look_into(&folder, stamp, true);
            if !contents.revived.is_empty() || contents.unreadable {
                log::warn!("kept {}: something in it changed after it went into the trash, or can't be read", folder.display());
                continue;
            }
            if let Err(e) = fs::remove_dir_all(&folder) {
                log::warn!("couldn't clear old trash {}: {e}", folder.display());
            }
        }
        // Leave no empty trash folder behind.
        let _ = fs::remove_dir(&trash);
    }

    /// `root/.charon-trash`, if it is a real folder (created when `create` and missing).
    fn real_trash(root: &Path, create: bool) -> io::Result<PathBuf> {
        let trash = root.join(TRASH_FOLDER);
        match fs::symlink_metadata(&trash) {
            Ok(meta) if meta.is_dir() => Ok(trash),
            Ok(_) => Err(io::Error::other(format!("{TRASH_FOLDER} at the top of the folder isn't a real folder (a link or a file), so Charon won't use it"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
                match fs::create_dir(&trash) {
                    Ok(()) => {
                        hide(&trash);
                        Ok(trash)
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Self::real_trash(root, false),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Whether `name` is one of the dated folders a trash holds.
    pub(crate) fn is_run_folder(name: &str) -> bool {
        Self::stamp_of(name).is_some()
    }

    /// When the dated folder `name` was made, in the computer's time zone.
    fn parse_stamp(name: &str) -> Option<DateTime<Local>> {
        let naive = Self::stamp_of(name)?;
        // A time that doesn't exist where the computer is now (it moved to another time
        // zone) is read as UTC: a few hours off, but never "never".
        Local.from_local_datetime(&naive).earliest().or_else(|| Some(naive.and_utc().with_timezone(&Local)))
    }

    /// `2026-10-20_09-00-00`, alone (earlier versions of Charon) or with a run id after it
    /// (`2026-10-20_09-00-00_3fa2c1`).
    fn stamp_of(name: &str) -> Option<NaiveDateTime> {
        let (stamp, id) = name.split_at_checked(STAMP_LENGTH)?;
        let id_fits = id.is_empty() || id.strip_prefix('_').is_some_and(|id| id.len() == 6 && id.bytes().all(|b| b.is_ascii_hexdigit()));
        if !id_fits {
            return None;
        }
        NaiveDateTime::parse_from_str(stamp, STAMP_FORMAT).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days_ago(days: i64) -> DateTime<Local> {
        Local::now() - chrono::Duration::days(days)
    }

    fn set_time(path: &Path, at: DateTime<Local>) {
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(at.timestamp(), 0)).unwrap();
    }

    #[test]
    fn moves_items_and_numbers_repeats() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = Trash::for_run(Local::now());
        fs::create_dir_all(tmp.path().join("a")).unwrap();
        fs::write(tmp.path().join("a/x.txt"), "1").unwrap();
        let first = trash.put(tmp.path(), "a/x.txt").unwrap();
        fs::write(tmp.path().join("a/x.txt"), "2").unwrap();
        let second = trash.put(tmp.path(), "a/x.txt").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(first).unwrap(), "1");
        assert_eq!(fs::read_to_string(second).unwrap(), "2");
    }

    #[test]
    fn preserving_copies_and_leaves_the_file_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = Trash::for_run(Local::now());
        fs::write(tmp.path().join("doc.txt"), "old version").unwrap();
        let kept = trash.preserve(tmp.path(), "doc.txt").unwrap();
        assert_eq!(fs::read_to_string(&kept).unwrap(), "old version");
        assert_eq!(fs::read_to_string(tmp.path().join("doc.txt")).unwrap(), "old version");
        let again = trash.preserve(tmp.path(), "doc.txt").unwrap();
        assert_ne!(kept, again);
        let leftovers: Vec<_> = fs::read_dir(kept.parent().unwrap()).unwrap().flatten().filter(|i| is_partial(&i.file_name().to_string_lossy())).collect();
        assert!(leftovers.is_empty(), "no temporary copy is left behind");
    }

    #[test]
    fn two_runs_in_the_same_second_use_their_own_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let started = Local::now();
        let (one, two) = (Trash::for_run(started), Trash::for_run(started));
        fs::write(tmp.path().join("a.txt"), "1").unwrap();
        fs::write(tmp.path().join("b.txt"), "2").unwrap();
        let (a, b) = (one.put(tmp.path(), "a.txt").unwrap(), two.put(tmp.path(), "b.txt").unwrap());
        assert_ne!(a.parent(), b.parent());
        for folder in [a.parent().unwrap(), b.parent().unwrap()] {
            assert!(Trash::is_run_folder(&folder.file_name().unwrap().to_string_lossy()));
        }
    }

    #[test]
    fn dated_folders_are_recognised_with_and_without_a_run_id() {
        assert!(Trash::is_run_folder("2026-10-20_09-00-00"));
        assert!(Trash::is_run_folder("2026-10-20_09-00-00_3fa2c1"));
        assert!(!Trash::is_run_folder("2026-10-20_09-00-00_3fa2c"));
        assert!(!Trash::is_run_folder("2026-10-20_09-00-00 copy"));
        assert!(!Trash::is_run_folder("2026-10-20"));
        assert!(!Trash::is_run_folder("Photos"));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_trash_folder_is_never_used() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, elsewhere) = (tmp.path().join("root"), tmp.path().join("elsewhere"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(elsewhere.join("2000-01-01_00-00-00")).unwrap();
        fs::write(elsewhere.join("2000-01-01_00-00-00/keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(TRASH_FOLDER)).unwrap();
        fs::write(root.join("a.txt"), "x").unwrap();
        assert!(Trash::for_run(Local::now()).put(&root, "a.txt").is_err());
        assert!(Trash::look(&root, 1, true).expired.is_empty());
        Trash::sweep(&root, &["2000-01-01_00-00-00".to_string()]);
        assert!(elsewhere.join("2000-01-01_00-00-00/keep.txt").exists(), "sweeping never follows the link");
    }

    #[test]
    fn lists_and_sweeps_only_old_dated_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path().join(TRASH_FOLDER);
        fs::create_dir_all(trash.join("2000-01-01_00-00-00/a")).unwrap();
        fs::write(trash.join("2000-01-01_00-00-00/a/x.txt"), "12345").unwrap();
        set_time(&trash.join("2000-01-01_00-00-00/a/x.txt"), Local.with_ymd_and_hms(1999, 12, 31, 0, 0, 0).unwrap());
        fs::create_dir_all(trash.join("not-a-stamp")).unwrap();
        let fresh = Local::now().format(STAMP_FORMAT).to_string();
        fs::create_dir_all(trash.join(&fresh)).unwrap();
        let look = Trash::look(tmp.path(), 30, false);
        assert_eq!(look.expired, vec![Expired { name: "2000-01-01_00-00-00".into(), items: 1, bytes: 5 }]);
        assert!(look.revived.is_empty());
        assert!(Trash::look(tmp.path(), 0, false).expired.is_empty(), "0 keeps everything");
        Trash::sweep(tmp.path(), &[look.expired[0].name.clone(), "not-a-stamp".into()]);
        assert!(!trash.join("2000-01-01_00-00-00").exists());
        assert!(trash.join("not-a-stamp").exists());
        assert!(trash.join(fresh).exists());
    }

    /// An app that had the file open goes on saving into the trash: that folder is never
    /// cleared out, and the look says where the file is.
    #[test]
    fn a_file_saved_into_the_trash_is_never_cleared_out() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = Trash::for_run(days_ago(40));
        fs::write(tmp.path().join("report.pages"), "the old report").unwrap();
        fs::write(tmp.path().join("other.txt"), "x").unwrap();
        set_time(&tmp.path().join("report.pages"), days_ago(41));
        let kept = trash.put(tmp.path(), "report.pages").unwrap();
        trash.put(tmp.path(), "other.txt").unwrap();
        assert_eq!(trash.write_lists().len(), 1);
        let folder = kept.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();

        let look = Trash::look(tmp.path(), 30, false);
        assert_eq!(look.expired.iter().map(|e| (e.name.as_str(), e.items)).collect::<Vec<_>>(), vec![(folder.as_str(), 2)], "the list itself isn't an item");

        // Days later, the app saves the person's work into the file it followed.
        fs::write(&kept, "the report, with this week's work").unwrap();
        set_time(&kept, days_ago(2));
        let look = Trash::look(tmp.path(), 30, false);
        assert!(look.expired.is_empty());
        assert_eq!(look.revived, vec![format!("{TRASH_FOLDER}/{folder}/report.pages")]);
        Trash::sweep(tmp.path(), std::slice::from_ref(&folder));
        assert_eq!(fs::read_to_string(&kept).unwrap(), "the report, with this week's work");
    }

    #[test]
    fn a_younger_folder_is_checked_by_its_list_when_the_look_is_thorough() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = Trash::for_run(days_ago(1));
        fs::write(tmp.path().join("notes.txt"), "old").unwrap();
        set_time(&tmp.path().join("notes.txt"), days_ago(3));
        let kept = trash.preserve(tmp.path(), "notes.txt").unwrap();
        trash.write_lists();
        assert_eq!(Trash::look(tmp.path(), 30, true), TrashLook { bytes: 3, ..Default::default() });
        fs::write(&kept, "edited in the trash").unwrap();
        assert!(Trash::look(tmp.path(), 30, false).revived.is_empty(), "a quick look leaves young folders alone");
        assert_eq!(Trash::look(tmp.path(), 30, true).revived.len(), 1);
    }

    /// A folder without a list (an interrupted run, an earlier version) is judged by the
    /// times of its files: only one clearly newer than the folder counts as changed there.
    #[test]
    fn without_a_list_only_a_clearly_newer_file_counts_as_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let name = days_ago(40).format(STAMP_FORMAT).to_string();
        let folder = tmp.path().join(TRASH_FOLDER).join(&name);
        fs::create_dir_all(&folder).unwrap();
        for (file, age) in [("older.txt", 50), ("future.txt", -400)] {
            fs::write(folder.join(file), "x").unwrap();
            set_time(&folder.join(file), days_ago(age));
        }
        assert_eq!(Trash::look(tmp.path(), 30, false).expired.len(), 1);
        fs::write(folder.join("saved-later.txt"), "x").unwrap();
        set_time(&folder.join("saved-later.txt"), days_ago(10));
        let look = Trash::look(tmp.path(), 30, false);
        assert!(look.expired.is_empty());
        assert_eq!(look.revived, vec![format!("{TRASH_FOLDER}/{name}/saved-later.txt")]);
    }
}
