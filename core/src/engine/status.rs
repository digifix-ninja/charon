//! Turning a stored job and its live state into the row the window shows.

use crate::autopilot::Schedule;
use crate::engine::Engine;
use crate::model::{Job, JobStatus, JobView, Settings};

impl Engine {
    pub(crate) fn view_of(&self, job: Job, settings: &Settings) -> JobView {
        let (running, progress, pending, conflicts, problem) = {
            let runtime = self.runtime();
            match runtime.get(&job.id) {
                Some(rt) => (rt.is_running(), rt.progress.clone(), rt.pending.clone(), rt.conflicts.clone(), rt.problem.clone()),
                None => (false, None, None, Vec::new(), None),
            }
        };
        let source_online = job.setup.source.is_dir();
        let destination_online = job.setup.destination.is_dir();
        let paused = job.paused || settings.paused_all;

        let status = if running {
            JobStatus::Syncing
        } else if !source_online || !destination_online {
            JobStatus::Offline
        } else if paused {
            JobStatus::Paused
        } else if !conflicts.is_empty() {
            JobStatus::Conflict
        } else if problem.is_some() || job.last_run.as_ref().is_some_and(|r| !r.is_clean() && !r.cancelled) {
            JobStatus::Error
        } else if job.last_synced.is_none() {
            JobStatus::New
        } else if pending.as_ref().is_some_and(|p| p.items > 0) {
            JobStatus::Pending
        } else {
            JobStatus::InSync
        };

        let problem = match status {
            JobStatus::Offline if !source_online => Some("The source folder isn't reachable.".into()),
            JobStatus::Offline => Some("The destination isn't reachable. Is the drive connected?".into()),
            _ => problem.or_else(|| job.last_run.as_ref().and_then(|r| r.error.clone())),
        };
        let next_run = (!paused).then(|| Schedule::next_run(&job)).flatten();

        JobView {
            job,
            status,
            pending,
            progress: if running { progress } else { None },
            conflicts,
            problem,
            source_online,
            destination_online,
            next_run,
        }
    }
}
