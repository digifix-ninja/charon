//! The engine: owns the jobs, runs them and tells the window what changed.
//!
//! * `mod.rs`     – `Engine`: opening, settings, reading rows, events
//! * `jobs.rs`    – adding, editing, pausing and removing jobs
//! * `runs.rs`    – syncing, previews (dry runs), checks for pending changes, conflicts
//! * `status.rs`  – turning a job and its live state into the row the window shows
//! * `runtime.rs` – the live state of each job (running, progress, pending, problems)
//!
//! `Engine` is cheap to clone; all clones share the same jobs.

mod jobs;
mod runs;
mod runtime;
mod status;

pub use runs::Preview;

use crate::error::Result;
use crate::model::{Finished, Job, JobId, JobView, Overview, Progress, Settings};
use crate::store::{Baselines, DataFolder, Journal, LogLine, StateFile, StoredState};
use runtime::JobRuntime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

/// Something the window (or the tray, or notifications) may want to know.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A job's row changed.
    Job(JobView),
    /// A job was removed.
    JobRemoved(JobId),
    /// Live progress of a running sync (frequent, small).
    Progress { id: JobId, progress: Progress },
    /// A run finished.
    Finished(Finished),
    /// The overall picture changed (for the tray icon and the header).
    Overview(Overview),
}

/// Receives every engine event. The app shell forwards them to the window.
pub type Emitter = Arc<dyn Fn(EngineEvent) + Send + Sync>;

struct Inner {
    state_file: StateFile,
    baselines: Baselines,
    journal: Journal,
    state: Mutex<StoredState>,
    runtime: Mutex<HashMap<JobId, JobRuntime>>,
    emit: Emitter,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    /// Opens (or creates) Charon's data in `data_folder`.
    pub fn open(data_folder: impl Into<PathBuf>, emit: Emitter) -> Result<Self> {
        let folder = DataFolder::new(data_folder);
        let state_file = StateFile::new(folder.state_file());
        let state = state_file.load()?;
        Ok(Self {
            inner: Arc::new(Inner {
                state_file,
                baselines: Baselines::new(folder.baselines()),
                journal: Journal::new(folder.logs()),
                state: Mutex::new(state),
                runtime: Mutex::new(HashMap::new()),
                emit,
            }),
        })
    }

    // ---------------------------------------------------------------- reading

    pub fn settings(&self) -> Settings {
        self.state().settings.clone()
    }

    /// Every job as a row, in the order they were added.
    pub fn jobs(&self) -> Vec<JobView> {
        let (jobs, settings) = {
            let state = self.state();
            (state.jobs.clone(), state.settings.clone())
        };
        jobs.into_iter().map(|job| self.view_of(job, &settings)).collect()
    }

    /// One job as a row.
    pub fn job(&self, id: &JobId) -> Result<JobView> {
        let job = self.stored_job(id)?;
        Ok(self.view_of(job, &self.settings()))
    }

    /// The stored jobs, without live state (for the autopilot).
    pub fn job_list(&self) -> Vec<Job> {
        self.state().jobs.clone()
    }

    pub fn overview(&self) -> Overview {
        Overview::of(&self.jobs(), self.settings().paused_all)
    }

    /// The newest lines of a job's activity log, newest first.
    pub fn log(&self, id: &JobId, limit: usize) -> Vec<LogLine> {
        self.inner.journal.read(id, limit)
    }

    // ---------------------------------------------------------------- settings

    /// Saves new settings. Every row is resent, since "pause everything" changes them all.
    pub fn set_settings(&self, settings: Settings) -> Result<()> {
        self.change_state(|state| state.settings = settings)?;
        self.publish_all();
        Ok(())
    }

    /// Pauses or resumes every job at once.
    pub fn set_all_paused(&self, paused: bool) -> Result<()> {
        let mut settings = self.settings();
        settings.paused_all = paused;
        self.set_settings(settings)
    }

    // ---------------------------------------------------------------- internals

    fn state(&self) -> MutexGuard<'_, StoredState> {
        self.inner.state.lock().expect("state lock")
    }

    fn runtime(&self) -> MutexGuard<'_, HashMap<JobId, JobRuntime>> {
        self.inner.runtime.lock().expect("runtime lock")
    }

    fn stored_job(&self, id: &JobId) -> Result<Job> {
        self.state()
            .jobs
            .iter()
            .find(|job| &job.id == id)
            .cloned()
            .ok_or_else(|| crate::Error::UnknownJob(id.clone()))
    }

    /// Changes the stored state and saves it.
    fn change_state<R>(&self, change: impl FnOnce(&mut StoredState) -> R) -> Result<R> {
        let mut state = self.state();
        let result = change(&mut state);
        self.inner.state_file.save(&state)?;
        Ok(result)
    }

    /// Changes one stored job and saves.
    fn change_job(&self, id: &JobId, change: impl FnOnce(&mut Job)) -> Result<()> {
        self.change_state(|state| state.jobs.iter_mut().find(|job| &job.id == id).map(change))?
            .ok_or_else(|| crate::Error::UnknownJob(id.clone()))
    }

    /// Changes one job's live state.
    fn change_runtime<R>(&self, id: &JobId, change: impl FnOnce(&mut JobRuntime) -> R) -> R {
        change(self.runtime().entry(id.clone()).or_default())
    }

    fn emit(&self, event: EngineEvent) {
        (self.inner.emit)(event);
    }

    /// Sends a job's fresh row, and the overview.
    pub fn publish(&self, id: &JobId) {
        if let Ok(view) = self.job(id) {
            self.emit(EngineEvent::Job(view));
        }
        self.emit(EngineEvent::Overview(self.overview()));
    }

    fn publish_all(&self) {
        let views = self.jobs();
        let overview = Overview::of(&views, self.settings().paused_all);
        for view in views {
            self.emit(EngineEvent::Job(view));
        }
        self.emit(EngineEvent::Overview(overview));
    }
}
