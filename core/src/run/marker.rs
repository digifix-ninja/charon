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

/// What the name of the probe file ends with that tells whether two folders are one (see
/// `Marker::origin`). It's there only for a moment.
pub const PROBE_EXTENSION: &str = ".probe";

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

/// Where a job's marker found in a folder other than the job's own comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The folder is the job's own folder, reached under another name (a drive or share
    /// mounted twice, a link).
    SameFolder,
    /// The folder holds a copy of the job's marker: the person copied the job's folder,
    /// hidden `.charon` folder and all, or the copy is older than the marker there now.
    Copy,
    /// The job's own folder can't be reached now.
    Away,
    /// Charon can't tell (neither Charon folder takes a file, or the job's own marker can't
    /// be read).
    Unknown,
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

    /// The jobs whose markers are at the top of `root`: the jobs whose folder `root` is.
    pub fn jobs_in(root: &Path) -> Vec<JobId> {
        let Ok(items) = fs::read_dir(root.join(MARKER_FOLDER)) else { return Vec::new() };
        items
            .flatten()
            .filter_map(|item| item.file_name().to_str().and_then(|name| name.strip_suffix(".json")).map(str::to_string))
            .collect()
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

    /// Where the marker of `job` found in the Charon folder of `found` comes from, when the
    /// job's own folder is `root` (see `Origin`). The two markers are read again, side by
    /// side: two different ones are two folders. Two identical ones are one folder, or a
    /// copy made since the job's last sync, which nothing in them tells apart, so Charon
    /// leaves a probe file in one of the two Charon folders, and looks for it in the other.
    pub fn origin(job: &JobId, root: &Path, found: &Path) -> Origin {
        if !root.is_dir() {
            return Origin::Away;
        }
        match (Self::read(root, job), Self::read(found, job)) {
            (Found::Marker(own), Found::Marker(seen)) if own == seen => match Self::same_place(root, found) {
                Some(true) => Origin::SameFolder,
                Some(false) => Origin::Copy,
                None => Origin::Unknown,
            },
            (Found::Unusable(_), _) | (_, Found::Unusable(_)) => Origin::Unknown,
            _ => Origin::Copy,
        }
    }

    /// Whether the folders `a` and `b`, which both hold a Charon folder, are one folder under
    /// two names: a probe file left in the Charon folder of one shows up in that of the
    /// other (a new name is looked up on the drive itself, not in what the computer
    /// remembers of the folder). `None` when neither Charon folder takes a file. Never makes
    /// a folder.
    fn same_place(a: &Path, b: &Path) -> Option<bool> {
        let name = format!("{}{PROBE_EXTENSION}", uuid::Uuid::new_v4().simple());
        for (here, there) in [(a, b), (b, a)] {
            let folder = here.join(MARKER_FOLDER);
            if !fs::symlink_metadata(&folder).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            let probe = folder.join(&name);
            if fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_err() {
                continue;
            }
            let seen = fs::symlink_metadata(there.join(MARKER_FOLDER).join(&name)).is_ok();
            let _ = fs::remove_file(&probe);
            return Some(seen);
        }
        None
    }

    /// `root/.charon`, created when missing; never a link.
    pub(crate) fn real_folder(root: &Path) -> crate::Result<PathBuf> {
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
    fn the_jobs_of_a_folder_are_read_from_their_markers() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Marker::jobs_in(tmp.path()).is_empty());
        Marker::new(&"job1".to_string(), Side::Source, "a", 1).write(tmp.path()).unwrap();
        Marker::new(&"job2".to_string(), Side::Destination, "b", 0).write(tmp.path()).unwrap();
        let mut jobs = Marker::jobs_in(tmp.path());
        jobs.sort();
        assert_eq!(jobs, vec!["job1".to_string(), "job2".to_string()]);
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

    #[test]
    fn a_copy_of_a_marker_is_told_from_the_folder_it_was_copied_from() {
        let tmp = tempfile::tempdir().unwrap();
        let (backup, restored) = (tmp.path().join("USB/Documents"), tmp.path().join("Documents/From the USB backup"));
        fs::create_dir_all(&backup).unwrap();
        fs::create_dir_all(&restored).unwrap();
        let job = "job".to_string();
        let marker = Marker::new(&job, Side::Destination, "token", 4);
        marker.write(&backup).unwrap();
        // Copied along with the files: the same marker, in another folder.
        marker.write(&restored).unwrap();
        assert_eq!(Marker::origin(&job, &backup, &restored), Origin::Copy);
        assert_eq!(fs::read_dir(backup.join(MARKER_FOLDER)).unwrap().count(), 1, "no probe is left behind");
        assert_eq!(fs::read_dir(restored.join(MARKER_FOLDER)).unwrap().count(), 1);
        // An older copy differs from the marker there now.
        Marker::new(&job, Side::Destination, "token", 5).write(&backup).unwrap();
        assert_eq!(Marker::origin(&job, &backup, &restored), Origin::Copy);
        // The drive is away.
        assert_eq!(Marker::origin(&job, &tmp.path().join("Elsewhere/Documents"), &restored), Origin::Away);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_reached_under_another_name_is_the_same_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let share = tmp.path().join("NAS/Docs");
        fs::create_dir_all(&share).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("NAS"), tmp.path().join("NAS-1")).unwrap();
        let job = "job".to_string();
        Marker::new(&job, Side::Destination, "token", 4).write(&share).unwrap();
        assert_eq!(Marker::origin(&job, &share, &tmp.path().join("NAS-1/Docs")), Origin::SameFolder);
        assert_eq!(fs::read_dir(share.join(MARKER_FOLDER)).unwrap().count(), 1, "no probe is left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_marker_charon_cannot_probe_leaves_it_unsure() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        let job = "job".to_string();
        for folder in [&one, &two] {
            fs::create_dir_all(folder).unwrap();
            Marker::new(&job, Side::Source, "token", 1).write(folder).unwrap();
            fs::set_permissions(folder.join(MARKER_FOLDER), fs::Permissions::from_mode(0o555)).unwrap();
        }
        // Root writes anyway: then there's nothing to show.
        let writable = fs::write(one.join(MARKER_FOLDER).join("x"), "x").is_ok();
        let origin = Marker::origin(&job, &one, &two);
        for folder in [&one, &two] {
            fs::set_permissions(folder.join(MARKER_FOLDER), fs::Permissions::from_mode(0o755)).unwrap();
        }
        if writable {
            return;
        }
        assert_eq!(origin, Origin::Unknown);
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
