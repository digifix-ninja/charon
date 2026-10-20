//! Adding, editing, pausing and removing jobs.

use crate::engine::{Engine, EngineEvent};
use crate::error::{Error, Result};
use crate::model::{Job, JobId, JobSetup, JobView};
use crate::plan::Side;
use crate::run::Marker;
use crate::scan::ExcludeFilter;

impl Engine {
    /// Adds a job and looks at what it would do. Its first sync always waits for the
    /// person's OK.
    pub fn add_job(&self, mut setup: JobSetup) -> Result<JobView> {
        setup.validate()?;
        ExcludeFilter::new(&setup.excludes, false)?;
        let job = Job::new(setup);
        let id = job.id.clone();
        let automation = job.setup.full_automation;
        self.change_state(|state| {
            if let Some(clash) = state.jobs.iter().find_map(|other| job.setup.clash_with(&other.setup, &other.setup.name)) {
                return Err(Error::Invalid(clash));
            }
            state.jobs.push(job);
            Ok(())
        })??;
        self.inner.journal.info(&id, "job created");
        if automation {
            self.inner.journal.warn(&id, "full automation turned on (both warnings accepted)");
        }
        self.publish(&id);
        self.check(&id);
        self.job(&id)
    }

    /// Saves a changed setup. Refused while the job syncs. A change to what a sync does
    /// (folders, mode, conflict rule, excludes, safe deletes, or turning full automation on)
    /// makes the next sync wait for the person's OK; new folders or a new mode also make
    /// Charon forget what it remembers of the last sync.
    pub fn update_job(&self, id: &JobId, mut setup: JobSetup) -> Result<JobView> {
        setup.validate()?;
        ExcludeFilter::new(&setup.excludes, false)?;
        let (old, name) = {
            let state = self.state();
            let job = state.jobs.iter().find(|job| &job.id == id).ok_or_else(|| Error::UnknownJob(id.clone()))?;
            (job.setup.clone(), job.setup.name.clone())
        };
        let folders_changed = old.source != setup.source || old.destination != setup.destination || old.mode != setup.mode;
        let automation_on = setup.full_automation && !old.full_automation;
        let automation_off = !setup.full_automation && old.full_automation;
        let review_next = setup.changes_what_syncs(&old) || automation_on;

        self.with_job_at_rest(id, &format!("Wait until “{name}” finishes syncing, then edit it."), |state, rt| {
            // Overlaps that were there before the edit (from an older Charon) don't block it.
            let clash = state.jobs.iter().filter(|other| &other.id != id).find_map(|other| {
                let now = setup.clash_with(&other.setup, &other.setup.name)?;
                old.clash_with(&other.setup, &other.setup.name).is_none().then_some(now)
            });
            if let Some(clash) = clash {
                return Err(Error::Invalid(clash));
            }
            let job = state.jobs.iter_mut().find(|job| &job.id == id).ok_or_else(|| Error::UnknownJob(id.clone()))?;
            if job.setup != setup {
                job.waiting_since = None;
            }
            job.setup = setup.clone();
            if review_next {
                job.needs_review = true;
            }
            if folders_changed {
                job.drives = None;
                job.destination_token = None;
                job.source_mark = None;
                job.run_in_progress = false;
                job.held = None;
                job.automatic_changes.clear();
                rt.forget_folders();
            } else {
                rt.stop_looking();
                rt.review = None;
            }
            Ok(())
        })?;
        if folders_changed {
            if let Err(e) = self.inner.baselines.forget(id) {
                // A memory for other folders is never used (it names its folders), so this
                // only leaves a stale file behind.
                log::warn!("couldn't forget the two-way memory of job {id}: {e}");
            }
            if let Err(e) = self.inner.landings.forget(id) {
                // The record names its folders and the destination's marker, so it isn't used.
                log::warn!("couldn't forget the copies whose dates didn't stick for job {id}: {e}");
            }
        }
        self.inner.journal.info(id, if folders_changed { "folders or mode changed: Charon forgot the last sync" } else { "settings changed" });
        if automation_on {
            self.inner.journal.warn(id, "full automation turned on (both warnings accepted)");
        }
        if automation_off {
            self.inner.journal.info(id, "full automation turned off");
        }
        self.publish(id);
        self.check(id);
        self.job(id)
    }

    /// A copy of a job, named "… copy", paused, and with full automation off (turning it
    /// on again goes through both warnings).
    pub fn duplicate_job(&self, id: &JobId) -> Result<JobView> {
        let mut setup = self.stored_job(id)?.setup;
        setup.name = format!("{} copy", setup.name);
        setup.full_automation = false;
        let mut job = Job::new(setup);
        job.paused = true;
        let new_id = job.id.clone();
        self.change_state(|state| state.jobs.push(job))?;
        self.inner.journal.info(&new_id, "job created as a copy");
        self.publish(&new_id);
        self.job(&new_id)
    }

    /// Pausing stops a running sync too. Paused jobs only sync when asked.
    pub fn set_paused(&self, id: &JobId, paused: bool) -> Result<()> {
        if paused {
            self.cancel(id);
        }
        self.change_job(id, |job| job.paused = paused)?;
        self.inner.journal.info(id, if paused { "paused" } else { "resumed" });
        self.publish(id);
        Ok(())
    }

    /// Removes a job. Refused while it syncs. Its folders and files are left alone; only
    /// Charon's own marker for the job is taken out of them.
    pub fn remove_job(&self, id: &JobId) -> Result<()> {
        let (job, name) = {
            let job = self.stored_job(id)?;
            let name = job.setup.name.clone();
            (job, name)
        };
        self.with_job_at_rest(id, &format!("Stop “{name}” first, then remove it."), |state, rt| {
            rt.stop_looking();
            state.jobs.retain(|job| &job.id != id);
            Ok(())
        })?;
        self.runtime().remove(id);
        if let Err(e) = self.inner.baselines.forget(id) {
            log::warn!("couldn't forget the two-way memory of job {id}: {e}");
        }
        if let Err(e) = self.inner.landings.forget(id) {
            log::warn!("couldn't forget the copies whose dates didn't stick for job {id}: {e}");
        }
        self.inner.journal.forget(id);
        for side in [Side::Source, Side::Destination] {
            let root = job.setup.root(side);
            if root.is_dir() {
                Marker::forget(root, id);
            }
        }
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

    /// "Leave this folder out": excludes exactly the place at `path` (as spelled on disk,
    /// relative to the job's folder). The next sync waits for the person's OK.
    pub fn leave_out(&self, id: &JobId, side: Side, path: &str) -> Result<JobView> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err(Error::Invalid("Choose a place inside the job's folders.".into()));
        }
        let pattern = ExcludeFilter::pattern_for(path);
        ExcludeFilter::new(std::slice::from_ref(&pattern), false)?;
        let name = self.stored_job(id)?.setup.name;
        self.with_job_at_rest(id, &format!("Wait until “{name}” finishes syncing."), |state, rt| {
            let job = state.jobs.iter_mut().find(|job| &job.id == id).ok_or_else(|| Error::UnknownJob(id.clone()))?;
            if !job.setup.excludes.contains(&pattern) {
                job.setup.excludes.push(pattern.clone());
            }
            job.needs_review = true;
            rt.stop_looking();
            rt.review = None;
            Ok(())
        })?;
        self.inner.journal.info(id, &format!("left out {path} (on the {})", side.word()));
        self.publish(id);
        self.check(id);
        self.job(id)
    }
}
