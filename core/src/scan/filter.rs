//! Exclude patterns: which items Charon never looks at, copies or deletes.

use crate::error::{Error, Result};
use crate::scan::Keys;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Charon's own safe-delete folder, at the root of each side.
pub const TRASH_FOLDER: &str = ".charon-trash";
/// Charon's own small folder at the root of each side: it holds the marker that tells
/// Charon it's looking at the same folder as last time.
pub const MARKER_FOLDER: &str = ".charon";
/// What Charon 0.2 and earlier added to files being copied (`report.pdf.charon-partial`).
pub const PARTIAL_SUFFIX: &str = ".charon-partial";
/// Files being copied are written as `.charon-<16 hex digits>.part` next to their real
/// name, then renamed once complete. The name is short, so a long name never gets too long.
pub const PARTIAL_PREFIX: &str = ".charon-";
pub const PARTIAL_EXTENSION: &str = ".part";

/// Whether `name` is a file Charon was copying (now or in an interrupted run).
pub fn is_partial(name: &str) -> bool {
    if name.ends_with(PARTIAL_SUFFIX) {
        return true;
    }
    let digits = name.strip_prefix(PARTIAL_PREFIX).and_then(|rest| rest.strip_suffix(PARTIAL_EXTENSION));
    digits.is_some_and(|d| d.len() == 16 && d.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Files the operating system scatters into folders by itself (Finder and Explorer
/// settings, thumbnail caches, custom folder icons, macOS metadata on non-Mac drives).
/// They never keep a folder from being removed: when everything else in a folder is gone,
/// they go along with it. Your own exclude patterns are never treated like this.
pub fn is_clutter(name: &str) -> bool {
    name == "Icon\r"
        || name.starts_with("._")
        || [".ds_store", "thumbs.db", "ehthumbs.db", "desktop.ini"].contains(&name.to_lowercase().as_str())
}

/// Folders a drive keeps for itself at its top level (recycle bins, indexes, repair
/// folders). At the top of a job's folder they are never looked at.
pub fn is_system_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "system volume information",
        "$recycle.bin",
        ".trashes",
        ".spotlight-v100",
        ".fseventsd",
        ".temporaryitems",
        ".documentrevisions-v100",
        ".pkinstallsandboxmanager",
        ".pkinstallsandboxmanager-systemsoftware",
        "lost+found",
    ]
    .contains(&lower.as_str())
        || lower.starts_with(".trash-")
}

/// Patterns without a `/` match an item's name anywhere in the tree (`node_modules`,
/// `*.tmp`); patterns with a `/` match its path from the folder's root (`build/cache`). A
/// pattern starting with `/` only matches from the root (`/Photos Library`).
#[derive(Debug, Clone)]
pub struct ExcludeFilter {
    names: GlobSet,
    paths: GlobSet,
}

impl ExcludeFilter {
    /// Builds a filter from the job's patterns. Charon's own files are always excluded.
    /// `ignore_case`: one of the drives ignores upper and lower case, so patterns do too.
    pub fn new(patterns: &[String], ignore_case: bool) -> Result<Self> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        for pattern in patterns {
            let pattern = pattern.trim();
            if pattern.is_empty() {
                continue;
            }
            let anchored = pattern.starts_with('/');
            let body = Keys::normalize(pattern.trim_matches('/'));
            if body.is_empty() {
                continue;
            }
            let glob = GlobBuilder::new(&body)
                .case_insensitive(ignore_case)
                .build()
                .map_err(|_| Error::Pattern(pattern.into()))?;
            if anchored || body.contains('/') {
                paths.add(glob);
            } else {
                names.add(glob);
            }
        }
        Ok(Self {
            names: names.build().map_err(|e| Error::Pattern(e.to_string()))?,
            paths: paths.build().map_err(|e| Error::Pattern(e.to_string()))?,
        })
    }

    /// A filter that only hides Charon's own files.
    pub fn none() -> Self {
        Self::new(&[], false).expect("an empty filter is always valid")
    }

    /// The pattern that leaves out exactly the item at `path` (relative, `/`-separated).
    pub fn pattern_for(path: &str) -> String {
        format!("/{}", globset::escape(path.trim_matches('/')))
    }

    /// Whether the item at `path` (relative, `/`-separated) is left alone.
    pub fn excludes(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        Self::is_internal(path, name) || self.names.is_match(name) || self.paths.is_match(path)
    }

    /// Charon's own folders and half-copied files, at any depth (a folder of another job
    /// can sit inside this one, with its own trash and marker).
    pub fn is_internal(path: &str, name: &str) -> bool {
        is_partial(name) || path.split('/').any(|part| part == TRASH_FOLDER || part == MARKER_FOLDER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(patterns: &[&str]) -> ExcludeFilter {
        ExcludeFilter::new(&patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>(), false).unwrap()
    }

    #[test]
    fn names_match_anywhere() {
        let f = filter(&["node_modules", "*.tmp", ".DS_Store"]);
        assert!(f.excludes("node_modules"));
        assert!(f.excludes("app/node_modules"));
        assert!(f.excludes("a/b/c.tmp"));
        assert!(f.excludes("photos/.DS_Store"));
        assert!(!f.excludes("photos/cat.jpg"));
    }

    #[test]
    fn paths_match_from_root() {
        let f = filter(&["build/cache"]);
        assert!(f.excludes("build/cache"));
        assert!(!f.excludes("src/build/cache"));
    }

    #[test]
    fn clutter_is_only_the_system_list() {
        assert!(is_clutter(".DS_Store"));
        assert!(is_clutter("._photo.jpg"));
        assert!(is_clutter("Icon\r"));
        assert!(!is_clutter("notes.tmp"));
        assert!(!is_clutter("node_modules"));
        assert!(is_clutter("thumbs.db") && is_clutter(".ds_store"), "drives that ignore case may spell them so");
    }

    #[test]
    fn a_leading_slash_matches_only_from_the_root() {
        let f = filter(&["/Photos Library"]);
        assert!(f.excludes("Photos Library"));
        assert!(!f.excludes("old/Photos Library"));
        let exact = ExcludeFilter::new(&[ExcludeFilter::pattern_for("Scans [2019]/raw")], false).unwrap();
        assert!(exact.excludes("Scans [2019]/raw"));
        assert!(!exact.excludes("Scans 2/raw"));
    }

    #[test]
    fn patterns_ignore_case_when_a_drive_does() {
        let f = ExcludeFilter::new(&["*.TMP".to_string(), "Build/Cache".to_string()], true).unwrap();
        assert!(f.excludes("a/notes.tmp"));
        assert!(f.excludes("build/cache"));
        assert!(!filter(&["*.TMP"]).excludes("a/notes.tmp"));
    }

    #[test]
    fn drive_folders_are_known() {
        assert!(is_system_folder("System Volume Information"));
        assert!(is_system_folder("$RECYCLE.BIN"));
        assert!(is_system_folder(".Trash-1000"));
        assert!(!is_system_folder("Photos"));
    }

    #[test]
    fn own_files_are_always_hidden() {
        let f = ExcludeFilter::none();
        assert!(f.excludes(".charon-trash"));
        assert!(f.excludes(".charon-trash/2026/a.txt"));
        assert!(f.excludes("nested/job/.charon-trash/2026/a.txt"), "another job's trash inside this folder");
        assert!(f.excludes(".charon/marker.json"));
        assert!(f.excludes("docs/report.pdf.charon-partial"));
        assert!(f.excludes("docs/.charon-0123456789abcdef.part"));
        assert!(!f.excludes("docs/.charon-notes.part"));
        assert!(!f.excludes("docs/.charon-trash-notes.txt"));
        assert!(!f.excludes("docs/.charon.txt"));
    }
}
