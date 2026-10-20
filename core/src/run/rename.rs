//! Renaming without replacing. When a finished copy takes its real name, or a file moves
//! into the trash, nothing that appeared there at the last moment may be overwritten.
//!
//! The systems offer this directly (Linux `renameat2` with `RENAME_NOREPLACE`, macOS
//! `renamex_np` with `RENAME_EXCL`, Windows `MoveFileExW` without `REPLACE_EXISTING`).
//! Some drives don't support it (NFS shares, sshfs and other drives run by a program). There
//! a look before renaming isn't enough: the computer answers such a look from its own
//! picture of the folder, which can miss a file another computer saved seconds ago. So the
//! new name is made on the drive itself, which refuses a name that's taken:
//!
//! 1. as a second name of the same file (a hard link), after which the old name goes;
//! 2. where the drive has no second names either (and always for a folder), by creating an
//!    empty file (or folder) under the new name and then renaming over that empty one.
//!
//! Replacing a file goes through `swap` where the drive can exchange two names in one step
//! (Linux `RENAME_EXCHANGE`, macOS `RENAME_SWAP`): what was swapped out can then be checked,
//! and swapped back if it isn't what Charon meant to replace (see `Executor::replace`).

use std::fs;
use std::io;
use std::path::Path;

/// Renames `from` to `to`, failing with `AlreadyExists` if something is at `to`.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    match system::rename_no_replace(from, to) {
        Err(e) if is_unsupported(&e) => rename_through_second_name(from, to),
        other => other,
    }
}

/// Exchanges the items at `a` and `b` in one step. Fails with `Unsupported` where the system
/// or the drive can't (Windows, FAT and exFAT, most network shares).
pub fn swap(a: &Path, b: &Path) -> io::Result<()> {
    match system::swap(a, b) {
        Err(e) if is_unsupported(&e) => Err(io::Error::new(io::ErrorKind::Unsupported, e.to_string())),
        other => other,
    }
}

/// `rename_new` on a drive without its own way: `to` becomes a second name of the file at
/// `from` (the drive refuses it if the name is taken), then `from` goes. A folder has no
/// second names, so it claims its new name instead.
fn rename_through_second_name(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_dir() {
        return rename_into_claimed_name(from, to);
    }
    match fs::hard_link(from, to) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
        Err(e) if has_no_second_names(&e) => return rename_into_claimed_name(from, to),
        Err(e) => return Err(e),
    }
    if let Err(e) = fs::remove_file(from) {
        // The file keeps its old name only, as if nothing happened.
        if same_file(from, to) {
            let _ = fs::remove_file(to);
        }
        return Err(e);
    }
    Ok(())
}

/// `rename_new` on a drive without second names (FAT, exFAT, most cloud drives), and for
/// folders: an empty file (or an empty folder, for a folder) claims the name `to` first (the
/// drive refuses it if the name is taken), and the item at `from` then takes its place. A
/// claimed folder that something was saved into meanwhile isn't empty, and the drive refuses
/// to rename over it.
fn rename_into_claimed_name(from: &Path, to: &Path) -> io::Result<()> {
    let folder = fs::symlink_metadata(from)?.is_dir();
    let claimed = if folder {
        fs::create_dir(to)?;
        fs::symlink_metadata(to).ok()
    } else {
        let claim = fs::OpenOptions::new().write(true).create_new(true).open(to)?;
        claim.metadata().ok()
    };
    if let Err(e) = fs::rename(from, to) {
        // The name is given back, unless something else took the place of the empty claim.
        let still_the_claim = |now: &fs::Metadata| claimed.as_ref().is_some_and(|claimed| same_item(now, claimed));
        match fs::symlink_metadata(to) {
            Ok(now) if folder && now.is_dir() && still_the_claim(&now) => {
                // Refused if something was saved into it meanwhile: then it stays.
                let _ = fs::remove_dir(to);
            }
            Ok(now) if !folder && now.is_file() && now.len() == 0 && still_the_claim(&now) => {
                let _ = fs::remove_file(to);
            }
            _ => {}
        }
        return Err(e);
    }
    Ok(())
}

/// Whether `a` and `b` are names of one and the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => same_item(&a, &b),
        _ => false,
    }
}

/// Whether two looks found the same item (by its drive and number; never on systems
/// without them, so nothing is removed on a guess).
fn same_item(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Whether a hard link failed because the drive (or the file's owner) doesn't allow one.
fn has_no_second_names(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(error.raw_os_error(), Some(code) if code == libc::EPERM || code == libc::ENOSYS || code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::EMLINK)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// `path` as a NUL-terminated UTF-16 string for a direct Windows call: absolute, with `\`
/// separators and the `\\?\` prefix, so paths longer than 260 characters work too (the
/// standard library does this by itself for its own calls).
#[cfg(windows)]
pub(crate) fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let units_of = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let units: Vec<u16> = absolute.as_os_str().encode_wide().map(|c| if c == '/' as u16 { '\\' as u16 } else { c }).collect();
    let mut verbatim = if units.starts_with(&units_of(r"\\?\")) {
        units
    } else if units.starts_with(&units_of(r"\\")) {
        units_of(r"\\?\UNC\").into_iter().chain(units[2..].iter().copied()).collect()
    } else {
        units_of(r"\\?\").into_iter().chain(units).collect()
    };
    verbatim.push(0);
    verbatim
}

fn is_unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(error.raw_os_error(), Some(code) if code == libc::EINVAL || code == libc::ENOSYS || code == libc::ENOTSUP || code == libc::EOPNOTSUPP)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
mod system {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: both paths are valid NUL-terminated strings that live through the call.
        let result = unsafe { libc::renameat2(libc::AT_FDCWD, from.as_ptr(), libc::AT_FDCWD, to.as_ptr(), libc::RENAME_NOREPLACE) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn swap(a: &Path, b: &Path) -> io::Result<()> {
        let a = CString::new(a.as_os_str().as_bytes())?;
        let b = CString::new(b.as_os_str().as_bytes())?;
        // SAFETY: both paths are valid NUL-terminated strings that live through the call.
        let result = unsafe { libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_EXCHANGE) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(target_os = "macos")]
mod system {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: both paths are valid NUL-terminated strings that live through the call.
        let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn swap(a: &Path, b: &Path) -> io::Result<()> {
        let a = CString::new(a.as_os_str().as_bytes())?;
        let b = CString::new(b.as_os_str().as_bytes())?;
        // SAFETY: both paths are valid NUL-terminated strings that live through the call.
        let result = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod system {
    use super::wide;
    use std::io;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    fn move_file(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL-terminated UTF-16 strings that live through the call.
        let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
        if moved != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Without MOVEFILE_REPLACE_EXISTING, Windows refuses to overwrite.
    pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        move_file(from, to, MOVEFILE_WRITE_THROUGH)
    }

    /// Windows has no call that exchanges two names.
    pub fn swap(_a: &Path, _b: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Windows can't exchange two names"))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod system {
    use std::io;
    use std::path::Path;

    pub fn rename_no_replace(_from: &Path, _to: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "not on this system"))
    }

    pub fn swap(_a: &Path, _b: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "not on this system"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renames `a` onto a taken name `b`, then onto a free one, the way `rename` does.
    fn never_replaces_with(rename: fn(&Path, &Path) -> io::Result<()>) {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, "new").unwrap();
        fs::write(&b, "precious").unwrap();
        assert_eq!(rename(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&b).unwrap(), "precious");
        assert_eq!(fs::read_to_string(&a).unwrap(), "new", "the file keeps its old name");
        fs::remove_file(&b).unwrap();
        rename(&a, &b).unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "new");
        assert!(fs::symlink_metadata(&a).is_err(), "the old name is gone");
    }

    #[test]
    fn never_replaces() {
        never_replaces_with(rename_new);
    }

    #[test]
    fn never_replaces_on_drives_without_their_own_way() {
        // NFS shares, sshfs and the like: the drive itself refuses a second name that's taken.
        never_replaces_with(rename_through_second_name);
        // Drives without second names: the drive itself refuses to create a file that's there.
        never_replaces_with(rename_into_claimed_name);
    }

    #[cfg(unix)]
    #[test]
    fn links_keep_being_links_on_drives_without_their_own_way() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, c) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("c"));
        std::os::unix::fs::symlink("../elsewhere", &a).unwrap();
        rename_through_second_name(&a, &b).unwrap();
        assert_eq!(fs::read_link(&b).unwrap(), Path::new("../elsewhere"));
        rename_into_claimed_name(&b, &c).unwrap();
        assert_eq!(fs::read_link(&c).unwrap(), Path::new("../elsewhere"));
    }

    #[test]
    fn a_failed_rename_gives_the_claimed_name_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (missing, to) = (tmp.path().join("missing"), tmp.path().join("to"));
        assert!(rename_into_claimed_name(&missing, &to).is_err());
        assert!(fs::symlink_metadata(&to).is_err(), "no empty file is left under the name");
    }

    #[test]
    fn folders_claim_their_new_name_too() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, c) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("c"));
        fs::create_dir(&a).unwrap();
        fs::write(a.join("inside"), "kept").unwrap();
        fs::create_dir(&b).unwrap();
        assert_eq!(rename_through_second_name(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists, "an empty folder is taken too");
        assert!(a.join("inside").exists() && b.read_dir().unwrap().next().is_none());
        rename_through_second_name(&a, &c).unwrap();
        assert_eq!(fs::read_to_string(c.join("inside")).unwrap(), "kept");
        assert!(fs::symlink_metadata(&a).is_err(), "the old name is gone");
    }

    #[test]
    fn swapping_exchanges_two_names_where_the_drive_can() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, "one").unwrap();
        fs::write(&b, "two").unwrap();
        match swap(&a, &b) {
            Ok(()) => {
                assert_eq!(fs::read_to_string(&a).unwrap(), "two");
                assert_eq!(fs::read_to_string(&b).unwrap(), "one");
            }
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{e}");
                assert_eq!(fs::read_to_string(&a).unwrap(), "one");
            }
        }
    }
}
