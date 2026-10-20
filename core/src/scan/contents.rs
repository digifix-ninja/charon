//! Reading files' contents, and comparing two files byte for byte.
//!
//! Before the first two-way sync (or when Charon starts afresh) there is no memory of which
//! side changed what. Two files of the same size with different times may well be the same
//! file, copied by hand or by a tool that didn't keep the time. Rather than calling every
//! such pair a conflict, Charon reads both and compares.
//!
//! Whatever Charon reads, it never downloads: a file that is only in the cloud (iCloud
//! Drive, OneDrive, Dropbox free up space that way, also after a scan) is refused instead.

use crate::scan::Scanner;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const CHUNK: usize = 1024 * 1024;

pub struct Contents;

/// A file found only in the cloud when Charon came to read it (see `Contents::open`).
#[derive(Debug)]
struct OnlyInTheCloud;

impl std::fmt::Display for OnlyInTheCloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("it's only in the cloud (not downloaded to this computer)")
    }
}

impl std::error::Error for OnlyInTheCloud {}

impl Contents {
    /// Opens the file at `path` to read its contents, never downloading it. On macOS a
    /// thread that reads contents can't download (see `never_download_on_this_thread`):
    /// reading such a file fails. On Windows a file only in the cloud reads like any other
    /// and is downloaded as it's read, so it's opened without being brought back where the
    /// system allows that (`FILE_FLAG_OPEN_NO_RECALL`). On both, the opened file's own
    /// details must say its contents are on this computer.
    pub fn open(path: &Path) -> io::Result<File> {
        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_NO_RECALL;
            std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_NO_RECALL).open(path)?
        };
        #[cfg(not(windows))]
        let file = File::open(path)?;
        if Scanner::placeholder(&file.metadata()?).is_some() {
            return Err(io::Error::other(OnlyInTheCloud));
        }
        Ok(file)
    }

    /// Whether `error`, from opening or reading a file, says it's only in the cloud.
    pub fn only_in_the_cloud(error: &io::Error) -> bool {
        #[cfg(target_os = "macos")]
        if error.raw_os_error() == Some(libc::EDEADLK) {
            return true;
        }
        error.get_ref().is_some_and(|inner| inner.is::<OnlyInTheCloud>())
    }

    /// Whether the files at `a` and `b` hold exactly the same bytes. Stops early with
    /// `Interrupted` when `cancel` becomes true.
    pub fn same(a: &Path, b: &Path, cancel: Option<&AtomicBool>) -> io::Result<bool> {
        let (mut first, mut second) = (Self::open(a)?, Self::open(b)?);
        if first.metadata()?.len() != second.metadata()?.len() {
            return Ok(false);
        }
        let (mut left, mut right) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
            }
            let n = Self::fill(&mut first, &mut left)?;
            let m = Self::fill(&mut second, &mut right)?;
            if n != m || left[..n] != right[..m] {
                return Ok(false);
            }
            if n == 0 {
                return Ok(true);
            }
        }
    }

    /// Reads until `buffer` is full or the file ends.
    fn fill(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
        let mut filled = 0;
        while filled < buffer.len() {
            match file.read(&mut buffer[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(filled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_bytes_not_times() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, c) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("c"));
        std::fs::write(&a, "same words").unwrap();
        std::fs::write(&b, "same words").unwrap();
        std::fs::write(&c, "some words").unwrap();
        assert!(Contents::same(&a, &b, None).unwrap());
        assert!(!Contents::same(&a, &c, None).unwrap());
    }

    #[test]
    fn only_a_file_found_only_in_the_cloud_says_so() {
        assert!(Contents::only_in_the_cloud(&io::Error::other(OnlyInTheCloud)));
        assert!(!Contents::only_in_the_cloud(&io::Error::other("the disk broke")));
        assert!(!Contents::only_in_the_cloud(&io::Error::from(io::ErrorKind::NotFound)));
        #[cfg(target_os = "macos")]
        assert!(Contents::only_in_the_cloud(&io::Error::from_raw_os_error(libc::EDEADLK)));
    }
}
