//! Copies one file (or link) without ever leaving a half-written file under the real name.
//!
//! The data goes to a new file with a short temporary name next to the target
//! (`.charon-<16 hex digits>.part`), never opened through a link. The source is checked
//! before and after reading: if it changed while it was being copied, the copy is thrown
//! away (the next look picks the file up again). Only a complete copy, with the original's
//! modification time, permissions and chosen extended attributes, takes the real name.

use crate::error::{Error, Result};
use crate::run::durable;
use crate::run::rename::{rename_new, rename_over};
use crate::scan::{Entry, Scanner, PARTIAL_EXTENSION, PARTIAL_PREFIX};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const CHUNK: usize = 1024 * 1024;
/// On a drive that keeps times in steps of a second or more, a file saved this recently
/// may still be being written within the same step, where its time can't show it.
const STILL_BEING_WRITTEN_MS: i64 = 3_000;

pub struct Copier<'a> {
    cancel: &'a AtomicBool,
    /// Whether the target drive keeps extended attributes (FAT and exFAT only fake them).
    attributes: bool,
    /// The source's drive keeps times in steps of a second or more (see `Volume::coarse_times`).
    coarse_source: bool,
}

impl<'a> Copier<'a> {
    pub fn new(cancel: &'a AtomicBool) -> Self {
        Self { cancel, attributes: true, coarse_source: false }
    }

    /// Leaves extended attributes out (the target drive can't really hold them).
    pub fn without_attributes(mut self) -> Self {
        self.attributes = false;
        self
    }

    /// The source's drive keeps coarse times: a file saved in the last moments is left for
    /// the next sync rather than copied while it may still be written.
    pub fn coarse_source_times(mut self, coarse: bool) -> Self {
        self.coarse_source = coarse;
        self
    }

    /// Copies the file `from` (which must still be `expected`) to a new temporary file next
    /// to `to`, reporting bytes as they go. Returns the temporary file, ready for a commit.
    pub fn file(&self, from: &Path, to: &Path, expected: &Entry, on_bytes: &mut dyn FnMut(u64)) -> Result<PathBuf> {
        let (partial, writer) = Self::create_partial(to)?;
        let result = self.write_partial(from, &partial, writer, expected, on_bytes);
        if result.is_err() {
            Self::discard(&partial);
        }
        result.map(|()| partial)
    }

    /// Makes a link like the one at `from` (which must still be `expected`) under a
    /// temporary name next to `to`. macOS and Linux only; Windows plans never hold links.
    /// Fails with `Error::NoLinks` when the target drive can't hold links.
    pub fn link(from: &Path, to: &Path, expected: &Entry) -> Result<PathBuf> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let target = fs::read_link(from).map_err(|e| Error::file(from, e))?;
            let meta = fs::symlink_metadata(from).map_err(|e| Error::file(from, e))?;
            let now = Entry::link(target.as_os_str().as_bytes(), Scanner::modified_ms(&meta));
            if !now.unchanged(expected) {
                return Err(Error::Changed("changed since the scan".into()));
            }
            let mut tries = 0;
            loop {
                let partial = Self::partial_path(to);
                match std::os::unix::fs::symlink(&target, &partial) {
                    Ok(()) => {
                        let times = filetime::FileTime::from_last_modification_time(&meta);
                        let _ = filetime::set_symlink_file_times(&partial, times, times);
                        return Ok(partial);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 5 => tries += 1,
                    Err(e) if matches!(e.raw_os_error(), Some(code) if code == libc::EPERM || code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::ENOSYS) => {
                        return Err(Error::NoLinks)
                    }
                    Err(e) => return Err(Error::file(&partial, e)),
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (from, to, expected);
            Err(Error::NoLinks)
        }
    }

    /// Gives the finished copy its real name. Nothing may be at `to`: a file that appeared
    /// there at the last moment is never overwritten.
    pub fn commit_new(partial: &Path, to: &Path) -> Result<()> {
        rename_new(partial, to).map_err(|e| {
            Self::discard(partial);
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Changed("something new appeared there meanwhile".into())
            } else {
                Error::file(to, e)
            }
        })
    }

    /// Gives the finished copy its real name in one step, replacing the old version (which
    /// was checked a moment ago, and copied into the trash first when it's to be kept).
    pub fn commit_over(partial: &Path, to: &Path) -> Result<()> {
        rename_over(partial, to).map_err(|e| {
            Self::discard(partial);
            Error::file(to, e)
        })
    }

    /// Removes an unfinished copy.
    pub fn discard(partial: &Path) {
        let _ = fs::remove_file(partial);
    }

    /// A fresh temporary name in the folder of `to`: `.charon-0123456789abcdef.part`.
    pub fn partial_path(to: &Path) -> PathBuf {
        let random = uuid::Uuid::new_v4().as_u128() as u64;
        to.with_file_name(format!("{PARTIAL_PREFIX}{random:016x}{PARTIAL_EXTENSION}"))
    }

    /// Creates the temporary file: always a new file, never an existing one and never
    /// through a link (`create_new`).
    fn create_partial(to: &Path) -> Result<(PathBuf, File)> {
        let mut tries = 0;
        loop {
            let partial = Self::partial_path(to);
            match OpenOptions::new().write(true).create_new(true).open(&partial) {
                Ok(file) => return Ok((partial, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 5 => tries += 1,
                Err(e) => return Err(Error::file(&partial, e)),
            }
        }
    }

    fn write_partial(&self, from: &Path, partial: &Path, mut writer: File, expected: &Entry, on_bytes: &mut dyn FnMut(u64)) -> Result<()> {
        let mut reader = File::open(from).map_err(|e| Error::file(from, e))?;
        let before = reader.metadata().map_err(|e| Error::file(from, e))?;
        let changed = Scanner::change_time(&before);
        if !before.is_file() || !Entry::file(before.len(), Scanner::modified_ms(&before)).changed_at(changed).unchanged(expected) {
            return Err(Error::Changed("changed since the scan".into()));
        }
        if self.coarse_source && Scanner::modified_ms(&before) > Scanner::millis(std::time::SystemTime::now()) - STILL_BEING_WRITTEN_MS {
            return Err(Error::Changed("saved a moment ago and may still be being written, so it waits for the next sync".into()));
        }
        let mut buffer = vec![0u8; CHUNK];
        let mut written: u64 = 0;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = match reader.read(&mut buffer) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::file(from, e)),
            };
            if n == 0 {
                break;
            }
            writer.write_all(&buffer[..n]).map_err(|e| Error::file(partial, e))?;
            written += n as u64;
            on_bytes(n as u64);
        }
        durable::sync_file(&writer).map_err(|e| Error::file(partial, e))?;

        // The source must be exactly as it was when reading started: same file, same size,
        // same modification time. Otherwise the copy may mix old and new contents.
        let after_handle = reader.metadata().map_err(|e| Error::file(from, e))?;
        let after_path = fs::symlink_metadata(from).map_err(|e| Error::file(from, e))?;
        // Where the drive keeps change times, an edit that put the old time back shows too.
        let unchanged = |m: &fs::Metadata| {
            m.is_file()
                && m.len() == before.len()
                && m.modified().ok() == before.modified().ok()
                && (expected.changed == 0 || Scanner::change_time(m) == changed)
        };
        if written != before.len() || !unchanged(&after_handle) || !unchanged(&after_path) {
            return Err(Error::Changed("changed while it was being copied".into()));
        }

        if self.attributes {
            Self::copy_extended_attributes(from, partial);
        }
        // Times last, through the open file, so the copy carries the original's time. The
        // modification time is required; the creation time is a bonus some drives refuse.
        Self::keep_times(&writer, &before).map_err(|e| Error::file(partial, e))?;
        if let Some(created) = Self::created_of(&before) {
            if let Err(e) = writer.set_times(created) {
                log::debug!("couldn't keep the creation time of {}: {e}", from.display());
            }
        }
        durable::sync_file(&writer).map_err(|e| Error::file(partial, e))?;
        drop(writer);
        // Permissions after the times (a read-only file can't take new times on Windows).
        if let Err(e) = fs::set_permissions(partial, before.permissions()) {
            log::warn!("couldn't copy the permissions of {}: {e}", from.display());
        }
        Ok(())
    }

    /// Gives the copy the original's modification (and access) time. A drive that refuses
    /// the access time gets the modification time alone; one that refuses times altogether
    /// (some network and cloud drives) keeps the copy anyway: the run records the time it
    /// landed with, so the copy isn't mistaken for a change later.
    fn keep_times(writer: &File, before: &fs::Metadata) -> std::io::Result<()> {
        let refused = |e: &std::io::Error| {
            matches!(e.kind(), std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput)
        };
        match writer.set_times(Self::times_of(before)) {
            Ok(()) => Ok(()),
            Err(e) if refused(&e) => {
                let Ok(modified) = before.modified() else { return Ok(()) };
                match writer.set_times(FileTimes::new().set_modified(modified)) {
                    Ok(()) => Ok(()),
                    Err(e) if refused(&e) => {
                        log::debug!("the drive doesn't take the file's time: {e}");
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Modification and access time of the original.
    fn times_of(meta: &fs::Metadata) -> FileTimes {
        let mut times = FileTimes::new();
        if let Ok(modified) = meta.modified() {
            times = times.set_modified(modified);
        }
        if let Ok(accessed) = meta.accessed() {
            times = times.set_accessed(accessed);
        }
        times
    }

    /// The original's creation time, on systems that let it be set (macOS and Windows).
    fn created_of(meta: &fs::Metadata) -> Option<FileTimes> {
        #[cfg(any(target_os = "macos", windows))]
        {
            #[cfg(target_os = "macos")]
            use std::os::macos::fs::FileTimesExt;
            #[cfg(windows)]
            use std::os::windows::fs::FileTimesExt;
            meta.created().ok().map(|created| FileTimes::new().set_created(created))
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = meta;
            None
        }
    }

    /// The extended attributes worth carrying: Finder tags, Finder info and resource forks
    /// on macOS, the `user.` ones on Linux. Others (quarantine flags, caches, labels of
    /// one machine) stay behind.
    #[cfg(unix)]
    fn carried(name: &std::ffi::OsStr) -> bool {
        let name = name.to_string_lossy();
        if cfg!(target_os = "macos") {
            matches!(name.as_ref(), "com.apple.metadata:_kMDItemUserTags" | "com.apple.FinderInfo" | "com.apple.ResourceFork")
        } else {
            name.starts_with("user.")
        }
    }

    /// Copies the carried extended attributes. A drive that can't hold them simply doesn't
    /// get them; the file's contents are copied all the same.
    #[cfg(unix)]
    fn copy_extended_attributes(from: &Path, to: &Path) {
        let names = match xattr::list(from) {
            Ok(names) => names,
            Err(e) => {
                log::debug!("couldn't list extended attributes of {}: {e}", from.display());
                return;
            }
        };
        for name in names.filter(|name| Self::carried(name)) {
            let copied = xattr::get(from, &name).and_then(|value| match value {
                Some(value) => xattr::set(to, &name, &value),
                None => Ok(()),
            });
            match copied {
                Ok(()) => {}
                // The destination's drive can't hold extended attributes at all.
                Err(e) if matches!(e.raw_os_error(), Some(code) if code == libc::ENOTSUP || code == libc::EOPNOTSUPP) => {
                    log::debug!("{} can't hold extended attributes: {e}", to.display());
                    return;
                }
                Err(e) => log::info!("couldn't copy extended attribute {} of {}: {e}", name.to_string_lossy(), from.display()),
            }
        }
    }

    #[cfg(not(unix))]
    fn copy_extended_attributes(_from: &Path, _to: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_of(path: &Path) -> Entry {
        let meta = fs::metadata(path).unwrap();
        Entry::file(meta.len(), Scanner::modified_ms(&meta))
    }

    #[test]
    fn copies_contents_and_modification_time() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("a.txt"), tmp.path().join("b.txt"));
        fs::write(&from, "hello").unwrap();
        filetime::set_file_mtime(&from, filetime::FileTime::from_unix_time(1_600_000_000, 0)).unwrap();
        let never = AtomicBool::new(false);
        let partial = Copier::new(&never).file(&from, &to, &entry_of(&from), &mut |_| {}).unwrap();
        Copier::commit_new(&partial, &to).unwrap();
        assert_eq!(fs::read_to_string(&to).unwrap(), "hello");
        assert!(entry_of(&to).same_as(&entry_of(&from)));
    }

    #[test]
    fn refuses_a_source_that_changed_since_the_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("a.txt"), tmp.path().join("b.txt"));
        fs::write(&from, "hello").unwrap();
        let planned = entry_of(&from);
        fs::write(&from, "hello, edited").unwrap();
        let never = AtomicBool::new(false);
        let result = Copier::new(&never).file(&from, &to, &planned, &mut |_| {});
        assert!(matches!(result, Err(Error::Changed(_))));
        let leftovers: Vec<_> = fs::read_dir(tmp.path()).unwrap().flatten().filter(|i| crate::scan::is_partial(&i.file_name().to_string_lossy())).collect();
        assert!(leftovers.is_empty(), "the unfinished copy is gone");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_source_edited_with_its_old_time_put_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("a.bin"), tmp.path().join("b.bin"));
        fs::write(&from, "version one").unwrap();
        let meta = fs::metadata(&from).unwrap();
        let planned = entry_of(&from).changed_at(Scanner::change_time(&meta));
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Same size, same modification time, other contents (some tools keep the time).
        fs::write(&from, "version two").unwrap();
        filetime::set_file_mtime(&from, filetime::FileTime::from_last_modification_time(&meta)).unwrap();
        let never = AtomicBool::new(false);
        if Scanner::change_time(&fs::metadata(&from).unwrap()) != planned.changed {
            assert!(matches!(Copier::new(&never).file(&from, &to, &planned, &mut |_| {}), Err(Error::Changed(_))));
        }
    }

    #[test]
    fn a_file_saved_a_moment_ago_on_a_coarse_drive_waits() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("a.txt"), tmp.path().join("b.txt"));
        fs::write(&from, "fresh").unwrap();
        let never = AtomicBool::new(false);
        let result = Copier::new(&never).coarse_source_times(true).file(&from, &to, &entry_of(&from), &mut |_| {});
        assert!(matches!(result, Err(Error::Changed(_))));
        filetime::set_file_mtime(&from, filetime::FileTime::from_unix_time(1_600_000_000, 0)).unwrap();
        assert!(Copier::new(&never).coarse_source_times(true).file(&from, &to, &entry_of(&from), &mut |_| {}).is_ok());
    }

    #[test]
    fn commit_new_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("a.txt"), tmp.path().join("b.txt"));
        fs::write(&from, "copy").unwrap();
        let never = AtomicBool::new(false);
        let partial = Copier::new(&never).file(&from, &to, &entry_of(&from), &mut |_| {}).unwrap();
        fs::write(&to, "appeared meanwhile").unwrap();
        assert!(matches!(Copier::commit_new(&partial, &to), Err(Error::Changed(_))));
        assert_eq!(fs::read_to_string(&to).unwrap(), "appeared meanwhile");
        assert!(!partial.exists());
    }

    #[test]
    fn temporary_names_are_short_fresh_and_recognised() {
        let to = Path::new("/x/a very long name that goes on and on.mov");
        let (a, b) = (Copier::partial_path(to), Copier::partial_path(to));
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name.len(), 29);
        assert!(crate::scan::is_partial(&name));
    }

    #[cfg(unix)]
    #[test]
    fn links_are_copied_as_links() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("link"), tmp.path().join("copy"));
        std::os::unix::fs::symlink("../somewhere/else", &from).unwrap();
        let meta = fs::symlink_metadata(&from).unwrap();
        let expected = Entry::link(b"../somewhere/else", Scanner::modified_ms(&meta));
        let partial = Copier::link(&from, &to, &expected).unwrap();
        Copier::commit_new(&partial, &to).unwrap();
        assert_eq!(fs::read_link(&to).unwrap(), Path::new("../somewhere/else"));
    }
}
