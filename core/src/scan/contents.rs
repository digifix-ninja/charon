//! Comparing two files byte for byte.
//!
//! Before the first two-way sync (or when Charon starts afresh) there is no memory of which
//! side changed what. Two files of the same size with different times may well be the same
//! file, copied by hand or by a tool that didn't keep the time. Rather than calling every
//! such pair a conflict, Charon reads both and compares.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const CHUNK: usize = 1024 * 1024;

pub struct Contents;

impl Contents {
    /// Whether the files at `a` and `b` hold exactly the same bytes. Stops early with
    /// `Interrupted` when `cancel` becomes true.
    pub fn same(a: &Path, b: &Path, cancel: Option<&AtomicBool>) -> io::Result<bool> {
        let (mut first, mut second) = (File::open(a)?, File::open(b)?);
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
}
