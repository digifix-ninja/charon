//! Writes a file next to its real name and renames it into place, so a crash or a full
//! disk never leaves a half-written settings file.

use crate::error::{Error, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::file(parent, e))?;
    }
    let temp = path.with_extension("tmp");
    let mut file = fs::File::create(&temp).map_err(|e| Error::file(&temp, e))?;
    file.write_all(bytes).map_err(|e| Error::file(&temp, e))?;
    file.sync_all().map_err(|e| Error::file(&temp, e))?;
    drop(file);
    fs::rename(&temp, path).map_err(|e| Error::file(path, e))
}
