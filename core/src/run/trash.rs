//! Safe deletes: removed and replaced items go into `.charon-trash/<run time>/` at the root
//! of their folder, and are cleared out after a number of days. Clearing out is part of
//! what a review lists ("Also empties 120 items older than 30 days…"): only the dated
//! folders the person saw listed are removed.
//!
//! A removed item is moved there. A replaced file is *copied* there instead and then
//! replaced in place: an app that has the file open (Pages, TextEdit, Preview…) follows the
//! file itself, not its name, so after a move it would go on saving the person's work into
//! the trash. Replaced in place, the app notices the new version instead.
//!
//! The trash folder must be a real folder: if `.charon-trash` is a link or a file, nothing
//! is moved into it and nothing is cleared out of it.

use crate::run::marker::hide;
use crate::run::names;
use crate::run::rename::rename_new;
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

/// A dated trash folder old enough to be cleared out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expired {
    /// The folder's name inside `.charon-trash` (its date and time).
    pub name: String,
    /// Files and links in it, and their size.
    pub items: u64,
    pub bytes: u64,
}

impl Trash {
    /// A trash for a run starting now.
    pub fn for_run(started: DateTime<Local>) -> Self {
        Self { stamp: started.format(STAMP_FORMAT).to_string() }
    }

    /// Moves `root/relative` (on-disk spelling) into `root/.charon-trash/<stamp>/relative`.
    /// Moving stays on the same drive, so it's instant even for big files.
    pub fn put(&self, root: &Path, relative: &str) -> io::Result<PathBuf> {
        let from = root.join(relative);
        let base = Self::real_trash(root, true)?.join(&self.stamp);
        let first = base.join(relative);
        if let Some(parent) = first.parent() {
            fs::create_dir_all(parent)?;
        }
        // The same path removed twice in one run (replaced, then replaced again): number it.
        let mut n = 1;
        loop {
            let to = if n == 1 { first.clone() } else { base.join(Self::numbered(relative, n)) };
            match rename_new(&from, &to) {
                Ok(()) => return Ok(to),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// Copies the file `root/relative` into the trash, leaving it where it is. On macOS
    /// drives that support it (APFS) the copy is a clone: instant, and it takes no space.
    pub fn preserve(&self, root: &Path, relative: &str) -> io::Result<PathBuf> {
        let from = root.join(relative);
        let base = Self::real_trash(root, true)?.join(&self.stamp);
        let first = base.join(relative);
        if let Some(parent) = first.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut n = 1;
        loop {
            let to = if n == 1 { first.clone() } else { base.join(Self::numbered(relative, n)) };
            match fs::symlink_metadata(&to) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    Self::copy_file(&from, &to)?;
                    return Ok(to);
                }
                Ok(_) if n < 1000 => n += 1,
                Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name in the trash")),
                Err(e) => return Err(e),
            }
        }
    }

    /// `a/plan.txt` → `a/plan.txt (2)`, shortened to fit the drive's name limit.
    fn numbered(relative: &str, n: u32) -> String {
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

    /// The dated folders in `root`'s trash older than `days` (none when `days` is 0, which
    /// keeps everything), with what they hold.
    pub fn expired(root: &Path, days: u32) -> Vec<Expired> {
        if days == 0 {
            return Vec::new();
        }
        let Ok(trash) = Self::real_trash(root, false) else { return Vec::new() };
        let Ok(items) = fs::read_dir(&trash) else { return Vec::new() };
        let cutoff = Local::now() - chrono::Duration::days(days as i64);
        let mut expired: Vec<Expired> = items
            .flatten()
            .filter(|item| item.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|item| {
                let name = item.file_name().to_str()?.to_string();
                (Self::parse_stamp(&name)? < cutoff).then_some((name, item.path()))
            })
            .map(|(name, path)| {
                let (mut items, mut bytes) = (0, 0);
                for inner in walkdir::WalkDir::new(&path).follow_links(false).into_iter().flatten() {
                    if !inner.file_type().is_dir() {
                        items += 1;
                        bytes += inner.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
                Expired { name, items, bytes }
            })
            .collect();
        expired.sort_by(|a, b| a.name.cmp(&b.name));
        expired
    }

    /// Writes a file's contents to its drive. Windows needs write access for that; a
    /// read-only copy there is flushed by the drive in its own time.
    fn flush(path: &Path) -> io::Result<()> {
        match fs::OpenOptions::new().read(true).write(cfg!(windows)).open(path) {
            Ok(file) => crate::run::durable::sync_file(&file),
            Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Permanently removes the dated folders `names` from `root`'s trash (those a review
    /// listed). Anything else in the trash stays.
    pub fn sweep(root: &Path, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let Ok(trash) = Self::real_trash(root, false) else { return };
        for name in names {
            if Self::parse_stamp(name).is_none() {
                continue;
            }
            let folder = trash.join(name);
            // Only a real folder: remove_dir_all never follows links inside it either.
            if fs::symlink_metadata(&folder).is_ok_and(|m| m.is_dir()) {
                if let Err(e) = fs::remove_dir_all(&folder) {
                    log::warn!("couldn't clear old trash {}: {e}", folder.display());
                }
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
        NaiveDateTime::parse_from_str(name, STAMP_FORMAT).is_ok()
    }

    fn parse_stamp(name: &str) -> Option<DateTime<Local>> {
        let naive = NaiveDateTime::parse_from_str(name, STAMP_FORMAT).ok()?;
        Local.from_local_datetime(&naive).earliest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(Trash::expired(&root, 1).is_empty());
        Trash::sweep(&root, &["2000-01-01_00-00-00".to_string()]);
        assert!(elsewhere.join("2000-01-01_00-00-00/keep.txt").exists(), "sweeping never follows the link");
    }

    #[test]
    fn lists_and_sweeps_only_old_dated_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path().join(TRASH_FOLDER);
        fs::create_dir_all(trash.join("2000-01-01_00-00-00/a")).unwrap();
        fs::write(trash.join("2000-01-01_00-00-00/a/x.txt"), "12345").unwrap();
        fs::create_dir_all(trash.join("not-a-stamp")).unwrap();
        let fresh = Local::now().format(STAMP_FORMAT).to_string();
        fs::create_dir_all(trash.join(&fresh)).unwrap();
        let expired = Trash::expired(tmp.path(), 30);
        assert_eq!(expired, vec![Expired { name: "2000-01-01_00-00-00".into(), items: 1, bytes: 5 }]);
        assert!(Trash::expired(tmp.path(), 0).is_empty(), "0 keeps everything");
        Trash::sweep(tmp.path(), &[expired[0].name.clone(), "not-a-stamp".into()]);
        assert!(!trash.join("2000-01-01_00-00-00").exists());
        assert!(trash.join("not-a-stamp").exists());
        assert!(trash.join(fresh).exists());
    }
}
