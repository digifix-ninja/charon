//! The placeholders cloud services leave in place of the files they free up.
//!
//! iCloud Drive before macOS 14 turns `Report.pdf` into a hidden `.Report.pdf.icloud`, and
//! the virtual files of Nextcloud and ownCloud are `Report.pdf.nextcloud` and
//! `Report.pdf.owncloud`. The file itself is then missing from its folder, though it's
//! still there, in the cloud: missed, it would look removed.
//!
//! A file named so is a placeholder only where its service keeps files: in iCloud Drive's
//! folders on this Mac, and in a folder a Nextcloud or ownCloud client on this computer
//! syncs, as its settings say (see `scan::clients`). Anywhere else (a backup drive, a
//! network share) it's a copy of a placeholder, made by a tool that copied a cloud folder
//! as it was, and an ordinary file: the file it names is synced there like any other.
//! Where Charon can't tell, it's taken for a placeholder: where no client's settings can be
//! read, a folder that holds a client's journal counts as one a client syncs. And where a
//! service keeps files, Charon never makes a file named like its placeholder: the service
//! would take it for its own, and the file it names for freed up.
//!
//! A client's journal of the folder it syncs, `.sync_0123456789ab.db`, is the client's own
//! and never synced or removed, wherever it is: a copy of it would only make another folder
//! look synced by the client.

use crate::scan::clients::ClientSettings;
use crate::scan::Keys;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A cloud service that leaves placeholders in place of the files it frees up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudService {
    /// iCloud Drive before macOS 14.
    ICloud,
    /// The Nextcloud and ownCloud clients, with virtual files turned on.
    SyncClient,
}

impl CloudService {
    /// The service's name, for a note.
    pub fn name(self) -> &'static str {
        match self {
            CloudService::ICloud => "iCloud Drive",
            CloudService::SyncClient => "Nextcloud or ownCloud",
        }
    }
}

/// A cloud service's placeholder for a file it freed up, told by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder<'a> {
    /// The name of the file it stands for.
    pub item: &'a str,
    /// The service that names its placeholders so.
    pub service: CloudService,
}

impl<'a> Placeholder<'a> {
    /// The placeholder named `name`, if a cloud service names its placeholders so.
    pub fn named(name: &'a str) -> Option<Self> {
        let (item, service) = match name.strip_prefix('.').and_then(|hidden| hidden.strip_suffix(".icloud")) {
            Some(item) => (item, CloudService::ICloud),
            None => (name.strip_suffix(".nextcloud").or_else(|| name.strip_suffix(".owncloud"))?, CloudService::SyncClient),
        };
        (!matches!(item, "" | "." | "..")).then_some(Self { item, service })
    }
}

/// Whether `name` is a Nextcloud or ownCloud client's journal of the folder it syncs, or
/// one of the files that go with it: `.sync_0123456789ab.db` (`._sync_0123456789ab.db`, or
/// `.csync_journal.db` from older clients), with its `-wal`, `-shm` or `-journal`.
pub fn is_sync_journal(name: &str) -> bool {
    let journal = ["-wal", "-shm", "-journal"].iter().find_map(|companion| name.strip_suffix(companion)).unwrap_or(name);
    journal == ".csync_journal.db" || CloudFolders::is_journal(journal)
}

/// The folders inside one scanned folder where cloud services keep files, by key (`""` for
/// the whole folder), with the service: there, a file named like the service's placeholders
/// is its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CloudPlaces(Vec<(String, CloudService)>);

impl CloudPlaces {
    /// Whether `service` keeps files in the folder `folder` (by key).
    pub fn keep(&self, folder: &str, service: CloudService) -> bool {
        self.0.iter().any(|(place, kept)| *kept == service && (place.is_empty() || Keys::is_within(folder, place)))
    }

    /// The cloud service that would take an item at `key` for its placeholder: the item is
    /// named like one, in a folder where that service keeps files.
    pub fn service_taking(&self, key: &str) -> Option<CloudService> {
        let placeholder = Placeholder::named(Keys::name(key))?;
        self.keep(Keys::parent(key).unwrap_or(""), placeholder.service).then_some(placeholder.service)
    }

    /// The same places, for keys that ignore upper and lower case (see `Keys::fold`).
    pub fn fold(&mut self) {
        for (place, _) in &mut self.0 {
            *place = Keys::fold(place);
        }
    }

    fn add(&mut self, folder: String, service: CloudService) {
        if !self.0.iter().any(|(place, kept)| *place == folder && *kept == service) {
            self.0.push((folder, service));
        }
    }
}

/// Finds out where cloud services keep files in one folder Charon scans, and so which files
/// named like their placeholders there are their own.
pub(crate) struct CloudFolders {
    /// The folder's real path, `None` when it can't be found.
    real: Option<PathBuf>,
    places: CloudPlaces,
    /// Whether a folder holding a sync client's journal counts as synced by the client: no
    /// client's settings could be read.
    by_journal: bool,
    /// The folders looked into for a sync client's journal, by real path, and whether one
    /// may be there.
    journals: HashMap<PathBuf, bool>,
}

impl CloudFolders {
    /// Where cloud services keep files in the folder `root`, as far as can be told before
    /// looking inside it, with the sync clients' settings in `clients`.
    pub fn of(root: &Path, clients: &ClientSettings) -> Self {
        Self::with(root, Self::icloud_folders(), clients.synced_folders())
    }

    /// `of`, with iCloud Drive's folders on this computer, and the folders sync clients sync,
    /// by their real paths. `None`: they can't be found, so any folder may be one of iCloud
    /// Drive's, and any folder holding a sync client's journal is one a client syncs.
    fn with(root: &Path, icloud: Option<Vec<PathBuf>>, clients: Option<Vec<PathBuf>>) -> Self {
        let real = fs::canonicalize(root).ok();
        let mut folders = Self { real: real.clone(), places: CloudPlaces::default(), by_journal: clients.is_none(), journals: HashMap::new() };
        folders.add_listed(icloud, CloudService::ICloud);
        if clients.is_some() {
            folders.add_listed(clients, CloudService::SyncClient);
        } else if real.is_none_or(|real| real.ancestors().any(|folder| folders.holds_journal(folder))) {
            folders.places.add(String::new(), CloudService::SyncClient);
        }
        folders
    }

    /// Notes that `service` keeps files in the folders `listed` (by real path) that are the
    /// scanned folder, above it or inside it. `None`: any folder may be one.
    fn add_listed(&mut self, listed: Option<Vec<PathBuf>>, service: CloudService) {
        match (&self.real, listed) {
            (_, Some(listed)) if listed.is_empty() => {}
            (Some(real), Some(listed)) => {
                for place in listed {
                    if real.starts_with(&place) {
                        self.places.add(String::new(), service);
                    } else if let Some(inside) = place.strip_prefix(real).ok().and_then(Self::key_of) {
                        self.places.add(inside, service);
                    }
                }
            }
            _ => self.places.add(String::new(), service),
        }
    }

    /// Where cloud services keep files in the scanned folder, once every item in it was
    /// noted (see `note`).
    pub fn places(self) -> CloudPlaces {
        self.places
    }

    /// Notes an item the scan came to, at `disk` (relative, as spelled on disk): where no
    /// client's settings could be read, a sync client's journal means the client syncs the
    /// folder holding it.
    pub fn note(&mut self, disk: &str, is_file: bool) {
        if self.by_journal && is_file && Self::is_journal(Keys::name(disk)) {
            self.places.add(Keys::normalize(Keys::parent(disk).unwrap_or("")), CloudService::SyncClient);
        }
    }

    /// Whether the file at `disk` (relative, as spelled on disk), named like the placeholders
    /// of `service`, is one: the service keeps files in its folder. Where no client's
    /// settings could be read, a sync client's journal in a folder the scan hasn't come to
    /// yet counts too.
    pub fn is_placeholder(&mut self, disk: &str, service: CloudService) -> bool {
        let folder = Keys::parent(disk).unwrap_or("");
        if self.places.keep(&Keys::normalize(folder), service) {
            return true;
        }
        // The scanned folder itself, and the folders above it, were looked into already.
        let Some(real) = self.real.clone().filter(|_| self.by_journal && service == CloudService::SyncClient && !folder.is_empty()) else { return false };
        let parts: Vec<&str> = folder.split('/').collect();
        for depth in (1..=parts.len()).rev() {
            let path = parts[..depth].iter().fold(real.clone(), |path, part| path.join(part));
            if self.holds_journal(&path) {
                self.places.add(Keys::normalize(&parts[..depth].join("/")), CloudService::SyncClient);
                return true;
            }
        }
        false
    }

    /// Whether the folder at the real path `folder` holds a sync client's journal. A folder
    /// that can't be read may hold one.
    fn holds_journal(&mut self, folder: &Path) -> bool {
        if let Some(&holds) = self.journals.get(folder) {
            return holds;
        }
        let holds = match fs::read_dir(folder) {
            Ok(items) => items.flatten().any(|item| item.file_name().to_str().is_some_and(Self::is_journal) && item.file_type().is_ok_and(|kind| kind.is_file())),
            Err(e) => e.kind() != io::ErrorKind::NotFound,
        };
        self.journals.insert(folder.to_path_buf(), holds);
        holds
    }

    /// Whether `name` is the journal a Nextcloud or ownCloud client keeps at the top of the
    /// folder it syncs: `.sync_0123456789ab.db`, or `._sync_0123456789ab.db`.
    fn is_journal(name: &str) -> bool {
        let id = name.strip_prefix(".sync_").or_else(|| name.strip_prefix("._sync_")).and_then(|rest| rest.strip_suffix(".db"));
        id.is_some_and(|id| !id.is_empty() && id.bytes().all(|digit| digit.is_ascii_hexdigit()))
    }

    /// The key of a relative path, `None` if part of it isn't valid Unicode.
    fn key_of(path: &Path) -> Option<String> {
        let parts: Option<Vec<&str>> = path.components().map(|part| part.as_os_str().to_str()).collect();
        Some(Keys::normalize(&parts?.join("/")))
    }

    /// iCloud Drive's folders on this Mac, by their real paths: `~/Library/Mobile Documents`
    /// (iCloud Drive, and the folders of apps that keep their documents in iCloud), and the
    /// Desktop and Documents folders, which iCloud Drive can keep too.
    #[cfg(target_os = "macos")]
    fn icloud_folders() -> Option<Vec<PathBuf>> {
        let home = std::env::home_dir()?;
        let folders = ["Library/Mobile Documents", "Desktop", "Documents"].iter().map(|folder| home.join(folder));
        Some(folders.map(|folder| fs::canonicalize(&folder).unwrap_or(folder)).collect())
    }

    /// Only a Mac's iCloud Drive leaves placeholders named so.
    #[cfg(not(target_os = "macos"))]
    fn icloud_folders() -> Option<Vec<PathBuf>> {
        Some(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }

    /// Whether the file at `disk`, named like a placeholder, is one there.
    fn is_placeholder(folders: &mut CloudFolders, disk: &str) -> bool {
        let placeholder = Placeholder::named(Keys::name(disk)).expect("named like a placeholder");
        folders.is_placeholder(disk, placeholder.service)
    }

    #[test]
    fn a_cloud_services_placeholder_names_the_file_it_stands_for() {
        let named = |name| Placeholder::named(name).map(|placeholder| (placeholder.item, placeholder.service));
        assert_eq!(named(".Report.pdf.icloud"), Some(("Report.pdf", CloudService::ICloud)));
        assert_eq!(named("..env.icloud"), Some((".env", CloudService::ICloud)));
        assert_eq!(named("Plans.odt.nextcloud"), Some(("Plans.odt", CloudService::SyncClient)));
        assert_eq!(named("Budget.xlsx.owncloud"), Some(("Budget.xlsx", CloudService::SyncClient)));
        assert_eq!(named(".Notes.txt.nextcloud"), Some((".Notes.txt", CloudService::SyncClient)));
        assert_eq!(named("Report.pdf.icloud"), None, "iCloud's placeholders are hidden");
        assert_eq!(named(".icloud"), None);
        assert_eq!(named("..icloud"), None);
        assert_eq!(named("...icloud"), None);
        assert_eq!(named(".nextcloud"), None);
        assert_eq!(named("Report.pdf"), None);
    }

    /// iCloud Drive leaves its placeholders only in its own folders. A copy of one anywhere
    /// else (a backup made with a tool that copied the folder as it was) is an ordinary file.
    #[test]
    fn icloud_placeholders_are_only_in_icloud_drives_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(tmp.path()).unwrap();
        let (drive, backup) = (home.join("Library/Mobile Documents/com~apple~CloudDocs"), home.join("Backup"));
        fs::create_dir_all(&drive).unwrap();
        fs::create_dir_all(&backup).unwrap();
        let icloud = || Some(vec![home.join("Library/Mobile Documents"), home.join("Documents")]);

        let mut inside = CloudFolders::with(&drive, icloud(), None);
        assert!(is_placeholder(&mut inside, "Docs/.Report.pdf.icloud"));
        assert!(is_placeholder(&mut inside, ".Plans.key.icloud"));
        assert!(!is_placeholder(&mut inside, "Docs/Report.pdf.nextcloud"), "no sync client keeps files here");
        let mut elsewhere = CloudFolders::with(&backup, icloud(), None);
        assert!(!is_placeholder(&mut elsewhere, "Docs/.Report.pdf.icloud"));
        assert!(elsewhere.places().0.is_empty());
        let mut above = CloudFolders::with(&home, icloud(), None);
        assert!(is_placeholder(&mut above, "Library/Mobile Documents/com~apple~CloudDocs/.Report.pdf.icloud"));
        assert!(is_placeholder(&mut above, "Documents/Taxes/.2025.pdf.icloud"));
        assert!(!is_placeholder(&mut above, "Backup/.Report.pdf.icloud"));
        assert!(!is_placeholder(&mut above, ".Report.pdf.icloud"));
        assert_eq!(above.places().0, vec![("Library/Mobile Documents".to_string(), CloudService::ICloud), ("Documents".to_string(), CloudService::ICloud)]);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&drive, home.join("Shortcut")).unwrap();
            let mut through_a_link = CloudFolders::with(&home.join("Shortcut"), icloud(), None);
            assert!(is_placeholder(&mut through_a_link, ".Report.pdf.icloud"), "told by the real path");
        }
        // Where iCloud Drive's folders can't be found, any folder may be one.
        let mut unknown = CloudFolders::with(&backup, None, None);
        assert!(is_placeholder(&mut unknown, ".Report.pdf.icloud"));
        // Where there is no iCloud Drive, nothing is its placeholder.
        let mut none = CloudFolders::with(&drive, Some(Vec::new()), None);
        assert!(!is_placeholder(&mut none, ".Report.pdf.icloud"));
    }

    /// A Nextcloud or ownCloud client keeps its journal at the top of the folder it syncs:
    /// above the scanned folder, in it, or in a folder inside it, which the scan may come to
    /// after the placeholders in it. Where no client's settings can be read, that's how a
    /// folder a client syncs is told.
    #[test]
    fn where_no_clients_settings_can_be_read_sync_client_placeholders_are_in_folders_with_a_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let none = || Some(Vec::new());
        write(&tmp.path().join("Nextcloud/.sync_0123456789ab.db"));
        write(&tmp.path().join("Inside/ownCloud/._sync_ba9876543210.db"));
        fs::create_dir_all(tmp.path().join("Nextcloud/Docs")).unwrap();
        fs::create_dir_all(tmp.path().join("Inside/ownCloud/Docs")).unwrap();
        // Named like a journal, but a folder.
        fs::create_dir_all(tmp.path().join("Backup/Docs/.sync_0123456789ab.db")).unwrap();

        let mut below = CloudFolders::with(&tmp.path().join("Nextcloud/Docs"), none(), None);
        assert!(is_placeholder(&mut below, "Plans.odt.nextcloud"));
        assert!(is_placeholder(&mut below, "Old/Budget.xlsx.owncloud"));
        assert_eq!(below.places().0, vec![(String::new(), CloudService::SyncClient)]);
        let mut copy = CloudFolders::with(&tmp.path().join("Backup"), none(), None);
        assert!(!is_placeholder(&mut copy, "Docs/Plans.odt.nextcloud"));
        assert!(copy.places().0.is_empty());

        let mut above = CloudFolders::with(&tmp.path().join("Inside"), none(), None);
        assert!(is_placeholder(&mut above, "ownCloud/Docs/Plans.odt.owncloud"), "before the scan comes to the journal");
        assert!(!is_placeholder(&mut above, "Plans.odt.owncloud"));
        above.note("ownCloud/._sync_ba9876543210.db", true);
        assert_eq!(above.places().0, vec![("ownCloud".to_string(), CloudService::SyncClient)]);
        let mut noted = CloudFolders::with(&tmp.path().join("Inside"), none(), None);
        noted.note("ownCloud/._sync_ba9876543210.db", true);
        noted.note("Other/.sync_.db", true);
        noted.note("Other/.sync_0123456789ab.db", false);
        noted.note("Other/.sync_notanumber.db", true);
        assert_eq!(noted.places().0, vec![("ownCloud".to_string(), CloudService::SyncClient)]);
    }

    /// Where the clients' settings can be read, they tell which folders a client syncs: at,
    /// above or inside the scanned folder, by real path. A journal anywhere else is a copy
    /// (a backup made by a tool that copied a synced folder as it was) and proves nothing.
    #[test]
    fn sync_client_placeholders_are_only_in_the_folders_the_clients_settings_list() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = fs::canonicalize(tmp.path()).unwrap();
        let none = || Some(Vec::new());
        let clients = || Some(vec![tmp.join("Nextcloud")]);
        write(&tmp.join("Nextcloud/.sync_0123456789ab.db"));
        write(&tmp.join("Backup/.sync_0123456789ab.db"));
        write(&tmp.join("Backup/Docs/._sync_ba9876543210.db"));
        fs::create_dir_all(tmp.join("Nextcloud/Docs")).unwrap();

        let mut below = CloudFolders::with(&tmp.join("Nextcloud/Docs"), none(), clients());
        assert!(is_placeholder(&mut below, "Plans.odt.nextcloud"));
        assert_eq!(below.places().0, vec![(String::new(), CloudService::SyncClient)]);
        let mut copy = CloudFolders::with(&tmp.join("Backup"), none(), clients());
        assert!(!is_placeholder(&mut copy, "Plans.odt.nextcloud"));
        assert!(!is_placeholder(&mut copy, "Docs/Plans.odt.nextcloud"));
        copy.note(".sync_0123456789ab.db", true);
        copy.note("Docs/._sync_ba9876543210.db", true);
        assert!(copy.places().0.is_empty());
        let mut above = CloudFolders::with(&tmp, none(), clients());
        assert!(is_placeholder(&mut above, "Nextcloud/Docs/Plans.odt.nextcloud"));
        assert!(!is_placeholder(&mut above, "Backup/Docs/Plans.odt.nextcloud"));
        assert_eq!(above.places().0, vec![("Nextcloud".to_string(), CloudService::SyncClient)]);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(tmp.join("Nextcloud"), tmp.join("Shortcut")).unwrap();
            let mut through_a_link = CloudFolders::with(&tmp.join("Shortcut"), none(), clients());
            assert!(is_placeholder(&mut through_a_link, "Plans.odt.nextcloud"), "told by the real path");
        }
        // Settings that list only another folder: no client syncs this one.
        let mut elsewhere = CloudFolders::with(&tmp.join("Nextcloud/Docs"), none(), Some(vec![tmp.join("Backup/Docs")]));
        assert!(!is_placeholder(&mut elsewhere, "Plans.odt.nextcloud"));
        // A folder whose real path can't be found may be one.
        let mut unknown = CloudFolders::with(&tmp.join("Missing"), none(), clients());
        assert!(is_placeholder(&mut unknown, "Plans.odt.nextcloud"));
    }

    /// A sync client's journal and the files that go with it are told by their names.
    #[test]
    fn a_sync_clients_journal_is_told_by_its_name() {
        let journals = [
            ".sync_0123456789ab.db",
            "._sync_BA9876543210.db",
            ".csync_journal.db",
            ".sync_0123456789ab.db-wal",
            ".sync_0123456789ab.db-shm",
            "._sync_ba9876543210.db-journal",
            ".csync_journal.db-journal",
        ];
        for name in journals {
            assert!(is_sync_journal(name), "{name}");
        }
        let others = [
            ".sync_.db",
            ".sync_notes.db",
            ".sync_0123456789ab.db-wal-shm",
            "sync_0123456789ab.db",
            ".sync_0123456789ab.db.bak",
            "notes.db-wal",
            ".csync_journal.db-wal-shm",
        ];
        for name in others {
            assert!(!is_sync_journal(name), "{name}");
        }
    }

    /// Above the scanned folder, a folder that can't be read may hold a sync client's
    /// journal: what's named like its placeholders is taken for them.
    #[cfg(unix)]
    #[test]
    fn a_folder_above_that_cant_be_read_may_be_synced_by_a_client() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (locked, root) = (tmp.path().join("locked"), tmp.path().join("locked/job"));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o111)).unwrap();
        let readable = fs::read_dir(&locked).is_ok(); // root ignores permissions
        let mut folders = CloudFolders::with(&root, Some(Vec::new()), None);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return;
        }
        assert!(is_placeholder(&mut folders, "Plans.odt.nextcloud"));
    }
}
