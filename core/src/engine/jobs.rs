//! Adding, editing, pausing and removing jobs.

use crate::engine::{Engine, EngineEvent};
use crate::error::Result;
use crate::model::{Job, JobId, JobSetup, JobView};
use crate::scan::ExcludeFilter;

impl Engine {
    /// Adds a job and looks at what it would do.
    pub fn add_job(&self, mut setup: JobSetup) -> Result<JobView> {
        setup.validate()?;
        ExcludeFilter::new(&setup.excludes)?;
        let job = Job::new(setup);
        let id = job.id.clone();
        self.change_state(|state| state.jobs.push(job))?;
        self.inner.journal.info(&id, "job created");
        self.publish(&id);
        self.check(&id);
        self.job(&id)
    }

    /// Saves a changed setup. If the folders or the mode changed, the two-way memory is
    /// forgotten, so the next run starts fresh (and merges instead of deleting).
    pub fn update_job(&self, id: &JobId, mut setup: JobSetup) -> Result<JobView> {
        setup.validate()?;
        ExcludeFilter::new(&setup.excludes)?;
        let old = self.stored_job(id)?.setup;
        let folders_changed = old.source != setup.source || old.destination != setup.destination || old.mode != setup.mode;
        self.change_job(id, |job| job.setup = setup)?;
        if folders_changed {
            self.inner.baselines.forget(id);
            self.change_runtime(id, |rt| {
                rt.pending = None;
                rt.conflicts.clear();
                rt.problem = None;
            });
        }
        self.inner.journal.info(id, "settings changed");
        self.publish(id);
        self.check(id);
        self.job(id)
    }

    /// A copy of a job, named "… copy", paused so it doesn't run before it's edited.
    pub fn duplicate_job(&self, id: &JobId) -> Result<JobView> {
        let mut setup = self.stored_job(id)?.setup;
        setup.name = format!("{} copy", setup.name);
        let mut job = Job::new(setup);
        job.paused = true;
        let new_id = job.id.clone();
        self.change_state(|state| state.jobs.push(job))?;
        self.publish(&new_id);
        self.job(&new_id)
    }

    /// Pausing stops a running sync too. Paused jobs only run with "Sync now".
    pub fn set_paused(&self, id: &JobId, paused: bool) -> Result<()> {
        if paused {
            self.cancel(id);
        }
        self.change_job(id, |job| job.paused = paused)?;
        self.inner.journal.info(id, if paused { "paused" } else { "resumed" });
        self.publish(id);
        Ok(())
    }

    /// Removes a job (its folders and files are left alone).
    pub fn remove_job(&self, id: &JobId) -> Result<()> {
        self.cancel(id);
        self.change_state(|state| state.jobs.retain(|job| &job.id != id))?;
        self.runtime().remove(id);
        self.inner.baselines.forget(id);
        self.inner.journal.forget(id);
        self.emit(EngineEvent::JobRemoved(id.clone()));
        self.emit(EngineEvent::Overview(self.overview()));
        Ok(())
    }

    /// Moves a job to a new position in the list.
    pub fn move_job(&self, id: &JobId, to_index: usize) -> Result<()> {
        self.change_state(|state| {
            if let Some(from) = state.jobs.iter().position(|job| &job.id == id) {
                let job = state.jobs.remove(from);
                let to = to_index.min(state.jobs.len());
                state.jobs.insert(to, job);
            }
        })
    }
}
