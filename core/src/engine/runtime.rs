//! The live state of a job: everything that isn't saved to disk.

use crate::engine::review::StoredReview;
use crate::model::{ConflictItem, JobSetup, PerSide, Place, Progress, VolumeLabel};
use crate::scan::{Entry, ExcludeFilter};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub(crate) struct JobRuntime {
    /// Set while a sync runs; setting it to true stops the run.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The person pressed Stop on the running sync (not Charon quitting).
    pub stopped_by_person: bool,
    /// Runs nobody looks at are refused until the person confirms a list (they stopped a
    /// sync, or a run was held back). Kept here too, in case the job's record couldn't be saved.
    pub must_ask: bool,
    /// A run nobody looks at is due (full automation), and why. It starts as soon as nothing
    /// stands in its way, so a trigger is never lost while another job uses the folders.
    pub wanted: Option<String>,
    /// The running job whose folders the wanted run waits for.
    pub waiting_for: Option<String>,
    /// Set while a review is being prepared; setting it to true stops that.
    pub preparing: Option<Arc<AtomicBool>>,
    /// Set while a background check runs; setting it to true stops it.
    pub checking: Option<Arc<AtomicBool>>,
    pub progress: Option<Progress>,
    /// What the last look found.
    pub look: Option<Look>,
    /// The list waiting for the person's OK.
    pub review: Option<Arc<StoredReview>>,
    /// What went wrong in the last look or run.
    pub problem: Option<String>,
    pub last_checked: Option<Instant>,
    pub finished_at: Option<Instant>,
    pub last_progress_sent: Option<Instant>,
    pub reminder: Reminder,
    /// Pairs of files compared byte for byte: key → both items as compared, and whether
    /// they were identical. Reused while neither file changes.
    pub same_contents: HashMap<String, (Entry, Entry, bool)>,
    /// The drive of the source / destination turned out not to hold symbolic links.
    pub no_links: (bool, bool),
    /// The job's exclude filter, to tell which folder changes matter (patterns it was
    /// built from, and the filter).
    pub filter: Option<(Vec<String>, ExcludeFilter)>,
}

impl JobRuntime {
    pub fn is_running(&self) -> bool {
        self.cancel.is_some()
    }

    pub fn is_checking(&self) -> bool {
        self.checking.is_some()
    }

    /// Stops a running check or review preparation (the job changed under them).
    pub fn stop_looking(&mut self) {
        for flag in [self.checking.as_ref(), self.preparing.as_ref()].into_iter().flatten() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Forgets everything learned about the folders (the job's folders or mode changed).
    pub fn forget_folders(&mut self) {
        self.stop_looking();
        self.look = None;
        self.review = None;
        self.problem = None;
        self.same_contents.clear();
        self.no_links = (false, false);
        self.filter = None;
        self.wanted = None;
        self.waiting_for = None;
    }
}

/// What a look at both folders found (a check, a review, or a run's scan).
#[derive(Debug, Clone)]
pub(crate) struct Look {
    /// The job's setup the look was made with.
    pub setup: JobSetup,
    pub at: DateTime<Utc>,
    /// Steps that change something.
    pub items: u64,
    pub bytes: u64,
    pub removals: PerSide,
    pub replacements: PerSide,
    pub conflicts: Vec<ConflictItem>,
    /// At most `PLACES_SHOWN` of each.
    pub unreadable: Vec<Place>,
    pub not_synced: Vec<Place>,
    pub not_synced_count: u64,
    pub volumes: Option<(VolumeLabel, VolumeLabel)>,
}

impl Look {
    /// Whether the look found anything waiting for the person: changes or conflicts.
    pub fn has_work(&self) -> bool {
        self.items > 0 || !self.conflicts.is_empty()
    }
}

/// When the person was last told that changes wait for their OK.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Reminder {
    /// The last notification about this job (kept when the changes are confirmed, so a new
    /// wait right after doesn't notify again at once).
    pub last: Option<Instant>,
    /// Notifications sent since changes began to wait.
    pub count: u32,
}
