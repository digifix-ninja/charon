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

    #[error("This job is already running.")]
    Busy,

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
