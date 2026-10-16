//! Counts finished items and bytes for the progress bar.

use crate::model::{Phase, Progress};
use crate::plan::Plan;

/// Keeps the progress of a run and hands a copy to the observer after each step.
pub struct ProgressMeter<'a> {
    progress: Progress,
    observer: &'a dyn Fn(&Progress),
}

impl<'a> ProgressMeter<'a> {
    /// A meter sized for `plan`.
    pub fn for_plan(plan: &Plan, observer: &'a dyn Fn(&Progress)) -> Self {
        let work: Vec<_> = plan.work().collect();
        Self {
            progress: Progress {
                phase: Phase::Copying,
                done_items: 0,
                total_items: work.len() as u64,
                done_bytes: 0,
                total_bytes: work.iter().map(|a| a.bytes()).sum(),
                current: None,
            },
            observer,
        }
    }

    /// The item now being worked on.
    pub fn start(&mut self, path: &str) {
        self.progress.current = Some(path.to_string());
        (self.observer)(&self.progress);
    }

    /// More bytes carried (called during long copies).
    pub fn add_bytes(&mut self, bytes: u64) {
        self.progress.done_bytes += bytes;
        (self.observer)(&self.progress);
    }

    /// One more item done.
    pub fn finish_item(&mut self) {
        self.progress.done_items += 1;
        (self.observer)(&self.progress);
    }

    /// Everything done; tidying up.
    pub fn finishing(&mut self) {
        self.progress.phase = Phase::Finishing;
        self.progress.current = None;
        (self.observer)(&self.progress);
    }
}
