//! What kind of drive a folder is on, and what that drive can hold.
//!
//! Drives differ in ways that matter for a sync: FAT and exFAT keep times in local time
//! (so a daylight-saving change moves every file by an hour) and can't hold links, files
//! over 4 GB on FAT, or names with characters like `:` or `?`; most Mac and Windows drives
//! treat "Report.pdf" and "report.pdf" as one name; network shares may show another
//! computer's changes late. Charon looks at each folder's drive before it plans, so an item
//! the other drive can't hold becomes a note instead of a step that fails on every run.

use crate::scan::MARKER_FOLDER;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The family of file system a drive uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FsKind {
    Apfs,
    Hfs,
    Fat,
    Exfat,
    Ntfs,
    Refs,
    Smb,
    Nfs,
    Afp,
    Ext,
    Btrfs,
    Xfs,
    Zfs,
    Other,
}

/// One drive, as seen from one folder on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub kind: FsKind,
    /// The file system's name as the system reports it ("apfs", "msdos", "NTFS", "0xef53").
    pub fs: String,
    /// Where the drive is mounted (macOS, Linux), or its serial number (Windows). A folder
    /// whose drive changed (a leftover mount folder on the computer's own disk) shows here.
    pub mount: String,
    /// A name for the person: "Backup SSD", "startup disk".
    pub name: String,
    /// A network share: other computers may change it, and it may show their changes late.
    pub network: bool,
    /// Whether "Report.pdf" and "report.pdf" are two names on this drive.
    pub case_sensitive: bool,
}

impl Volume {
    /// The drive `root` is on, without asking how it treats case: quick enough to ask
    /// every second. `None` where the system can't tell.
    pub fn basics(root: &Path) -> Option<Volume> {
        system::volume_of(root)
    }

    /// The drive of the folder `root`. Never fails: what can't be found out is assumed in the
    /// careful direction (another kind of drive, ignoring case).
    pub fn of(root: &Path) -> Volume {
        let mut volume = system::volume_of(root).unwrap_or_else(|| Volume {
            kind: FsKind::Other,
            fs: "unknown".into(),
            mount: String::new(),
            name: Self::folder_name(root),
            network: false,
            case_sensitive: false,
        });
        let asked = system::case_sensitive(root).or_else(|| probe_case(root)).or_else(|| probe_case_with_own_file(root));
        volume.case_sensitive = asked.unwrap_or_else(|| volume.kind.usually_case_sensitive());
        volume
    }

    /// The kind of drive in words: "APFS", "exFAT", "network share (SMB)".
    pub fn kind_label(&self) -> String {
        match self.kind {
            FsKind::Apfs => "APFS".into(),
            FsKind::Hfs => "Mac OS Extended".into(),
            FsKind::Fat => "FAT32".into(),
            FsKind::Exfat => "exFAT".into(),
            FsKind::Ntfs => "NTFS".into(),
            FsKind::Refs => "ReFS".into(),
            FsKind::Smb => "network share (SMB)".into(),
            FsKind::Nfs => "network share (NFS)".into(),
            FsKind::Afp => "network share (AFP)".into(),
            FsKind::Ext => "ext4".into(),
            FsKind::Btrfs => "Btrfs".into(),
            FsKind::Xfs => "XFS".into(),
            FsKind::Zfs => "ZFS".into(),
            FsKind::Other if self.network => format!("network share ({})", self.fs),
            FsKind::Other => self.fs.clone(),
        }
    }

    /// Whether this drive may show changes late: a network share (another computer's
    /// change), or a drive run by a program instead of the system (FUSE: sshfs, rclone and
    /// most cloud drives), which keeps its own picture of the folders for a while. A look at
    /// one item there opens it, so the drive itself answers.
    pub fn shows_changes_late(&self) -> bool {
        self.network || self.fs.to_ascii_lowercase().contains("fuse")
    }

    /// Whether this drive keeps times in local time, so a daylight-saving change or a trip
    /// to another time zone moves every file's time by whole hours.
    pub fn shifts_times(&self) -> bool {
        matches!(self.kind, FsKind::Fat | FsKind::Exfat)
    }

    /// Whether this drive keeps a trustworthy change time for each file (it moves with
    /// every edit, even one that puts the old modification time back). Local Mac and Linux
    /// drives do; FAT and exFAT have none, and network shares and other drivers report
    /// whatever they like.
    pub fn keeps_change_times(&self) -> bool {
        !self.network && matches!(self.kind, FsKind::Apfs | FsKind::Hfs | FsKind::Ext | FsKind::Btrfs | FsKind::Xfs | FsKind::Zfs)
    }

    /// Whether this drive keeps modification times in steps of a second or more (Mac OS
    /// Extended, FAT, exFAT, network shares, and drives run by a program, like sshfs), so two
    /// writes within one step look the same.
    pub fn coarse_times(&self) -> bool {
        self.shows_changes_late() || matches!(self.kind, FsKind::Hfs | FsKind::Fat | FsKind::Exfat | FsKind::Smb | FsKind::Nfs | FsKind::Afp)
    }

    /// Whether this drive's times can be held against another drive's to say which version
    /// is newer: not on network shares, nor on drivers Charon doesn't know (a FAT drive seen
    /// through one keeps local times nobody can account for).
    pub fn comparable_times(&self) -> bool {
        !self.network && (self.kind != FsKind::Other || matches!(self.fs.as_str(), "tmpfs" | "overlay"))
    }

    /// Whether symbolic links can be created here.
    pub fn holds_links(&self) -> bool {
        !matches!(self.kind, FsKind::Fat | FsKind::Exfat)
    }

    /// Whether extended attributes (Finder tags and the like) can be kept here. FAT and exFAT
    /// drives only fake them with "._" files next to every file.
    pub fn holds_extended_attributes(&self) -> bool {
        !matches!(self.kind, FsKind::Fat | FsKind::Exfat)
    }

    /// Why a file of `size` bytes can't be stored here, if it can't.
    pub fn too_big(&self, size: u64) -> Option<String> {
        (self.kind == FsKind::Fat && size > u32::MAX as u64)
            .then(|| format!("it's over 4 GB, and the {} can't hold files that big", self.describe()))
    }

    /// Why an item named `name` can't be created here, if it can't.
    pub fn cannot_hold(&self, name: &str) -> Option<String> {
        if self.kind.uses_windows_names() {
            if let Some(reason) = windows_name_problem(name) {
                return Some(format!("{reason}, which the {} can't hold", self.describe()));
            }
        }
        let too_long = if self.kind.counts_utf16() {
            name.encode_utf16().count() > 255
        } else {
            name.len() > 255
        };
        too_long.then(|| format!("its name is too long for the {}", self.describe()))
    }

    /// "Backup SSD (exFAT)".
    pub fn describe(&self) -> String {
        format!("{} ({})", self.name, self.kind_label())
    }

    fn folder_name(path: &Path) -> String {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
    }
}

impl FsKind {
    /// Drives that keep Windows' rules for names (also when used from a Mac or Linux).
    fn uses_windows_names(self) -> bool {
        matches!(self, FsKind::Fat | FsKind::Exfat | FsKind::Ntfs | FsKind::Refs | FsKind::Smb)
    }

    /// Drives that count name length in UTF-16 units rather than bytes.
    fn counts_utf16(self) -> bool {
        matches!(self, FsKind::Fat | FsKind::Exfat | FsKind::Ntfs | FsKind::Refs | FsKind::Smb | FsKind::Hfs)
    }

    /// The usual case setting of this kind of drive, when the drive can't be asked.
    fn usually_case_sensitive(self) -> bool {
        matches!(self, FsKind::Ext | FsKind::Btrfs | FsKind::Xfs | FsKind::Zfs | FsKind::Nfs)
    }

    fn from_name(fs: &str) -> FsKind {
        match fs.to_ascii_lowercase().as_str() {
            "apfs" => FsKind::Apfs,
            "hfs" | "hfsplus" => FsKind::Hfs,
            "msdos" | "vfat" | "fat" | "fat32" | "fat16" | "fat12" => FsKind::Fat,
            "exfat" => FsKind::Exfat,
            "ntfs" | "ntfs3" => FsKind::Ntfs,
            "refs" => FsKind::Refs,
            "smbfs" | "cifs" | "smb2" | "smb3" => FsKind::Smb,
            "nfs" | "nfs4" => FsKind::Nfs,
            "afpfs" => FsKind::Afp,
            "ext2" | "ext3" | "ext4" => FsKind::Ext,
            "btrfs" => FsKind::Btrfs,
            "xfs" => FsKind::Xfs,
            "zfs" => FsKind::Zfs,
            _ => FsKind::Other,
        }
    }
}

/// What Windows (and any drive formatted for it) can't take in a name.
pub(crate) fn windows_name_problem(name: &str) -> Option<&'static str> {
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("its name ends with a dot or a space");
    }
    if name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20) {
        return Some("its name has a character like : ? * \" < > | or \\");
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    reserved.then_some("its name is reserved by Windows (like CON or NUL)")
}

/// Asks the drive whether it tells upper and lower case apart, using names already in the
/// folder: if "Photos" exists, is "pHOTOS" the same item? Only ASCII letters are swapped.
/// `None` when no name in the folder can tell.
fn probe_case(root: &Path) -> Option<bool> {
    let items = std::fs::read_dir(root).ok()?;
    for item in items.take(64).flatten() {
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        let swapped = swap_case(name);
        if swapped == name {
            continue;
        }
        let Ok(original) = std::fs::symlink_metadata(root.join(name)) else { continue };
        match std::fs::symlink_metadata(root.join(&swapped)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(true),
            Err(_) => continue,
            Ok(other) => return Some(!same_item(&original, &other)),
        }
    }
    None
}

/// Asks the drive with a file of Charon's own where the folder's names can't tell (an empty
/// folder, or names without letters), so a folder gets the same answer empty as once it
/// fills up. The file is made in a `.charon` folder made for this, and both are removed at
/// once. `None` where the folder can't be written to.
fn probe_case_with_own_file(root: &Path) -> Option<bool> {
    let folder = root.join(MARKER_FOLDER);
    std::fs::create_dir(&folder).ok()?;
    let name = format!("Case-Probe-{:016x}.tmp", uuid::Uuid::new_v4().as_u128() as u64);
    let probe = folder.join(&name);
    let answer = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).ok().and_then(|_| {
        match std::fs::symlink_metadata(folder.join(swap_case(&name))) {
            Ok(_) => Some(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(true),
            Err(_) => None,
        }
    });
    let _ = std::fs::remove_file(&probe);
    let _ = std::fs::remove_dir(&folder);
    answer
}

/// `name` with the upper and lower case of its ASCII letters swapped.
fn swap_case(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() })
        .collect()
}

/// Whether two looks found the same item. Some network shares make up item numbers per
/// name, so equal size, time and kind count as the same item too.
fn same_item(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() == b.dev() && a.ino() == b.ino() {
            return true;
        }
    }
    a.file_type() == b.file_type() && a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// Keeps the current thread from downloading files that are only in the cloud (iCloud Drive,
/// Dropbox, OneDrive placeholders) when it looks into them: such places fail quickly and are
/// left alone, instead of filling the disk with downloads. macOS only; elsewhere placeholders
/// are recognised by their attributes.
pub fn never_download_on_this_thread() {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
        }
        const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;
        const IOPOL_SCOPE_THREAD: libc::c_int = 1;
        const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;
        // SAFETY: plain values; the call only changes this thread's I/O policy.
        let result = unsafe { setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD, IOPOL_MATERIALIZE_DATALESS_FILES_OFF) };
        if result != 0 {
            log::debug!("couldn't turn off downloads of cloud files: {}", std::io::Error::last_os_error());
        }
    }
}

#[cfg(target_os = "macos")]
mod system {
    use super::{FsKind, Volume};
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn volume_of(root: &Path) -> Option<Volume> {
        let path = CString::new(root.as_os_str().as_bytes()).ok()?;
        let mut info: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and `info` is a valid buffer for the call.
        if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
            return None;
        }
        // SAFETY: the system fills both fields with NUL-terminated strings.
        let fs = unsafe { CStr::from_ptr(info.f_fstypename.as_ptr()) }.to_string_lossy().into_owned();
        let mount = unsafe { CStr::from_ptr(info.f_mntonname.as_ptr()) }.to_string_lossy().into_owned();
        let local = info.f_flags & libc::MNT_LOCAL as u32 != 0;
        let kind = FsKind::from_name(&fs);
        let name = match mount.as_str() {
            "/" | "/System/Volumes/Data" => "startup disk".to_string(),
            other => Path::new(other).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| other.to_string()),
        };
        Some(Volume { kind, fs, mount, name, network: !local, case_sensitive: false })
    }

    /// macOS answers this directly.
    pub fn case_sensitive(root: &Path) -> Option<bool> {
        let path = CString::new(root.as_os_str().as_bytes()).ok()?;
        // SAFETY: `path` is NUL-terminated.
        match unsafe { libc::pathconf(path.as_ptr(), libc::_PC_CASE_SENSITIVE) } {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }
    }
}

#[cfg(target_os = "linux")]
mod system {
    use super::{FsKind, Volume};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    pub fn volume_of(root: &Path) -> Option<Volume> {
        let path = CString::new(root.as_os_str().as_bytes()).ok()?;
        let mut info: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and `info` is a valid buffer for the call.
        if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
            return None;
        }
        let magic = info.f_type as u64 & 0xffff_ffff;
        let (fs, network) = match magic {
            0xEF53 => ("ext4", false),
            0x9123_683E => ("btrfs", false),
            0x5846_5342 => ("xfs", false),
            0x4D44 => ("vfat", false),
            0x2011_BAB0 => ("exfat", false),
            0x5346_544E | 0x7366_746E => ("ntfs", false),
            0x482B => ("hfsplus", false),
            0x2FC1_2FC1 => ("zfs", false),
            0x517B | 0xFF53_4D42 | 0xFE53_4D42 => ("cifs", true),
            0x6969 => ("nfs", true),
            0x6573_5546 => ("fuse", false),
            0x0102_1994 => ("tmpfs", false),
            0x794C_7630 => ("overlay", false),
            _ => ("", false),
        };
        let fs = if fs.is_empty() { format!("0x{magic:x}") } else { fs.to_string() };
        let mount = mount_point(root);
        let name = match mount.as_str() {
            "/" => "system disk".to_string(),
            other => Path::new(other).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| other.to_string()),
        };
        Some(Volume { kind: FsKind::from_name(&fs), fs, mount, name, network, case_sensitive: true })
    }

    /// The topmost folder above `root` on the same drive.
    fn mount_point(root: &Path) -> String {
        let Ok(start) = std::fs::canonicalize(root) else { return String::new() };
        let Ok(device) = std::fs::metadata(&start).map(|m| m.dev()) else { return String::new() };
        let mut top = start.as_path();
        while let Some(parent) = top.parent() {
            match std::fs::metadata(parent) {
                Ok(meta) if meta.dev() == device => top = parent,
                _ => break,
            }
        }
        top.display().to_string()
    }

    /// Linux has no general answer; the folder's own names are asked instead.
    pub fn case_sensitive(_root: &Path) -> Option<bool> {
        None
    }
}

#[cfg(windows)]
mod system {
    use super::{FsKind, Volume};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationByHandleW, GetVolumePathNameW, FILE_FLAG_BACKUP_SEMANTICS};

    /// GetDriveTypeW's answer for network drives.
    const DRIVE_REMOTE: u32 = 4;

    pub fn volume_of(root: &Path) -> Option<Volume> {
        let folder = std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(root).ok()?;
        let mut label = [0u16; 261];
        let mut fs_name = [0u16; 261];
        let (mut serial, mut longest, mut flags) = (0u32, 0u32, 0u32);
        // SAFETY: the handle is open and every buffer is valid for the sizes given.
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                folder.as_raw_handle() as _,
                label.as_mut_ptr(),
                label.len() as u32,
                &mut serial,
                &mut longest,
                &mut flags,
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        let text = |buffer: &[u16]| String::from_utf16_lossy(&buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())]);
        let fs = text(&fs_name);
        let label = text(&label);
        let plain: Vec<u16> = root.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut mount = [0u16; 1024];
        // SAFETY: the path is NUL-terminated and `mount` is valid for its length.
        let ask = |path: &[u16], mount: &mut [u16; 1024]| unsafe { GetVolumePathNameW(path.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } != 0;
        // A path longer than 260 characters only works in its long form (`\\?\C:\…`), which
        // then comes back in the answer too.
        let has_mount = ask(&plain, &mut mount) || ask(&crate::run::rename::wide(root), &mut mount);
        let mount_text = if has_mount { text(&mount) } else { String::new() };
        let mount_text = match mount_text.strip_prefix(r"\\?\UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => mount_text.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(mount_text),
        };
        let spelled = root.as_os_str().to_string_lossy();
        let unc = spelled.starts_with(r"\\?\UNC\") || (spelled.starts_with(r"\\") && !spelled.starts_with(r"\\?\"));
        // SAFETY: `mount` is NUL-terminated when `has_mount`.
        let remote = has_mount && unsafe { GetDriveTypeW(mount.as_ptr()) } == DRIVE_REMOTE;
        let network = unc || remote;
        let name = if !label.is_empty() { label } else if !mount_text.is_empty() { mount_text.trim_end_matches('\\').to_string() } else { "drive".into() };
        let kind = if network { FsKind::Smb } else { FsKind::from_name(&fs) };
        Some(Volume { kind, fs, mount: format!("{serial:08X}"), name, network, case_sensitive: false })
    }

    /// Windows programs see every drive as ignoring case.
    pub fn case_sensitive(_root: &Path) -> Option<bool> {
        Some(false)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod system {
    use super::Volume;
    use std::path::Path;

    pub fn volume_of(_root: &Path) -> Option<Volume> {
        None
    }

    pub fn case_sensitive(_root: &Path) -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(kind: FsKind) -> Volume {
        Volume { kind, fs: "x".into(), mount: "/x".into(), name: "Stick".into(), network: false, case_sensitive: false }
    }

    #[test]
    fn windows_drives_refuse_windows_names() {
        let exfat = volume(FsKind::Exfat);
        assert!(exfat.cannot_hold("a:b.txt").is_some());
        assert!(exfat.cannot_hold("notes.").is_some());
        assert!(exfat.cannot_hold("CON.txt").is_some());
        assert!(exfat.cannot_hold("normal name.txt").is_none());
        assert!(volume(FsKind::Ext).cannot_hold("a:b.txt").is_none());
    }

    #[test]
    fn long_names_are_counted_the_way_the_drive_counts() {
        let name = "é".repeat(200); // 400 bytes, 200 UTF-16 units
        assert!(volume(FsKind::Ext).cannot_hold(&name).is_some());
        assert!(volume(FsKind::Ntfs).cannot_hold(&name).is_none());
    }

    #[test]
    fn fat_drives_hold_no_big_files_links_or_attributes() {
        let fat = volume(FsKind::Fat);
        assert!(fat.too_big(5_000_000_000).is_some());
        assert!(fat.too_big(1_000).is_none());
        assert!(!fat.holds_links() && !fat.holds_extended_attributes() && fat.shifts_times());
        assert!(volume(FsKind::Exfat).too_big(5_000_000_000).is_none());
    }

    #[test]
    fn shares_and_drives_run_by_a_program_may_show_changes_late() {
        assert!(!volume(FsKind::Ext).shows_changes_late());
        let fuse = Volume { fs: "fuse".into(), ..volume(FsKind::Other) };
        assert!(fuse.shows_changes_late() && fuse.coarse_times());
        assert!(Volume { fs: "macfuse".into(), ..volume(FsKind::Other) }.shows_changes_late());
        assert!(Volume { network: true, ..volume(FsKind::Smb) }.shows_changes_late());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_empty_folder_gets_the_answer_it_gets_once_it_fills_up() {
        // tmpfs is a kind of drive whose case setting Charon can't guess.
        let shared_memory = Path::new("/dev/shm");
        let tmp = if shared_memory.is_dir() { tempfile::tempdir_in(shared_memory) } else { tempfile::tempdir() }.unwrap();
        assert!(Volume::of(tmp.path()).case_sensitive, "asked with a file of Charon's own");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "and nothing is left behind");
        std::fs::write(tmp.path().join("Photos"), "x").unwrap();
        assert!(Volume::of(tmp.path()).case_sensitive);
        // A `.charon` folder that was there before is never touched.
        std::fs::remove_file(tmp.path().join("Photos")).unwrap();
        std::fs::create_dir(tmp.path().join(MARKER_FOLDER)).unwrap();
        assert!(Volume::of(tmp.path()).case_sensitive);
        assert_eq!(std::fs::read_dir(tmp.path().join(MARKER_FOLDER)).unwrap().count(), 0);
    }

    #[test]
    fn the_drive_of_a_folder_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let found = Volume::of(tmp.path());
        assert!(!found.fs.is_empty());
        #[cfg(target_os = "linux")]
        {
            std::fs::write(tmp.path().join("Probe"), "x").unwrap();
            assert!(Volume::of(tmp.path()).case_sensitive, "Linux temp folders tell case apart");
        }
    }
}
