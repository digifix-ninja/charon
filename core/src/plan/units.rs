//! Files that only make sense together, and must never be mixed from two versions.
//!
//! * An app's library or document package: a Photos or Final Cut library, a Logic
//!   project, a Pages document, a disk image in bands… On macOS it looks like one file,
//!   but it's a folder of many, written together by one app.
//! * An SQLite database and its journal files (`name-wal`, `name-shm`, `name-journal`):
//!   the journal holds changes that only fit the database it was written for.
//!
//! When such a unit changed on both sides of a two-way job, syncing some of its files one
//! way and others the other way would leave a mix that the app may not open, or would
//! quietly damage. So the whole unit becomes one conflict, and the person chooses whose
//! version to keep as a whole (see `plan::Resolution`).

use crate::scan::Keys;

/// The endings of folders that hold one app's library or document.
const PACKAGES: &[&str] = &[
    "app",
    "aplibrary",
    "band",
    "dtbase2",
    "fcpbundle",
    "imovielibrary",
    "key",
    "logicx",
    "lrlibrary",
    "musiclibrary",
    "numbers",
    "pages",
    "photolibrary",
    "photoslibrary",
    "rtfd",
    "scriv",
    "sparsebundle",
    "tvlibrary",
];

/// The journal files SQLite keeps next to a database.
const JOURNALS: &[&str] = &["-wal", "-shm", "-journal"];

/// A group of files that only make sense together, named by its key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unit {
    /// An app's library or document package: the key of its folder.
    Package(String),
    /// An SQLite database: the key of the database file (its journals are named after it).
    Database(String),
}

impl Unit {
    /// The unit `key` belongs to, if any. `exists` tells whether a key is there on either side
    /// (a database is only one with a journal next to it).
    pub fn of(key: &str, exists: impl Fn(&str) -> bool) -> Option<Unit> {
        if let Some(package) = Self::package_of(key) {
            return Some(Unit::Package(package.to_string()));
        }
        Self::database_of(key, exists).map(Unit::Database)
    }

    /// The database `key` is a file of (the database itself or one of its journals), even
    /// inside an app library: the key of the database file. A database is only one with a
    /// journal next to it, and a journal only one next to its database (a note called
    /// "travel-journal" alone is an ordinary file).
    pub fn database_of(key: &str, exists: impl Fn(&str) -> bool) -> Option<String> {
        let name = key.rsplit('/').next().unwrap_or(key);
        for journal in JOURNALS {
            if name.len() > journal.len() && name.ends_with(journal) {
                let database = &key[..key.len() - journal.len()];
                return exists(database).then(|| database.to_string());
            }
        }
        JOURNALS.iter().any(|journal| exists(&format!("{key}{journal}"))).then(|| key.to_string())
    }

    /// The key that stands for the whole unit (the package folder, or the database file).
    pub fn key(&self) -> &str {
        match self {
            Unit::Package(key) | Unit::Database(key) => key,
        }
    }

    /// Whether `key` is part of this unit.
    pub fn holds(&self, key: &str) -> bool {
        match self {
            Unit::Package(folder) => Keys::is_within(key, folder),
            Unit::Database(database) => key == database || JOURNALS.iter().any(|journal| key.strip_prefix(database.as_str()) == Some(journal)),
        }
    }

    /// The keys of a database's files (the database and its journals).
    pub fn database_files(database: &str) -> Vec<String> {
        std::iter::once(database.to_string()).chain(JOURNALS.iter().map(|journal| format!("{database}{journal}"))).collect()
    }

    /// "an app library" or "a database", for messages.
    pub fn describe(&self) -> &'static str {
        match self {
            Unit::Package(_) => "an app library or document",
            Unit::Database(_) => "a database",
        }
    }

    /// The topmost folder above or at `key` that is a package.
    fn package_of(key: &str) -> Option<&str> {
        let mut end = 0;
        for part in key.split('/') {
            end += part.len();
            if Self::is_package_name(part) {
                return Some(&key[..end]);
            }
            end += 1; // the slash
        }
        None
    }

    fn is_package_name(name: &str) -> bool {
        match name.rsplit_once('.') {
            Some((stem, ending)) if !stem.is_empty() => PACKAGES.iter().any(|package| ending.eq_ignore_ascii_case(package)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn files_inside_a_library_belong_to_it() {
        let library = Some(Unit::Package("Pictures/Photos Library.photoslibrary".into()));
        assert_eq!(Unit::of("Pictures/Photos Library.photoslibrary", none), library);
        assert_eq!(Unit::of("Pictures/Photos Library.photoslibrary/database/Photos.sqlite-wal", none), library);
        assert_eq!(Unit::of("Docs/Report.PAGES/Index.zip", none), Some(Unit::Package("Docs/Report.PAGES".into())));
        assert_eq!(Unit::of("Pictures/cat.jpg", none), None);
        assert_eq!(Unit::of("notes/.pages", none), None, "a name that is only an ending isn't a package");
    }

    #[test]
    fn a_database_and_its_journals_belong_together() {
        let database = Some(Unit::Database("app/data.db".into()));
        let there = |key: &str| ["app/data.db", "app/data.db-wal"].contains(&key);
        assert_eq!(Unit::of("app/data.db-wal", there), database);
        assert_eq!(Unit::of("app/data.db-shm", there), database);
        assert_eq!(Unit::of("app/data.db-journal", there), database);
        assert_eq!(Unit::of("app/data.db", there), database);
        assert_eq!(Unit::of("app/data.db", none), None, "without a journal it's an ordinary file");
        assert_eq!(Unit::of("notes/travel-journal", none), None, "without its database it's an ordinary file");
        assert_eq!(
            Unit::database_of("Lib.photoslibrary/database/Photos.sqlite-wal", |key| key.ends_with("Photos.sqlite")),
            Some("Lib.photoslibrary/database/Photos.sqlite".into()),
            "inside a library too"
        );
        let unit = Unit::Database("app/data.db".into());
        assert!(unit.holds("app/data.db-wal") && unit.holds("app/data.db") && !unit.holds("app/data.db2"));
    }
}
