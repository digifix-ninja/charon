//! Which folder a path really is: the drive it's on and the folder's number on that drive.
//!
//! A path alone can lie. When an external drive or a network share goes away, the folder
//! where it was mounted can stay behind, empty, under the same name. Anything written there
//! lands on the computer's own disk and disappears again when the drive comes back, so a
//! run must stop the moment the folder it started with isn't the one under the path anymore.
//!
//! The drive and folder numbers alone can repeat: a drive unplugged and another one plugged
//! in can get the same device number, and the top folder of a volume often has the same
//! number on every volume of a kind. So the folder's creation time counts too, where the
//! system keeps a believable one: two volumes formatted at different times differ there.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootId {
    /// The drive (device or volume number).
    device: u64,
    /// The folder's number on that drive.
    index: u64,
    /// When the folder was created, in nanoseconds since 1970, where the system keeps it.
    born: Option<i128>,
}

/// A creation time this close to now isn't believed: some network and cloud file systems
/// report the moment they were asked instead of a real creation time.
const BORN_JUST_NOW_SECS: u64 = 10;

impl RootId {
    /// The identity of the folder at `path` (following a link at `path` itself, as the scan
    /// does). `None` where the system can't tell, which never counts as a change.
    pub fn of(path: &Path) -> io::Result<Option<RootId>> {
        system::id_of(path)
    }

    /// Whether `self` and `other` are the same folder. A creation time only one of them
    /// knows (or neither) doesn't count as a difference.
    pub fn same_folder(&self, other: &RootId) -> bool {
        let born = match (self.born, other.born) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        self.device == other.device && self.index == other.index && born
    }

    /// The folder's creation time, when it's believable: not missing, not the start of
    /// 1970, and not "just now".
    fn born(meta: &std::fs::Metadata) -> Option<i128> {
        let created = meta.created().ok()?;
        let since_1970 = created.duration_since(std::time::UNIX_EPOCH).ok()?;
        if since_1970.is_zero() {
            return None;
        }
        let age = std::time::SystemTime::now().duration_since(created).unwrap_or_default();
        (age.as_secs() >= BORN_JUST_NOW_SECS).then_some(since_1970.as_nanos() as i128)
    }

    /// The drive the item at `path` is on, without following a link there (macOS and Linux).
    #[cfg(unix)]
    pub fn device_of(meta: &std::fs::Metadata) -> u64 {
        use std::os::unix::fs::MetadataExt;
        meta.dev()
    }

    pub fn device(&self) -> u64 {
        self.device
    }
}

#[cfg(unix)]
mod system {
    use super::RootId;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    pub fn id_of(path: &Path) -> io::Result<Option<RootId>> {
        let meta = std::fs::metadata(path)?;
        Ok(Some(RootId { device: meta.dev(), index: meta.ino(), born: RootId::born(&meta) }))
    }
}

#[cfg(windows)]
mod system {
    use super::RootId;
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS};

    pub fn id_of(path: &Path) -> io::Result<Option<RootId>> {
        // A folder can only be opened with backup semantics; reading its information needs no rights.
        let folder = std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the handle is open for the duration of the call and `info` is a valid buffer.
        let ok = unsafe { GetFileInformationByHandle(folder.as_raw_handle() as _, &mut info) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(RootId {
            device: info.dwVolumeSerialNumber as u64,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            born: folder.metadata().ok().and_then(|meta| RootId::born(&meta)),
        }))
    }
}

#[cfg(not(any(unix, windows)))]
mod system {
    use super::RootId;
    use std::io;
    use std::path::Path;

    pub fn id_of(path: &Path) -> io::Result<Option<RootId>> {
        std::fs::metadata(path).map(|_| None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_replaced_under_the_same_name_is_another_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("photos");
        std::fs::create_dir(&folder).unwrap();
        let before = RootId::of(&folder).unwrap();
        assert_eq!(RootId::of(&folder).unwrap(), before);
        std::fs::rename(&folder, tmp.path().join("old")).unwrap();
        std::fs::create_dir(&folder).unwrap();
        if let (Some(before), Some(now)) = (before, RootId::of(&folder).unwrap()) {
            assert!(!now.same_folder(&before));
        }
    }

    #[test]
    fn the_same_numbers_with_another_creation_time_are_another_folder() {
        let a = RootId { device: 1, index: 2, born: Some(1_000) };
        let b = RootId { device: 1, index: 2, born: Some(2_000) };
        let unknown = RootId { device: 1, index: 2, born: None };
        assert!(!a.same_folder(&b));
        assert!(a.same_folder(&unknown) && unknown.same_folder(&b));
        assert!(!a.same_folder(&RootId { device: 1, index: 3, born: Some(1_000) }));
    }
}
