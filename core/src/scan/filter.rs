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
    partial_digits(name).is_some()
}

/// When the copy written under the temporary name `name` was started, in milliseconds since
/// 1970: its first 8 hex digits count the seconds (see `Copier::partial_path`). `None` for
/// other names, and for the temporary names of earlier versions, whose digits were all
/// random (a time that isn't within the last ten years is taken for one of those).
pub fn partial_started(name: &str, now_ms: i64) -> Option<i64> {
    const TEN_YEARS_MS: i64 = 10 * 366 * 24 * 3_600_000;
    let seconds = i64::from_str_radix(&partial_digits(name)?[..8], 16).ok()?;
    let started = seconds * 1000;
    (now_ms - TEN_YEARS_MS..=now_ms + 24 * 3_600_000).contains(&started).then_some(started)
}

/// The 16 hex digits of a temporary name `.charon-0123456789abcdef.part`.
fn partial_digits(name: &str) -> Option<&str> {
    let digits = name.strip_prefix(PARTIAL_PREFIX).and_then(|rest| rest.strip_suffix(PARTIAL_EXTENSION))?;
    (digits.len() == 16 && digits.bytes().all(|b| b.is_ascii_hexdigit())).then_some(digits)
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
        Self::is_internal(path, name) || Self::is_apple_double(name) || self.names.is_match(name) || self.paths.is_match(path)
    }

    /// On a Mac, `._report.pdf` next to `report.pdf` on a drive that isn't a Mac drive
    /// (exFAT, FAT, many network shares) holds the details of `report.pdf` that drive can't
    /// keep itself: its Finder tags, its resource fork. macOS reads and writes it along with
    /// the file. Copied or removed as a file of its own, it would overwrite or strip the
    /// details of another file, so on a Mac it's never synced, whatever the patterns say.
    /// It goes along with its folder when everything else in it is removed.
    fn is_apple_double(name: &str) -> bool {
        cfg!(target_os = "macos") && name.len() > 2 && name.starts_with("._")
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
    fn a_temporary_name_tells_when_its_copy_started() {
        let now = 1_760_000_000_000;
        let name = format!(".charon-{:08x}{:08x}.part", now / 1000 - 60, 0xabcdefu32);
        assert!(is_partial(&name));
        assert_eq!(partial_started(&name, now), Some(now - 60_000));
        assert_eq!(partial_started(".charon-ffffffff00000000.part", now), None, "random digits of an older version");
        assert_eq!(partial_started(".charon-0000000100000000.part", now), None);
        assert_eq!(partial_started("report.pdf", now), None);
    }

    #[test]
    fn apple_double_files_are_left_alone_on_a_mac() {
        let f = filter(&[]);
        assert_eq!(f.excludes("photos/._cat.jpg"), cfg!(target_os = "macos"));
        assert!(!f.excludes("photos/._"));
        assert!(!f.excludes("photos/_cat.jpg"));
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
