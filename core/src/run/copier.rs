//! Copies one file without ever leaving a half-written file under the real name.
//!
//! The data goes to `name.charon-partial` first. Only when it's complete, with the original
//! modification time and permissions, does it take the real name. An interrupted copy
//! leaves the old version untouched.

use crate::error::{Error, Result};
use crate::scan::PARTIAL_SUFFIX;
use filetime::FileTime;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const CHUNK: usize = 1024 * 1024;

pub struct Copier<'a> {
    cancel: &'a AtomicBool,
}

impl<'a> Copier<'a> {
    pub fn new(cancel: &'a AtomicBool) -> Self {
        Self { cancel }
    }

    /// Copies `from` to `partial_path(to)`, reporting bytes as they go. Returns the partial
    /// file, ready for `Copier::commit`.
    pub fn copy(&self, from: &Path, to: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<PathBuf> {
        let partial = Self::partial_path(to);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::file(parent, e))?;
        }
        let result = self.write_partial(from, &partial, on_bytes);
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result.map(|_| partial)
    }

    /// Gives the finished partial file its real name.
    pub fn commit(partial: &Path, to: &Path) -> Result<()> {
        fs::rename(partial, to).map_err(|e| {
            let _ = fs::remove_file(partial);
            Error::file(to, e)
        })
    }

    fn write_partial(&self, from: &Path, partial: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<()> {
        let meta = fs::metadata(from).map_err(|e| Error::file(from, e))?;
        let mut reader = File::open(from).map_err(|e| Error::file(from, e))?;
        let mut writer = File::create(partial).map_err(|e| Error::file(partial, e))?;
        let mut buffer = vec![0u8; CHUNK];
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = reader.read(&mut buffer).map_err(|e| Error::file(from, e))?;
            if n == 0 {
                break;
            }
            writer.write_all(&buffer[..n]).map_err(|e| Error::file(partial, e))?;
            on_bytes(n as u64);
        }
        writer.sync_all().map_err(|e| Error::file(partial, e))?;
        drop(writer);

        // Keep the original's permissions and modification time, so the next scan sees
        // both copies as equal.
        let _ = fs::set_permissions(partial, meta.permissions());
        filetime::set_file_mtime(partial, FileTime::from_last_modification_time(&meta))
            .map_err(|e| Error::file(partial, e))?;
        Ok(())
    }

    /// `report.pdf` → `report.pdf.charon-partial`.
    fn partial_path(to: &Path) -> PathBuf {
        let mut name = to.file_name().unwrap_or_default().to_os_string();
        name.push(PARTIAL_SUFFIX);
        to.with_file_name(name)
    }
}
