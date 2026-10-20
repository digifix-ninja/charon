//! The one error type of the core. Messages are written for people, because the window
//! shows them as they are.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("Couldn't read or write {path}: {source}")]
    File {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("A Charon file is damaged: {0}")]
    Json(#[from] serde_json::Error),

    #[error("“{0}” isn't a valid exclude pattern.")]
    Pattern(String),

    #[error("{0}")]
    Invalid(String),

    #[error("This sync job no longer exists.")]
    UnknownJob(String),

    #[error("{} isn't reachable. Is the drive connected?", .0.display())]
    Offline(PathBuf),

    #[error("Charon can't read {}: {reason}. On a Mac, check System Settings → Privacy & Security → Files and Folders.", .path.display())]
    Unreadable { path: PathBuf, reason: String },

    #[error("Charon is already running. Only one Charon may work with your jobs at a time.")]
    AlreadyRunning,

    #[error("This job is syncing right now.")]
    Busy,

    /// Another running job (named) works with one of the same folders.
    #[error("“{0}” is syncing one of these folders right now. Look again when it's done.")]
    FoldersBusy(String),

    /// The list the person looked at no longer matches the folders or the job.
    #[error("{0}")]
    ReviewOutdated(String),

    /// An item changed between the scan (or the review) and the moment Charon got to it.
    #[error("{0}")]
    Changed(String),

    /// The drive hasn't enough room left for a step (see `run::Room`).
    #[error("{0}")]
    NoRoom(String),

    /// The drive can't hold symbolic links (FAT, exFAT, some network shares).
    #[error("this drive can't hold symbolic links")]
    NoLinks,

    /// Something written during a run landed on another drive than the folder's: the drive
    /// went away and left its mount folder behind. The run stops there.
    #[error("{0}")]
    LandedElsewhere(String),

    #[error("Stopped.")]
    Cancelled,
}

impl Error {
    /// Wraps an I/O error with the path it happened on.
    pub fn file(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::File {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
