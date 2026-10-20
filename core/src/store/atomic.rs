//! Writes a file next to its real name and renames it into place, so a crash or a full
//! disk never leaves a half-written settings file.

use crate::error::{Error, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Makes every temporary name unique, so two saves of the same file never share one.
static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| Error::file(parent, e))?;
    let temp = temp_name(path);
    let written = write_and_sync(&temp, bytes).and_then(|()| fs::rename(&temp, path).map_err(|e| Error::file(path, e)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    sync_folder(parent);
    Ok(())
}

fn write_and_sync(temp: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|e| Error::file(temp, e))?;
    file.write_all(bytes).map_err(|e| Error::file(temp, e))?;
    crate::run::durable::sync_file(&file).map_err(|e| Error::file(temp, e))
}

/// `charon.json` → `.charon.json.4711-3.tmp`
fn temp_name(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.{}-{n}.tmp", std::process::id()))
}

/// Makes the rename itself survive a power cut (macOS and Linux; Windows has no such call).
fn sync_folder(folder: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(folder) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = folder;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("charon.json");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }
}
