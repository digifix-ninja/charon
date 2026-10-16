//! One plain-text activity log per job: what each run did and which files failed.

use crate::model::JobId;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

/// Logs are trimmed to their newest half when they grow beyond this.
const MAX_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn word(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    fn parse(word: &str) -> Self {
        match word {
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }
}

/// One line of a log, as the window shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub time: DateTime<Utc>,
    pub level: Level,
    pub text: String,
}

pub struct Journal {
    folder: PathBuf,
}

impl Journal {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }

    pub fn info(&self, job: &JobId, text: &str) {
        self.write(job, Level::Info, text);
    }

    pub fn warn(&self, job: &JobId, text: &str) {
        self.write(job, Level::Warn, text);
    }

    pub fn error(&self, job: &JobId, text: &str) {
        self.write(job, Level::Error, text);
    }

    /// The newest `limit` lines, newest first.
    pub fn read(&self, job: &JobId, limit: usize) -> Vec<LogLine> {
        let Ok(text) = fs::read_to_string(self.file(job)) else { return Vec::new() };
        text.lines().rev().filter_map(Self::parse_line).take(limit).collect()
    }

    pub fn forget(&self, job: &JobId) {
        let _ = fs::remove_file(self.file(job));
    }

    fn write(&self, job: &JobId, level: Level, text: &str) {
        let path = self.file(job);
        if let Err(e) = fs::create_dir_all(&self.folder) {
            log::warn!("couldn't create the log folder: {e}");
            return;
        }
        self.trim(&path);
        let line = format!("{}\t{}\t{}\n", Utc::now().to_rfc3339(), level.word(), text.replace('\n', " "));
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(e) = written {
            log::warn!("couldn't write to {}: {e}", path.display());
        }
    }

    /// Keeps the newest half of a log that grew too long.
    fn trim(&self, path: &PathBuf) {
        let too_long = fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false);
        if !too_long {
            return;
        }
        if let Ok(text) = fs::read_to_string(path) {
            let lines: Vec<&str> = text.lines().collect();
            let kept = lines[lines.len() / 2..].join("\n") + "\n";
            let _ = fs::write(path, kept);
        }
    }

    fn parse_line(line: &str) -> Option<LogLine> {
        let mut parts = line.splitn(3, '\t');
        let time = DateTime::parse_from_rfc3339(parts.next()?).ok()?.with_timezone(&Utc);
        let level = Level::parse(parts.next()?);
        Some(LogLine { time, level, text: parts.next()?.to_string() })
    }

    fn file(&self, job: &JobId) -> PathBuf {
        self.folder.join(format!("{job}.log"))
    }
}
