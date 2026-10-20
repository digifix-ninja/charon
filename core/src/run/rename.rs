//! Renaming without replacing. When a finished copy takes its real name, or a file moves
//! into the trash, nothing that appeared there at the last moment may be overwritten.
//!
//! The systems offer this directly (Linux `renameat2` with `RENAME_NOREPLACE`, macOS
//! `renamex_np` with `RENAME_EXCL`, Windows `MoveFileExW` without `REPLACE_EXISTING`).
//! Where a drive doesn't support it (some network shares), Charon looks first and then
//! renames: the gap is a few microseconds.

use std::fs;
use std::io;
use std::path::Path;

/// Renames `from` to `to` in one step, replacing what's at `to` (used only right after
/// checking that `to` is exactly what the plan saw). Written through to the drive on Windows.
pub fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    system::rename_over(from, to)
}

/// Renames `from` to `to`, failing with `AlreadyExists` if something is at `to`.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    match system::rename_no_replace(from, to) {
        Err(e) if is_unsupported(&e) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "something is already there"));
            }
            fs::rename(from, to)
        }
        other => other,
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

    pub fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
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

    pub fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
}

#[cfg(windows)]
mod system {
    use super::wide;
    use std::io;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

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

    /// A file another program has open can't be replaced: Windows refuses, which is safe.
    pub fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
        move_file(from, to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod system {
    use std::io;
    use std::path::Path;

    pub fn rename_no_replace(_from: &Path, _to: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "not on this system"))
    }

    pub fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, "new").unwrap();
        fs::write(&b, "precious").unwrap();
        assert_eq!(rename_new(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&b).unwrap(), "precious");
        fs::remove_file(&b).unwrap();
        rename_new(&a, &b).unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "new");
    }
}
