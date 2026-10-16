//! The live state of a job: everything that isn't saved to disk.

use crate::model::{ConflictItem, Pending, Progress};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Default)]
pub(crate) struct JobRuntime {
    /// Set while a sync runs; setting it to true stops the run.
    pub cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<Progress>,
    /// What the last check found waiting.
    pub pending: Option<Pending>,
    /// Two-way conflicts waiting for a decision.
    pub conflicts: Vec<ConflictItem>,
    /// What went wrong in the last run or check.
    pub problem: Option<String>,
    /// A check for pending changes is running.
    pub checking: bool,
    pub last_checked: Option<Instant>,
    pub finished_at: Option<Instant>,
    pub last_progress_sent: Option<Instant>,
}

impl JobRuntime {
    pub fn is_running(&self) -> bool {
        self.cancel.is_some()
    }
}
