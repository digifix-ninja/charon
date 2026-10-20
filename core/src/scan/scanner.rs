//! Walks a folder and records every item that isn't excluded, and every place it couldn't
//! read. Nothing goes unnoticed: a folder that can't be read is never mistaken for an
//! empty one (that would look like "everything in it was deleted").

use crate::error::{Error, Result};
use crate::scan::volume::windows_name_problem;
use crate::scan::{
    is_clutter, is_partial, is_system_folder, Entry, ExcludeFilter, Keys, RootId, Skipped, Snapshot, Volume, MARKER_FOLDER, TRASH_FOLDER,
};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

/// How often (in items) a scan reports how far it got.
const COUNT_EVERY: u64 = 500;

/// Reads folders into snapshots. Symbolic links are recorded as links, never followed.
pub struct Scanner<'a> {
    filter: &'a ExcludeFilter,
    cancel: Option<&'a AtomicBool>,
    counter: Option<&'a (dyn Fn(u64) + Sync)>,
}

impl<'a> Scanner<'a> {
    pub fn new(filter: &'a ExcludeFilter) -> Self {
        Self { filter, cancel: None, counter: None }
    }

    /// Stops the scan early when `cancel` becomes true.
    pub fn cancellable(mut self, cancel: &'a AtomicBool) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Calls `counter` with the number of items seen so far, now and then.
    pub fn counting(mut self, counter: &'a (dyn Fn(u64) + Sync)) -> Self {
        self.counter = Some(counter);
        self
    }

    /// Every item under `root`, on the drive `volume`. Fails if `root` isn't reachable or
    /// can't be read. Places inside it that can't be read are recorded as frozen, so
    /// nothing there is touched.
    pub fn scan(&self, root: &Path, volume: Volume) -> Result<Snapshot> {
        Self::check_root(root)?;
        let root_id = RootId::of(root).map_err(|e| Error::Unreadable { path: root.to_path_buf(), reason: Self::io_reason(&e) })?;
        let mut snapshot = Snapshot::new();
        snapshot.facts_mut().root = root_id;
        let change_times = volume.keeps_change_times();
        snapshot.facts_mut().volume = Some(volume);
        let mut walker = WalkDir::new(root).follow_links(false).min_depth(1).into_iter();
        let mut seen: u64 = 0;

        while let Some(item) = walker.next() {
            if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(Error::Cancelled);
            }
            seen += 1;
            if seen.is_multiple_of(COUNT_EVERY) {
                if let Some(counter) = self.counter {
                    counter(seen);
                }
            }
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    Self::note_unreadable(root, &error, &mut snapshot)?;
                    continue;
                }
            };
            let file_type = item.file_type();
            let Some(disk) = Self::relative(root, item.path()) else {
                // A name that isn't valid Unicode can't be named safely on the other side.
                // It stays where it is, and so does its folder.
                if file_type.is_dir() {
                    walker.skip_current_dir();
                }
                Self::leave_out(root, item.path(), "its name isn't valid Unicode, so Charon can't carry it", &mut snapshot);
                continue;
            };
            let key = Keys::normalize(&disk);
            let name = Keys::name(&key);

            if item.depth() == 1 && is_system_folder(name) {
                if file_type.is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            if self.filter.excludes(&key) {
                if file_type.is_dir() {
                    walker.skip_current_dir();
                }
                if ExcludeFilter::is_internal(&key, name) {
                    if is_partial(name) && file_type.is_file() {
                        if let Ok(meta) = item.metadata() {
                            snapshot.facts_mut().partials.push((disk.clone(), Entry::file(meta.len(), Self::modified_ms(&meta))));
                        }
                        snapshot.occupy_folders_of(&key);
                    } else if (name == MARKER_FOLDER || name == TRASH_FOLDER) && !Self::looks_like_charons(item.path(), name, file_type.is_dir()) {
                        // The person's own item, only named like one of Charon's folders:
                        // left alone, and said so.
                        snapshot.facts_mut().skipped.push(Skipped {
                            path: disk.clone(),
                            reason: "named like one of Charon's own folders, so Charon leaves it alone".into(),
                        });
                        snapshot.occupy_folders_of(&key);
                    } else if Keys::parent(&key).is_some() {
                        // Another job's trash or marker inside this folder: never removed.
                        snapshot.occupy_folders_of(&key);
                    }
                } else if !is_clutter(name) {
                    snapshot.occupy_folders_of(&key);
                }
                continue;
            }
            if cfg!(windows) {
                if let Some(reason) = windows_name_problem(Keys::name(&disk)) {
                    if file_type.is_dir() {
                        walker.skip_current_dir();
                    }
                    snapshot.facts_mut().skipped.push(Skipped { path: disk, reason: format!("{reason}, which Windows can't handle safely") });
                    snapshot.occupy_folders_of(&key);
                    continue;
                }
            }

            if file_type.is_dir() {
                if let Some(reason) = Self::other_drive(&item, root_id).or_else(|| Self::only_in_the_cloud(item.path())) {
                    // A drive or share mounted inside the folder (if it's ever unmounted, its
                    // folder looks empty), or a folder whose contents aren't downloaded.
                    walker.skip_current_dir();
                    snapshot.freeze(&key, reason);
                    continue;
                }
                Self::add(&mut snapshot, key, disk, Entry::dir());
            } else if file_type.is_file() {
                match Self::file_metadata(&item) {
                    Ok(meta) => match Self::placeholder(&meta) {
                        Some(reason) => snapshot.freeze(&key, reason),
                        None => {
                            let changed = if change_times { Self::change_time(&meta) } else { 0 };
                            Self::add(&mut snapshot, key, disk, Entry::file(meta.len(), Self::modified_ms(&meta)).changed_at(changed))
                        }
                    },
                    Err(e) => snapshot.freeze(&key, Self::io_reason(&e)),
                }
            } else if file_type.is_symlink() {
                Self::add_link(&mut snapshot, item.path(), key, disk);
            } else {
                snapshot.facts_mut().skipped.push(Skipped { path: disk, reason: "isn't a file or a folder (a socket, pipe or device)".into() });
                snapshot.occupy_folders_of(&key);
            }
        }
        if let Some(counter) = self.counter {
            counter(seen);
        }
        Ok(snapshot)
    }

    /// A file's size and time. On Windows they are read from the file itself, the way the
    /// sync checks them later (the folder listing can lag behind a file being written).
    fn file_metadata(item: &walkdir::DirEntry) -> std::io::Result<fs::Metadata> {
        if cfg!(windows) {
            fs::symlink_metadata(item.path())
        } else {
            item.metadata().map_err(|e| e.into_io_error().unwrap_or_else(|| std::io::Error::other("couldn't read it")))
        }
    }

    /// Why the file is left alone if it's only a placeholder for a file in the cloud
    /// (iCloud Drive, OneDrive, Dropbox): reading it would download it, and its size and
    /// time say nothing about its contents here.
    fn placeholder(meta: &fs::Metadata) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt;
            const SF_DATALESS: u32 = 0x4000_0000;
            if meta.st_flags() & SF_DATALESS != 0 {
                return Some(Self::CLOUD.into());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN};
            if meta.file_attributes() & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_OFFLINE) != 0 {
                return Some(Self::CLOUD.into());
            }
        }
        let _ = meta;
        None
    }

    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    const CLOUD: &'static str = "it's only in the cloud (not downloaded to this computer), so Charon leaves it alone";

    /// A folder whose contents are still in the cloud.
    fn only_in_the_cloud(path: &Path) -> Option<String> {
        if cfg!(any(target_os = "macos", windows)) {
            fs::symlink_metadata(path).ok().and_then(|meta| Self::placeholder(&meta))
        } else {
            None
        }
    }

    /// Why the folder `item` must not be entered: it's on another drive than the scanned
    /// folder, or Charon can't tell.
    /// Whether an item named `.charon` or `.charon-trash` really is one of Charon's: a
    /// folder holding only markers, or only dated trash folders (or nothing).
    fn looks_like_charons(path: &Path, name: &str, is_dir: bool) -> bool {
        if !is_dir {
            return false;
        }
        let Ok(items) = fs::read_dir(path) else { return false };
        items.into_iter().all(|item| {
            let Ok(item) = item else { return false };
            let (Ok(kind), Some(inner)) = (item.file_type(), item.file_name().to_str().map(str::to_owned)) else { return false };
            if name == MARKER_FOLDER {
                kind.is_file() && (inner.ends_with(".json") || inner.ends_with(".tmp"))
            } else {
                kind.is_dir() && crate::run::Trash::is_run_folder(&inner)
            }
        })
    }

    #[cfg(unix)]
    fn other_drive(item: &walkdir::DirEntry, root: Option<RootId>) -> Option<String> {
        let root = root?;
        match item.metadata() {
            Ok(meta) if RootId::device_of(&meta) == root.device() => None,
            Ok(_) => Some("another drive or share is mounted here; Charon doesn't cross into it".into()),
            Err(e) => Some(Self::reason(&e)),
        }
    }

    /// Windows mounts drives inside folders as junctions, which are left out as links.
    #[cfg(not(unix))]
    fn other_drive(_item: &walkdir::DirEntry, _root: Option<RootId>) -> Option<String> {
        None
    }

    /// The folder must be there and readable.
    fn check_root(root: &Path) -> Result<()> {
        match fs::metadata(root) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(Error::Offline(root.to_path_buf())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::Offline(root.to_path_buf())),
            Err(e) => return Err(Error::Unreadable { path: root.to_path_buf(), reason: Self::io_reason(&e) }),
        }
        fs::read_dir(root)
            .map(|_| ())
            .map_err(|e| Error::Unreadable { path: root.to_path_buf(), reason: Self::io_reason(&e) })
    }

    /// Records an item, or freezes its name when another item already has the same key
    /// (two spellings of one name in one folder, possible on some drives).
    fn add(snapshot: &mut Snapshot, key: String, disk: String, entry: Entry) {
        if snapshot.contains(&key) {
            snapshot.freeze(&key, "two items here have the same name written in different ways (Unicode forms), so Charon leaves both alone");
            snapshot.facts_mut().clashes.insert(key);
            return;
        }
        if disk != key {
            snapshot.facts_mut().spellings.insert(key.clone(), disk);
        }
        snapshot.insert(key, entry);
    }

    /// Links are carried as links on macOS and Linux. Windows links and junctions are left
    /// alone (creating them needs special rights there).
    #[cfg(unix)]
    fn add_link(snapshot: &mut Snapshot, path: &Path, key: String, disk: String) {
        use std::os::unix::ffi::OsStrExt;
        match (fs::read_link(path), fs::symlink_metadata(path)) {
            (Ok(target), Ok(meta)) => {
                Self::add(snapshot, key, disk, Entry::link(target.as_os_str().as_bytes(), Self::modified_ms(&meta)))
            }
            (Err(e), _) | (_, Err(e)) => snapshot.freeze(&key, Self::io_reason(&e)),
        }
    }

    #[cfg(not(unix))]
    fn add_link(snapshot: &mut Snapshot, _path: &Path, key: String, disk: String) {
        snapshot.facts_mut().skipped.push(Skipped { path: disk, reason: "is a link or junction; Charon leaves those alone on Windows".into() });
        snapshot.occupy_folders_of(&key);
    }

    /// An item that can't be named: noted, and its folder is kept.
    fn leave_out(root: &Path, path: &Path, reason: &str, snapshot: &mut Snapshot) {
        let shown = path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned();
        snapshot.facts_mut().skipped.push(Skipped { path: shown, reason: reason.into() });
        let parent = path.parent().and_then(|p| Self::relative(root, p)).map(|p| Keys::normalize(&p));
        if let Some(parent) = parent {
            snapshot.occupy(&parent);
        }
    }

    /// A place the walk couldn't read. Inside the folder it's frozen; the folder itself
    /// failing (or an error without a place) fails the whole scan.
    fn note_unreadable(root: &Path, error: &walkdir::Error, snapshot: &mut Snapshot) -> Result<()> {
        let reason = Self::reason(error);
        let place = error.path().filter(|path| *path != root).and_then(|path| Self::relative(root, path));
        match place {
            Some(disk) => {
                snapshot.freeze(&Keys::normalize(&disk), reason);
                Ok(())
            }
            None => Err(Error::Unreadable { path: root.to_path_buf(), reason }),
        }
    }

    /// `path` relative to `root`, with `/` separators on every system, exactly as spelled
    /// on disk. `None` if a part of the name isn't valid Unicode.
    pub(crate) fn relative(root: &Path, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(root).ok()?;
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        let parts = parts?;
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    /// A file's change time in nanoseconds since 1970 (macOS and Linux), 0 where unknown.
    pub(crate) fn change_time(meta: &fs::Metadata) -> i64 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            meta.ctime().saturating_mul(1_000_000_000).saturating_add(meta.ctime_nsec())
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            0
        }
    }

    pub(crate) fn modified_ms(meta: &fs::Metadata) -> i64 {
        meta.modified().map(Self::millis).unwrap_or(0)
    }

    /// Milliseconds since 1970 (negative before).
    pub(crate) fn millis(time: SystemTime) -> i64 {
        match time.duration_since(UNIX_EPOCH) {
            Ok(after) => after.as_millis() as i64,
            Err(before) => -(before.duration().as_millis() as i64),
        }
    }

    fn reason(error: &walkdir::Error) -> String {
        error.io_error().map(Self::io_reason).unwrap_or_else(|| error.to_string())
    }

    /// "permission denied" rather than "Permission denied (os error 13)". On macOS, a
    /// refusal by the privacy settings says where to allow it.
    pub(crate) fn io_reason(error: &std::io::Error) -> String {
        #[cfg(target_os = "macos")]
        match error.raw_os_error() {
            Some(libc::EPERM) => return PRIVACY_REFUSAL.into(),
            // A cloud file or folder Charon declined to download (see `Volume`).
            Some(libc::EDEADLK) => return Self::CLOUD.into(),
            _ => {}
        }
        let text = error.to_string();
        let text = text.split(" (os error").next().unwrap_or(&text).to_string();
        let mut chars = text.chars();
        match chars.next() {
            Some(first) => first.to_lowercase().chain(chars).collect(),
            None => text,
        }
    }
}

/// The reason given for places macOS's privacy settings keep Charon out of.
pub const PRIVACY_REFUSAL: &str =
    "macOS didn't let Charon read this. Allow Charon in System Settings › Privacy & Security › Full Disk Access";

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn scan(root: &Path, patterns: &[&str]) -> Snapshot {
        let filter = ExcludeFilter::new(&patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>(), false).unwrap();
        Scanner::new(&filter).scan(root, Volume::of(root)).unwrap()
    }

    #[test]
    fn names_windows_cannot_hold() {
        assert!(windows_name_problem("notes.").is_some());
        assert!(windows_name_problem("notes ").is_some());
        assert!(windows_name_problem("con").is_some());
        assert!(windows_name_problem("NUL.txt").is_some());
        assert!(windows_name_problem("com1.log").is_some());
        assert!(windows_name_problem("console.txt").is_none());
        assert!(windows_name_problem("com.txt").is_none());
        assert!(windows_name_problem(".env").is_none());
    }

    #[test]
    fn the_persons_own_items_named_like_charons_folders_are_noted_not_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        // Charon's own: the marker folder at the top, and another job's inside.
        write(&tmp.path().join(".charon/job1.json"), "{}");
        write(&tmp.path().join("inner/.charon/job2.json"), "{}");
        write(&tmp.path().join("inner/.charon-trash/2026-01-02_03-04-05/old.txt"), "x");
        // The person's own.
        write(&tmp.path().join("projects/app/.charon/settings.toml"), "x");
        write(&tmp.path().join("notes/.charon-trash"), "a file");
        let s = scan(tmp.path(), &[]);
        let mut noted: Vec<&str> = s.skipped().iter().map(|skipped| skipped.path.as_str()).collect();
        noted.sort();
        assert_eq!(noted, vec!["notes/.charon-trash", "projects/app/.charon"]);
        assert!(s.is_occupied("projects/app") && s.is_occupied("inner"));
        assert!(!s.contains("projects/app/.charon/settings.toml"));
    }

    #[test]
    fn a_drives_own_folders_at_the_top_are_never_looked_at() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("System Volume Information/x.dat"), "x");
        write(&tmp.path().join("photos/lost+found/kept.txt"), "x");
        let s = scan(tmp.path(), &[]);
        assert!(s.get("System Volume Information").is_none());
        assert!(s.get("photos/lost+found/kept.txt").is_some(), "only at the top");
    }

    #[test]
    fn counts_as_it_goes() {
        let tmp = tempfile::tempdir().unwrap();
        for n in 0..1200 {
            write(&tmp.path().join(format!("f{n}")), "x");
        }
        let seen = std::sync::Mutex::new(Vec::new());
        let counter = |n: u64| seen.lock().unwrap().push(n);
        let filter = ExcludeFilter::none();
        Scanner::new(&filter).counting(&counter).scan(tmp.path(), Volume::of(tmp.path())).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![500, 1000, 1200]);
    }

    #[test]
    fn another_jobs_trash_inside_keeps_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("inner/.charon-trash/2026-01-01_00-00-00/a.txt"), "x");
        write(&tmp.path().join(".charon-trash/2026-01-01_00-00-00/b.txt"), "x");
        let s = scan(tmp.path(), &[]);
        assert!(s.is_occupied("inner"));
        assert!(s.get("inner/.charon-trash").is_none());
        assert!(s.get(".charon-trash").is_none());
    }

    #[test]
    fn records_files_and_folders() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("a/b.txt"), "hi");
        let s = scan(tmp.path(), &[]);
        assert!(s.get("a").unwrap().is_dir());
        assert_eq!(s.get("a/b.txt").unwrap().size, 2);
    }

    #[test]
    fn a_missing_folder_is_offline() {
        let tmp = tempfile::tempdir().unwrap();
        let filter = ExcludeFilter::none();
        let gone = tmp.path().join("gone");
        assert!(matches!(Scanner::new(&filter).scan(&gone, Volume::of(&gone)), Err(Error::Offline(_))));
    }

    #[test]
    fn excluded_items_keep_their_folders_but_clutter_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("work/notes.tmp"), "x");
        write(&tmp.path().join("work/deep/more.tmp"), "x");
        write(&tmp.path().join("photos/.DS_Store"), "x");
        let s = scan(tmp.path(), &["*.tmp", ".DS_Store"]);
        assert!(s.get("work/notes.tmp").is_none());
        assert!(s.is_occupied("work") && s.is_occupied("work/deep"));
        assert!(!s.is_occupied("photos"), ".DS_Store alone never keeps a folder");
    }

    #[test]
    fn leftover_partial_files_are_noticed() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("a/big.mov.charon-partial"), "half");
        write(&tmp.path().join("a/.charon-00112233445566ff.part"), "half");
        let s = scan(tmp.path(), &[]);
        assert_eq!(s.partials().len(), 2);
        assert!(s.get("a/big.mov.charon-partial").is_none());
        assert!(s.is_occupied("a"), "a folder with a leftover isn't empty");
    }

    #[test]
    fn names_are_compared_in_one_unicode_form() {
        let tmp = tempfile::tempdir().unwrap();
        let nfd = "cafe\u{301}.txt";
        write(&tmp.path().join(nfd), "x");
        let s = scan(tmp.path(), &[]);
        // Some file systems turn the name into NFC on their own; either way the key is NFC.
        assert!(s.get("caf\u{e9}.txt").is_some());
        let on_disk = fs::read_dir(tmp.path()).unwrap().next().unwrap().unwrap().file_name();
        assert_eq!(s.spelling("caf\u{e9}.txt"), on_disk.to_str().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn two_spellings_of_one_name_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("cafe\u{301}.txt"), "nfd");
        write(&tmp.path().join("caf\u{e9}.txt"), "nfc");
        let s = scan(tmp.path(), &[]);
        assert!(s.frozen("caf\u{e9}.txt").is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn case_twins_are_left_alone_once_keys_ignore_case() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("Readme.md"), "a");
        write(&tmp.path().join("README.md"), "b");
        let mut s = scan(tmp.path(), &[]);
        assert_eq!(s.len(), 2, "both names exist on a drive that tells case apart");
        s.fold();
        assert!(s.frozen("readme.md").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_recorded_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("real/inside.txt"), "x");
        std::os::unix::fs::symlink("real", tmp.path().join("shortcut")).unwrap();
        let s = scan(tmp.path(), &[]);
        assert!(s.get("shortcut").unwrap().is_link());
        assert!(s.get("shortcut/inside.txt").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_frozen_not_empty() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("locked/secret.txt"), "x");
        write(&tmp.path().join("open.txt"), "x");
        fs::set_permissions(tmp.path().join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(tmp.path().join("locked")).is_ok(); // root ignores permissions
        let s = scan(tmp.path(), &[]);
        fs::set_permissions(tmp.path().join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return;
        }
        assert!(s.frozen("locked").is_some());
        assert!(s.frozen("locked/secret.txt").is_some());
        assert!(s.get("open.txt").is_some());
    }
}
