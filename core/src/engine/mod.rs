//! The engine: owns the jobs, looks at their folders, syncs them and tells the window what
//! changed.
//!
//! * `mod.rs`     – `Engine`: opening, settings, reading rows, events
//! * `jobs.rs`    – adding, editing, pausing and removing jobs
//! * `survey.rs`  – looking at both folders and planning, with every safety check
//! * `review.rs`  – the list of changes waiting for the person's OK, and confirming it
//! * `runs.rs`    – syncing: confirmed runs, full automation, held-back runs
//! * `checks.rs`  – background looks for changes, and reminders when changes wait
//! * `status.rs`  – turning a job and its live state into the row the window shows
//! * `runtime.rs` – the live state of each job (running, progress, last look, review)
//!
//! `Engine` is cheap to clone; all clones share the same jobs. Locks are always taken in
//! the same order, the stored state before the live state. Nothing that looks at a job's
//! folders or tells the window happens while either is held (saving in the app's own data
//! folder does: the state file, so a change counts only once it's saved, and what looks
//! found missing, so two saves never come in the wrong order).

mod checks;
mod jobs;
mod review;
mod runs;
mod runtime;
mod status;
mod survey;

pub use review::{NotePage, StepPage};
pub use runs::SyncStart;

use crate::error::{Error, Result};
use crate::model::{Finished, Job, JobId, JobView, Overview, Progress, Settings, UpgradeNotice, WaitingJob};
use crate::plan::Side;
use crate::store::{Absences, Baselines, DataFolder, DataLock, Journal, Landings, LogLine, StateFile, StoredState, Unattended};
use runtime::{JobRuntime, Look, Missing};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Something the window (or the tray, or notifications) may want to know.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A job's row changed.
    Job(Box<JobView>),
    /// A job was removed.
    JobRemoved(JobId),
    /// Live progress of a running sync (frequent, small).
    Progress { id: JobId, progress: Progress },
    /// A run finished.
    Finished(Finished),
    /// The overall picture changed (for the tray icon and the header).
    Overview(Overview),
    /// The settings changed.
    Settings(Settings),
    /// Items counted so far while looking at one side of a job.
    ScanProgress { id: JobId, side: Side, items: u64 },
    /// Pairs of files read so far to tell whether they are identical (after the scans, when
    /// some files differ only by date).
    CompareProgress { id: JobId, files: u64, total_files: u64, bytes: u64, total_bytes: u64 },
    /// Something changed in the job's folders after its list was made: it can't be
    /// confirmed anymore.
    ReviewStale { id: JobId, review_id: String },
    /// Jobs whose changes wait for the person's OK and are due a mention (one notification).
    Waiting(Vec<WaitingJob>),
    /// A run nobody looked at was held back for the person's OK.
    Held { id: JobId, name: String, reason: String },
}

/// Receives every engine event. The app shell forwards them to the window.
pub type Emitter = Arc<dyn Fn(EngineEvent) + Send + Sync>;

struct Inner {
    state_file: StateFile,
    baselines: Baselines,
    /// Mirror and Backup: copies whose dates didn't stick on the destination.
    landings: Landings,
    /// Full automation: what automatic runs changed since the person last confirmed a list.
    unattended: Unattended,
    /// Items found missing on a drive that may show changes late (see `Missing`).
    absences: Absences,
    journal: Journal,
    state: Mutex<StoredState>,
    runtime: Mutex<HashMap<JobId, JobRuntime>>,
    emit: Emitter,
    /// Every view gets the next number (see `JobView::rev`).
    rev: AtomicU64,
    /// Charon is quitting: nothing new starts.
    quitting: AtomicBool,
    /// When this engine opened (reminders wait for the first looks after a start).
    opened: Instant,
    /// Only one Charon works with the data folder at a time.
    _lock: DataLock,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    /// Opens (or creates) Charon's data in `data_folder`. Fails with
    /// `Error::AlreadyRunning` while another Charon works with the same folder.
    pub fn open(data_folder: impl Into<PathBuf>, emit: Emitter) -> Result<Self> {
        let folder = DataFolder::new(data_folder);
        let lock = DataLock::acquire(folder.root())?;
        let state_file = StateFile::new(folder.state_file(), folder.legacy_state_file());
        let state = state_file.load()?;
        let absences = Absences::new(folder.absences());
        // What looks found missing before this launch, and since when.
        let runtime = state
            .jobs
            .iter()
            .map(|job| (job.id.clone(), absences.load(&job.id, &job.setup)))
            .filter(|(_, kept)| !kept.is_empty())
            .map(|(id, kept)| (id, JobRuntime { missing: Missing::restored(kept), ..JobRuntime::default() }))
            .collect();
        let engine = Self {
            inner: Arc::new(Inner {
                state_file,
                baselines: Baselines::new(folder.baselines()),
                landings: Landings::new(folder.landings()),
                unattended: Unattended::new(folder.unattended()),
                absences,
                journal: Journal::new(folder.logs()),
                state: Mutex::new(state),
                runtime: Mutex::new(runtime),
                emit,
                rev: AtomicU64::new(0),
                quitting: AtomicBool::new(false),
                opened: Instant::now(),
                _lock: lock,
            }),
        };
        engine.journal_upgrade();
        Ok(engine)
    }

    /// Writes the move to confirm-first syncing into the log of each job it changed (once).
    fn journal_upgrade(&self) {
        let Some(notice) = self.state().upgrade_notice.clone().filter(|notice| !notice.journaled) else { return };
        for job in &notice.jobs {
            self.inner.journal.info(
                &job.id,
                &format!("Charon {}: this job no longer syncs by itself; it looks for changes and waits for your OK", notice.version),
            );
        }
        if let Err(e) = self.change_state(|state| {
            if let Some(notice) = state.upgrade_notice.as_mut() {
                notice.journaled = true;
            }
        }) {
            log::warn!("couldn't save that the update was logged: {e}");
        }
    }

    // ---------------------------------------------------------------- reading

    pub fn settings(&self) -> Settings {
        self.state().settings.clone()
    }

    /// Every job as a row, in the order they were added.
    pub fn jobs(&self) -> Vec<JobView> {
        let (snapshots, settings) = {
            let state = self.state();
            let runtime = self.runtime();
            let snapshots: Vec<_> = state.jobs.iter().map(|job| self.snapshot(job, runtime.get(&job.id))).collect();
            (snapshots, state.settings.clone())
        };
        snapshots.into_iter().map(|snapshot| self.view_of(snapshot, &settings)).collect()
    }

    /// One job as a row.
    pub fn job(&self, id: &JobId) -> Result<JobView> {
        let (snapshot, settings) = {
            let state = self.state();
            let runtime = self.runtime();
            let job = state.jobs.iter().find(|job| &job.id == id).ok_or_else(|| Error::UnknownJob(id.clone()))?;
            (self.snapshot(job, runtime.get(id)), state.settings.clone())
        };
        Ok(self.view_of(snapshot, &settings))
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

    /// The notice shown once after the move to 0.3, while it hasn't been dismissed.
    pub fn upgrade_notice(&self) -> Option<UpgradeNotice> {
        let state = self.state();
        let mut notice = state.upgrade_notice.clone()?;
        // Only jobs that still exist.
        notice.jobs.retain(|upgraded| state.jobs.iter().any(|job| job.id == upgraded.id));
        (!notice.jobs.is_empty()).then_some(notice)
    }

    pub fn dismiss_upgrade_notice(&self) -> Result<()> {
        self.change_state(|state| state.upgrade_notice = None)
    }

    // ---------------------------------------------------------------- settings

    /// Saves new settings. "Pause everything" only changes through `set_all_paused`, so a
    /// window with an old copy of the settings can never resume everything by accident.
    pub fn set_settings(&self, mut settings: Settings) -> Result<()> {
        self.change_state(|state| {
            settings.paused_all = state.settings.paused_all;
            state.settings = settings;
        })?;
        self.emit(EngineEvent::Settings(self.settings()));
        self.publish_all();
        Ok(())
    }

    /// Pauses or resumes every job at once.
    pub fn set_all_paused(&self, paused: bool) -> Result<()> {
        self.change_state(|state| state.settings.paused_all = paused)?;
        self.emit(EngineEvent::Settings(self.settings()));
        self.publish_all();
        Ok(())
    }

    // ---------------------------------------------------------------- quitting

    /// Before Charon quits: nothing new starts from now on, every look stops, and every
    /// running sync stops between two steps, recording what it carried as usual. Waits at
    /// most `wait` for the syncs; returns whether they all stopped in time.
    pub fn shut_down(&self, wait: Duration) -> bool {
        self.inner.quitting.store(true, Ordering::SeqCst);
        for rt in self.runtime().values_mut() {
            rt.stop_looking();
            if let Some(cancel) = &rt.cancel {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        let deadline = Instant::now() + wait;
        while self.runtime().values().any(|rt| rt.is_running()) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(25));
        }
        true
    }

    /// Whether Charon is quitting.
    pub fn is_shutting_down(&self) -> bool {
        self.inner.quitting.load(Ordering::SeqCst)
    }

    /// The jobs syncing right now.
    pub fn running(&self) -> Vec<JobId> {
        self.runtime().iter().filter(|(_, rt)| rt.is_running()).map(|(id, _)| id.clone()).collect()
    }

    // ---------------------------------------------------------------- internals

    fn state(&self) -> MutexGuard<'_, StoredState> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn runtime(&self) -> MutexGuard<'_, HashMap<JobId, JobRuntime>> {
        self.inner.runtime.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn stored_job(&self, id: &JobId) -> Result<Job> {
        self.state()
            .jobs
            .iter()
            .find(|job| &job.id == id)
            .cloned()
            .ok_or_else(|| Error::UnknownJob(id.clone()))
    }

    /// Changes the stored state: the changed copy is saved first, and only then becomes the
    /// state Charon works with. A failed save changes nothing.
    fn change_state<R>(&self, change: impl FnOnce(&mut StoredState) -> R) -> Result<R> {
        let mut state = self.state();
        let mut next = state.clone();
        let result = change(&mut next);
        self.inner.state_file.save(&next)?;
        *state = next;
        Ok(result)
    }

    /// Changes one stored job and saves.
    fn change_job(&self, id: &JobId, change: impl FnOnce(&mut Job)) -> Result<()> {
        self.change_state(|state| state.jobs.iter_mut().find(|job| &job.id == id).map(change))?
            .ok_or_else(|| Error::UnknownJob(id.clone()))
    }

    /// Changes one stored job and saves, like `change_job`, but when the save fails (a full
    /// system disk, a state file another program holds open) the change still holds while
    /// Charon runs, and the next save that works writes it too. For what a job waits for
    /// after a run (held back, stopped, not recorded): every part of Charon must keep
    /// seeing that the job waits for an OK, whether or not the disk took it.
    fn change_job_anyway(&self, id: &JobId, change: impl FnOnce(&mut Job)) -> Result<()> {
        let mut state = self.state();
        let mut next = state.clone();
        next.jobs.iter_mut().find(|job| &job.id == id).map(change).ok_or_else(|| Error::UnknownJob(id.clone()))?;
        let saved = self.inner.state_file.save(&next);
        *state = next;
        saved
    }

    /// Changes the stored state and the job's live state together, but only while the job
    /// isn't syncing (refused with `busy`). The stored change is saved before it counts.
    fn with_job_at_rest(&self, id: &JobId, busy: &str, change: impl FnOnce(&mut StoredState, &mut JobRuntime) -> Result<()>) -> Result<()> {
        let mut state = self.state();
        let mut runtime = self.runtime();
        let rt = runtime.entry(id.clone()).or_default();
        if rt.is_running() {
            return Err(Error::Invalid(busy.to_string()));
        }
        let mut next = state.clone();
        change(&mut next, rt)?;
        self.inner.state_file.save(&next)?;
        *state = next;
        Ok(())
    }

    /// Changes one job's live state.
    fn change_runtime<R>(&self, id: &JobId, change: impl FnOnce(&mut JobRuntime) -> R) -> R {
        change(self.runtime().entry(id.clone()).or_default())
    }

    /// Changes what the looks at `job` found missing (see `Missing`), and keeps it for the
    /// next launch when that changed. It's saved while the live state is locked, so two
    /// looks never save in the wrong order; a record that can't be saved is removed, so the
    /// next launch starts the waits over rather than trust an old record.
    fn change_missing<R>(&self, job: &Job, change: impl FnOnce(&mut Missing) -> R) -> R {
        let mut runtime = self.runtime();
        let missing = &mut runtime.entry(job.id.clone()).or_default().missing;
        let before = missing.kept();
        let result = change(missing);
        let kept = missing.kept();
        if kept != before {
            if let Err(e) = self.inner.absences.save(&job.id, &job.setup, &kept) {
                log::warn!("couldn't keep the items missing in {} for the next launch: {e}", job.setup.name);
                let _ = self.inner.absences.forget(&job.id);
            }
        }
        result
    }

    fn emit(&self, event: EngineEvent) {
        (self.inner.emit)(event);
    }

    /// Sends a job's fresh row, and the overview.
    pub fn publish(&self, id: &JobId) {
        if let Ok(view) = self.job(id) {
            self.emit(EngineEvent::Job(Box::new(view)));
        }
        self.emit(EngineEvent::Overview(self.overview()));
    }

    fn publish_all(&self) {
        let views = self.jobs();
        let overview = Overview::of(&views, self.settings().paused_all);
        for view in views {
            self.emit(EngineEvent::Job(Box::new(view)));
        }
        self.emit(EngineEvent::Overview(overview));
    }
}
