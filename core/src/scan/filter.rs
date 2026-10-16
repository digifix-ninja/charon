//! Exclude patterns: which items Charon never looks at, copies or deletes.

use crate::error::{Error, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

/// Charon's own safe-delete folder, at the root of each side.
pub const TRASH_FOLDER: &str = ".charon-trash";
/// Suffix of files being copied; renamed to their real name once complete.
pub const PARTIAL_SUFFIX: &str = ".charon-partial";

/// Patterns without a `/` match an item's name anywhere in the tree (`node_modules`,
/// `*.tmp`); patterns with a `/` match its path from the folder's root (`build/cache`).
#[derive(Debug, Clone)]
pub struct ExcludeFilter {
    names: GlobSet,
    paths: GlobSet,
}

impl ExcludeFilter {
    /// Builds a filter from the job's patterns. Charon's own files are always excluded.
    pub fn new(patterns: &[String]) -> Result<Self> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        for pattern in patterns {
            let pattern = pattern.trim();
            if pattern.is_empty() {
                continue;
            }
            let glob = Glob::new(pattern.trim_matches('/')).map_err(|_| Error::Pattern(pattern.into()))?;
            if pattern.trim_matches('/').contains('/') {
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
        Self::new(&[]).expect("an empty filter is always valid")
    }

    /// Whether the item at `path` (relative, `/`-separated) is left alone.
    pub fn excludes(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        Self::is_internal(path, name) || self.names.is_match(name) || self.paths.is_match(path)
    }

    /// Charon's trash folder and half-copied files.
    fn is_internal(path: &str, name: &str) -> bool {
        name.ends_with(PARTIAL_SUFFIX) || path == TRASH_FOLDER || path.starts_with(&format!("{TRASH_FOLDER}/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(patterns: &[&str]) -> ExcludeFilter {
        ExcludeFilter::new(&patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>()).unwrap()
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
    fn own_files_are_always_hidden() {
        let f = ExcludeFilter::none();
        assert!(f.excludes(".charon-trash"));
        assert!(f.excludes(".charon-trash/2026/a.txt"));
        assert!(f.excludes("docs/report.pdf.charon-partial"));
        assert!(!f.excludes("docs/.charon-trash-notes.txt"));
    }
}
