//! The marker Charon leaves in a job's folders: `.charon/<job>.json` at the top of the
//! folder, with a random token and a counter that goes up with every finished sync. Both
//! sides of a two-way job carry one; a Mirror or Backup leaves one in the destination, and
//! in the source too where it can write there.
//!
//! The two-way memory only means something for the very folders it was made with. A path
//! can't tell: a replaced drive formatted with the same name, two backup drives used in
//! turns, a drive restored from last week's copy, or the empty folder an unplugged drive
//! leaves behind all sit under the same path. The marker can tell:
//!
//! * no marker, or another token: not the folder Charon synced with;
//! * an older counter: the folder went back in time (restored from a copy), or the last
//!   sync's changes never reached the drive (pulled out without ejecting).
//!
//! In each of those cases a two-way job doesn't trust its memory: it compares both sides
//! afresh, which never removes anything, and asks the person to look first. A Mirror or
//! Backup waits for the person's look too. And a marker that changed between the list the
//! person confirmed and the end of the sync means another drive came in between: nothing
//! about that sync is recorded.

use crate::model::JobId;
use crate::plan::Side;
use crate::scan::MARKER_FOLDER;
use crate::store::atomic;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub job: JobId,
    pub side: Side,
    pub token: String,
    pub generation: u64,
}

/// What a side's marker says, as far as Charon can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Marker(Marker),
    Missing,
    /// There, but unreadable or damaged.
    Unusable(String),
}

/// A job's markers on both sides, as found at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markers {
    pub source: Found,
    pub destination: Found,
}

impl Markers {
    /// Reads the markers of `job` at the top of both folders.
    pub fn read(source: &Path, destination: &Path, job: &JobId) -> Self {
        Self { source: Marker::read(source, job), destination: Marker::read(destination, job) }
    }

    pub fn side(&self, side: Side) -> &Found {
        match side {
            Side::Source => &self.source,
            Side::Destination => &self.destination,
        }
    }

    /// The first side whose marker isn't what it was in `before`: something replaced the
    /// folder (another drive under the same name) or wrote to it in between.
    pub fn changed_since(&self, before: &Markers) -> Option<Side> {
        [Side::Source, Side::Destination].into_iter().find(|&side| self.side(side) != before.side(side))
    }
}

impl Marker {
    pub fn new(job: &JobId, side: Side, token: &str, generation: u64) -> Self {
        Self { job: job.clone(), side, token: token.to_string(), generation }
    }

    /// A new random token for a pair of folders.
    pub fn fresh_token() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }

    /// The marker of `job` at the top of `root`.
    pub fn read(root: &Path, job: &JobId) -> Found {
        let path = Self::path(root, job);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Found::Unusable(format!("{} isn't a file", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Found::Missing,
            Err(e) => return Found::Unusable(e.to_string()),
        }
        match fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).map_err(|e| e.to_string())) {
            Ok(marker) => Found::Marker(marker),
            Err(e) => Found::Unusable(e),
        }
    }

    /// Writes the marker for good (on the drive, not only in its memory).
    pub fn write(&self, root: &Path) -> crate::Result<()> {
        let folder = Self::real_folder(root)?;
        let bytes = serde_json::to_vec_pretty(self)?;
        atomic::write(&folder.join(format!("{}.json", self.job)), &bytes)
    }

    /// Removes the marker of `job` (the job is gone), and the marker folder if it's empty.
    pub fn forget(root: &Path, job: &JobId) {
        let path = Self::path(root, job);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            let _ = fs::remove_file(&path);
        }
        let folder = root.join(MARKER_FOLDER);
        if fs::symlink_metadata(&folder).is_ok_and(|m| m.is_dir()) {
            let _ = fs::remove_dir(&folder); // only when empty
        }
    }

    fn path(root: &Path, job: &JobId) -> PathBuf {
        root.join(MARKER_FOLDER).join(format!("{job}.json"))
    }

    /// `root/.charon`, created when missing; never a link.
    fn real_folder(root: &Path) -> crate::Result<PathBuf> {
        let folder = root.join(MARKER_FOLDER);
        match fs::symlink_metadata(&folder) {
            Ok(meta) if meta.is_dir() => Ok(folder),
            Ok(_) => Err(crate::Error::Invalid(format!("{} isn't a real folder (a link or a file), so Charon won't write into it", folder.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(&folder) {
                Ok(()) => {
                    hide(&folder);
                    Ok(folder)
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Self::real_folder(root),
                Err(e) => Err(crate::Error::file(&folder, e)),
            },
            Err(e) => Err(crate::Error::file(&folder, e)),
        }
    }
}

/// Hides one of Charon's own folders on Windows (macOS and Linux hide names starting with a dot).
pub(crate) fn hide(path: &Path) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, INVALID_FILE_ATTRIBUTES};
        let wide = crate::run::rename::wide(path);
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that lives through both calls.
        unsafe {
            let attributes = GetFileAttributesW(wide.as_ptr());
            if attributes != INVALID_FILE_ATTRIBUTES {
                SetFileAttributesW(wide.as_ptr(), attributes | FILE_ATTRIBUTE_HIDDEN);
            }
        }
    }
    #[cfg(not(windows))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_markers_read_back_and_go_away() {
        let tmp = tempfile::tempdir().unwrap();
        let job = "job1".to_string();
        assert_eq!(Marker::read(tmp.path(), &job), Found::Missing);
        let marker = Marker::new(&job, Side::Destination, "token", 3);
        marker.write(tmp.path()).unwrap();
        assert_eq!(Marker::read(tmp.path(), &job), Found::Marker(marker));
        Marker::forget(tmp.path(), &job);
        assert_eq!(Marker::read(tmp.path(), &job), Found::Missing);
        assert!(!tmp.path().join(MARKER_FOLDER).exists());
    }

    #[test]
    fn a_marker_that_changed_names_its_side() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let job = "job1".to_string();
        let before = Markers::read(a.path(), b.path(), &job);
        assert_eq!(Markers::read(a.path(), b.path(), &job).changed_since(&before), None);
        Marker::new(&job, Side::Destination, "token", 1).write(b.path()).unwrap();
        assert_eq!(Markers::read(a.path(), b.path(), &job).changed_since(&before), Some(Side::Destination));
    }

    #[test]
    fn a_damaged_marker_is_unusable_not_missing() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(MARKER_FOLDER)).unwrap();
        fs::write(tmp.path().join(MARKER_FOLDER).join("j.json"), "{nope").unwrap();
        assert!(matches!(Marker::read(tmp.path(), &"j".to_string()), Found::Unusable(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_marker_folder_is_never_written_through() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        let root = tmp.path().join("root");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(MARKER_FOLDER)).unwrap();
        assert!(Marker::new(&"j".to_string(), Side::Source, "t", 1).write(&root).is_err());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }
}
