//! Syncing: runs the person confirmed, and runs of jobs with full automation.
//!
//! A run carries out a plan made from a survey. A confirmed run uses the very survey the
//! person looked at; a run nobody looked at makes a fresh one, and goes ahead only when
//! nothing about it needs a look (`Survey::holds`). Otherwise it is held back: nothing is
//! touched, and the job waits for the person's OK.
//!
//! While files are being carried, the job's record says so (`Job::run_in_progress`). Only
//! when every change is on the drives does Charon write the markers, save the two-way
//! memory and clear that mark. A run that never got there (Charon closed, the computer
//! stopped) is recognised at the next look, which then doesn't trust the memory.

use crate::engine::checks::ChangeAt;
use crate::engine::runtime::{OwnWrites, MISSING_FOR};
use crate::engine::survey::{Purpose, Survey};
use crate::engine::{Engine, EngineEvent, JobRuntime, Look};
use crate::error::{Error, Result};
use crate::model::{plural, Drives, Finished, Job, JobId, JobSetup, Mark, Phase, Place, Progress, ReviewKind, RunSummary, FAILED_NAMED, PLACES_SHOWN};
use crate::plan::{Action, Baseline, NoteKind, Plan, Side};
use crate::run::{BusyNote, Care, Claim, Executor, Found, Marker, Markers, Outcome, ProgressMeter, Shores, Spellings, Trash};
use crate::scan::{never_download_on_this_thread, ExcludeFilter, Keys, Scanner, Snapshot, MARKER_FOLDER};
use crate::store::{Landings, Memory};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Why a job waits for an OK after the person stopped its sync.
pub(crate) const STOPPED: &str = "You stopped the last sync. Nothing more happens until you confirm the list.";
/// Why a job waits for an OK after a sync Charon couldn't record.
pub(crate) const UNRECORDED: &str = "Charon couldn't record the last sync, so nothing more happens until you confirm the list.";
/// Why a job with full automation waits for an OK after Charon couldn't count what an
/// automatic sync removed or replaced.
pub(crate) const UNCOUNTED: &str =
    "Charon couldn't count what the last automatic sync removed or replaced, so nothing more happens until you confirm the list.";
/// Why a job waits for an OK after its sync ended on a mistake in Charon itself.
pub(crate) const CRASHED: &str = "The last sync stopped on an error of Charon's own, so nothing more happens until you confirm the list.";
/// Why nothing syncs while an older Charon runs at the same time.
pub(crate) const OLDER_CHARON: &str = "An older version of Charon is running at the same time, and syncs your jobs by itself without this version's safety checks. \
     Quit it (from its icon in the menu bar or the system tray), then quit and reopen Charon. Until then, nothing syncs.";
/// Why runs nobody looks at wait while Charon's state can't be saved.
pub(crate) const CANNOT_SAVE: &str = "Charon can't save its jobs and settings right now (is the disk full?), so nothing syncs by itself until it can again.";
/// What a sync wrote is forgotten once it ended this long ago. Only the changes the folder
/// watcher reports right after a sync are told apart by it (see `note_change_while_syncing`).
const OWN_WRITES_KEPT: Duration = Duration::from_secs(60);
/// Live progress reaches the window at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);
/// At most this many failed or skipped items are written to the log per run.
const LOGGED_ITEMS: usize = 50;

/// What "Sync now" did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncStart {
    /// The job runs by itself: its sync started.
    Started,
    /// The job waits for the person's OK: the window shows the list first.
    Review,
}

/// How a run came about.
#[derive(Debug, Clone)]
pub(crate) struct Crossing {
    pub kind: ReviewKind,
    /// The person confirmed the list.
    pub approved: bool,
    /// For the log: "confirmed by you", "automatically on schedule".
    pub reason: String,
    /// The conflict this run settles (a resolution), by its key.
    pub resolving: Option<String>,
}

/// For each other job: its setup as it was looked at, and whether it shares a folder with
/// the job about to start (worked out before any lock is taken, since it asks the disk).
pub(crate) struct Neighbours {
    mine: JobSetup,
    others: HashMap<JobId, (JobSetup, bool)>,
}

impl Engine {
    // ---------------------------------------------------------------- starting

    /// "Sync now": starts the sync of a job that runs by itself; any other job shows its
    /// list of changes first, and so does one that must ask after a stopped or held-back
    /// run (see `must_ask`). Works for paused jobs too: pausing only stops automatic runs.
    pub fn sync_now(&self, id: &JobId) -> Result<SyncStart> {
        let job = self.stored_job(id)?;
        if !job.runs_by_itself() || self.must_ask(id) {
            return Ok(SyncStart::Review);
        }
        self.start_unreviewed(id, "started by hand")?;
        Ok(SyncStart::Started)
    }

    /// "Sync all": starts every job that runs by itself (not paused, folders reachable)
    /// and returns the other jobs that could sync now: their lists need the person's OK.
    pub fn sync_all(&self) -> Vec<JobId> {
        let mut asking = Vec::new();
        for job in self.job_list() {
            if job.paused || !job.setup.source.is_dir() || !job.setup.destination.is_dir() || self.is_running(&job.id) {
                continue;
            }
            if job.runs_by_itself() && !self.must_ask(&job.id) {
                // Another job using the folders makes it wait, not give up (see `start_wanted`).
                self.want(&job.id, "started with Sync all");
                self.start_wanted(&job.id);
            } else {
                asking.push(job.id);
            }
        }
        asking
    }

    /// Stops a running sync (what's already carried stays), or calls off a confirmed one
    /// that waits for another sync. The next sync of the job waits for the person's OK.
    pub fn cancel(&self, id: &JobId) {
        // Under one lock: a confirmed sync that starts now either sees it called off, or
        // runs with its stop flag set.
        let called_off = self.runtime().get_mut(id).is_some_and(|rt| {
            if let Some(cancel) = &rt.cancel {
                cancel.store(true, Ordering::Relaxed);
                rt.stopped_by_person = true;
            }
            rt.call_off_confirmed()
        });
        if called_off {
            self.called_off(id, "");
        }
    }

    /// Calls off every confirmed sync that waits for another sync (*Pause everything*: the
    /// person wants nothing to start without them now). A sync that already started isn't
    /// stopped, as *Pause everything* doesn't stop syncs at work.
    pub(crate) fn call_off_all_confirmed(&self, why: &str) {
        let called_off: Vec<JobId> = self
            .runtime()
            .iter_mut()
            .filter_map(|(id, rt)| (!rt.is_running() && rt.call_off_confirmed()).then(|| id.clone()))
            .collect();
        for id in called_off {
            self.called_off(&id, why);
        }
    }

    /// Says in the job's log, and on its row, that its confirmed sync was called off.
    fn called_off(&self, id: &JobId, why: &str) {
        self.inner.journal.info(id, &format!("the confirmed sync that waited for another sync was called off{why}"));
        self.publish(id);
    }

    /// Whether runs nobody looks at are refused for this job until the person confirms a
    /// list (see `JobRuntime::must_ask`).
    pub(crate) fn must_ask(&self, id: &JobId) -> bool {
        self.runtime().get(id).is_some_and(|rt| rt.must_ask)
    }

    /// Whether a sync of this job is running.
    pub fn is_running(&self, id: &JobId) -> bool {
        self.runtime().get(id).is_some_and(|rt| rt.is_running())
    }

    /// Whether the job is running or just finished: the moment its own writes would look
    /// like changes to the folder watcher.
    pub fn is_settling(&self, id: &JobId, quiet: Duration) -> bool {
        self.runtime()
            .get(id)
            .is_some_and(|rt| rt.is_running() || rt.finished_at.is_some_and(|t| t.elapsed() < quiet))
    }

    /// Notes that a job with full automation should sync (`reason` is for the log). The run
    /// starts with `start_wanted`, now or as soon as nothing stands in its way.
    pub fn want(&self, id: &JobId, reason: &str) {
        self.change_runtime(id, |rt| {
            if rt.wanted.is_none() {
                rt.wanted = Some(format!("automatically {reason}"));
            }
        });
    }

    /// Forgets a wanted run (the job can't run by itself now: paused, offline, or it waits
    /// for the person's OK).
    pub fn unwant(&self, id: &JobId) {
        let changed = self.change_runtime(id, |rt| {
            let had = rt.wanted.take().is_some() || rt.waiting_for.is_some();
            rt.waiting_for = None;
            had
        });
        if changed {
            self.publish(id);
        }
    }

    /// Starts the job's wanted run, if it has one. While another sync uses the folders it
    /// keeps waiting (the row says for which); any other refusal drops it.
    pub fn start_wanted(&self, id: &JobId) {
        let Some(reason) = self.runtime().get(id).and_then(|rt| rt.wanted.clone()) else { return };
        let result = self.start_unreviewed(id, &reason);
        let (now_waiting, changed) = self.change_runtime(id, |rt| {
            let before = rt.waiting_for.clone();
            match &result {
                // A run that started took the wanted run along (see `begin_run`).
                Ok(()) | Err(Error::Busy) => {}
                Err(Error::FoldersBusy(name)) => rt.waiting_for = Some(name.clone()),
                Err(_) => {
                    rt.wanted = None;
                    rt.waiting_for = None;
                }
            }
            (rt.waiting_for.clone(), before != rt.waiting_for)
        });
        match result {
            Ok(()) => {}
            Err(Error::FoldersBusy(_)) | Err(Error::Busy) if changed => {
                log::info!("job {id}: automatic sync waits for {}", now_waiting.unwrap_or_default());
                self.publish(id);
            }
            Err(Error::FoldersBusy(_)) | Err(Error::Busy) => {}
            Err(e) => {
                log::info!("job {id}: automatic sync not started: {e}");
                if changed {
                    self.publish(id);
                }
            }
        }
    }

    /// Starts a run nobody looks at (full automation): it surveys first, and holds back for
    /// the person's OK when anything about the plan needs a look.
    pub(crate) fn start_unreviewed(&self, id: &JobId, reason: &str) -> Result<()> {
        // A run whose end can't be recorded mustn't start by itself (a failed save tries
        // again at most once a minute).
        if !self.can_save() {
            return Err(Error::Invalid(CANNOT_SAVE.into()));
        }
        let (job, cancel) = self.begin_run(id, |job, rt| {
            if !job.runs_by_itself() || rt.must_ask {
                return Err(Error::Invalid("This job waits for your OK before it syncs.".into()));
            }
            if rt.confirmed.is_some() {
                return Err(Error::Invalid("You confirmed a list of this job's changes, and its sync waits for another sync to finish.".into()));
            }
            if rt.preparing.is_some() || rt.review.as_ref().is_some_and(|review| Utc::now() < review.expires_at) {
                return Err(Error::Invalid("A list of this job's changes is open, waiting for your OK.".into()));
            }
            Ok(())
        })?;
        self.publish(id);
        let crossing = Crossing { kind: ReviewKind::Sync, approved: false, reason: reason.to_string(), resolving: None };
        self.spawn_crossing(job, cancel, crossing, |engine, job, cancel, crossing| match engine.survey(&job, &cancel, Purpose::Automatic) {
            Ok(survey) if !survey.holds.is_empty() => engine.hold(&job, &survey),
            Ok(mut survey) => {
                engine.hold_back_recent_removals(&job, &mut survey);
                match engine.claim_folders(&job, &survey) {
                    Ok(claim) => engine.cross(&job, &survey, &cancel, &crossing, claim),
                    Err(who) => {
                        engine.not_begun(&job, &crossing, &who);
                    }
                }
            }
            Err(e) => engine.unfinished(&job, &crossing, e),
        })
    }

    /// Leaves for a later run the removals that follow an item missing for only a few
    /// minutes on a drive that may show changes late (see `Missing`), and says so in the log.
    /// That run comes by itself once they've waited long enough, whatever starts the job's
    /// syncs otherwise (see `take_follow_up`).
    fn hold_back_recent_removals(&self, job: &Job, survey: &mut Survey) {
        let held = self.change_missing(job, |missing| missing.hold_back_recent(&mut survey.plan, &survey.source, &survey.destination, Utc::now()));
        survey.left_for_later = Plan { actions: held, notes: Vec::new() };
        let count = survey.left_for_later.actions.len() as u64;
        if count > 0 {
            self.inner.journal.info(
                &job.id,
                &format!(
                    "left {} for a later sync: the items are missing on a network share or a cloud drive, which can show changes late, \
                     so Charon waits until looks have found them missing for {} minutes",
                    plural(count, "removal"),
                    MISSING_FOR.as_secs() / 60
                ),
            );
        }
    }

    /// Marks the job as running, if it may run now: Charon isn't quitting, the job isn't
    /// running already, `check` (the caller's own conditions) passes, and no other sync is
    /// at work in its folders: no running job of this Charon by their paths, and no sync of
    /// this or another Charon by its busy notes (see `run::Claim`; the run leaves its own
    /// notes before it changes anything).
    pub(crate) fn begin_run(&self, id: &JobId, check: impl FnOnce(&Job, &mut JobRuntime) -> Result<()>) -> Result<(Job, Arc<AtomicBool>)> {
        if self.older_charon_running() {
            return Err(Error::Invalid(OLDER_CHARON.into()));
        }
        // Both ask the disk, so they come before any lock is taken.
        let neighbours = self.neighbours(id);
        let in_use = self.folders_in_use(id);
        let state = self.state();
        let job = state.jobs.iter().find(|job| &job.id == id).cloned().ok_or_else(|| Error::UnknownJob(id.clone()))?;
        let mut runtime = self.runtime();
        if self.is_shutting_down() {
            return Err(Error::Invalid("Charon is quitting.".into()));
        }
        if runtime.get(id).is_some_and(|rt| rt.is_running()) {
            return Err(Error::Busy);
        }
        check(&job, runtime.entry(id.clone()).or_default())?;
        if let Some(name) = Self::busy_neighbour(&state.jobs, &runtime, &job, &neighbours).or(in_use) {
            return Err(Error::FoldersBusy(name));
        }
        let rt = runtime.entry(id.clone()).or_default();
        let cancel = Arc::new(AtomicBool::new(false));
        rt.cancel = Some(cancel.clone());
        rt.stopped_by_person = false;
        rt.progress = Some(Progress::scanning());
        rt.problem = None;
        // This run does what a wanted run would have done.
        rt.wanted = None;
        rt.waiting_for = None;
        // A look at folders that are about to change would be wrong at once.
        rt.outdate_looks();
        // From now on, changes in the folders are told apart from the run's own writes.
        rt.own_writes = Some(OwnWrites::default());
        Ok((job, cancel))
    }

    /// Who else is at work in the job's folders by their busy notes, if anyone: a job of
    /// this Charon whose folders are the same place under other names, or another Charon
    /// (asks the disk; done before locking).
    pub(crate) fn folders_in_use(&self, id: &JobId) -> Option<String> {
        let job = self.stored_job(id).ok()?;
        let inside = self.runtime().get(id).map(|rt| rt.folders_inside.clone()).unwrap_or_default();
        let me = self.inner.busy.note(id, &job.setup.name);
        self.inner.busy.around(&[&job.setup.source, &job.setup.destination], &inside, &me).first().map(|note| self.busy_with(note))
    }

    /// Leaves the run's busy notes in the job's folders and in the Charon folders inside them
    /// (see `run::Claim`), unless another sync is at work there or in the folders above and
    /// inside them: then it says who.
    pub(crate) fn claim_folders(&self, job: &Job, survey: &Survey) -> std::result::Result<Claim, String> {
        let inside = Self::folders_inside_now(survey);
        let note = self.inner.busy.note(&job.id, &job.setup.name);
        Claim::take(&self.inner.busy, note, &[&survey.setup.source, &survey.setup.destination], &inside).map_err(|other| self.busy_with(&other))
    }

    /// The folders inside the job's folders where the scans met Charon's own folder.
    pub(crate) fn folders_inside(setup: &JobSetup, source: &Snapshot, destination: &Snapshot) -> Vec<PathBuf> {
        [(Side::Source, source), (Side::Destination, destination)]
            .into_iter()
            .flat_map(|(side, snapshot)| {
                let root = setup.root(side);
                snapshot.charon_folders().iter().filter(|found| !found.folder.is_empty()).map(move |found| root.join(&found.folder))
            })
            .collect()
    }

    /// The folders inside the job's folders that hold Charon's own folder, for a run about to
    /// carry out `survey`'s plan: where the scans met one, and where one is now in a folder
    /// whose contents the plan changes. A sync of a folder inside may have started since the
    /// scans (for a confirmed list, maybe hours ago) and made Charon's folder there: its
    /// notes must be seen before this run changes anything in its folder.
    ///
    /// The folders are taken as they are now, not as the scans saw them: since then a folder
    /// may have been replaced by a link, or a link by a folder. (Where the scans met one is
    /// checked the same way when the notes are read and left: see `run::Claim`.)
    fn folders_inside_now(survey: &Survey) -> Vec<PathBuf> {
        let mut inside: BTreeSet<PathBuf> = Self::folders_inside(&survey.setup, &survey.source, &survey.destination).into_iter().collect();
        let spellings = Spellings::new(&survey.source, &survey.destination);
        for side in [Side::Source, Side::Destination] {
            let changed: BTreeSet<&str> = survey.plan.actions.iter().filter(|action| action.changes(side)).flat_map(Action::folders).collect();
            // A folder comes before the folders inside it. One that isn't a folder of its own
            // here now (it's new to this side and not there yet, or it's a link) has nothing
            // inside it to look at: Charon never looks through a link.
            let mut not_folders: HashSet<&str> = HashSet::new();
            for key in changed {
                if Keys::ancestors(key).any(|above| not_folders.contains(above)) {
                    continue;
                }
                let folder = survey.setup.root(side).join(spellings.disk(side, key));
                if !fs::symlink_metadata(&folder).is_ok_and(|meta| meta.is_dir()) {
                    not_folders.insert(key);
                    continue;
                }
                // Only a folder that is Charon's own: one of the person's of that name is
                // left alone.
                let charons = folder.join(MARKER_FOLDER);
                if fs::symlink_metadata(&charons).is_ok_and(|meta| meta.is_dir()) && Scanner::looks_like_charons(&charons, MARKER_FOLDER, true) {
                    inside.insert(folder);
                }
            }
        }
        inside.into_iter().collect()
    }

    /// Who left a busy note, as the person knows them: a job of this Charon by its name, or
    /// another Charon (on another computer, by its name, when the note says).
    fn busy_with(&self, note: &BusyNote) -> String {
        if note.instance == self.inner.busy.instance() {
            return self.stored_job(&note.job).map(|job| job.setup.name).unwrap_or_else(|_| note.name.clone());
        }
        if note.host.is_empty() || note.from_this_computer() {
            "another Charon".into()
        } else {
            format!("Charon on {}", note.host)
        }
    }

    /// Ends a run that found another sync (`who`) at work in its folders right before it
    /// would have changed anything: nothing happened. A run nobody looks at is wanted again,
    /// and starts once that sync is done; a confirmed run is kept waiting by the one who
    /// started it (see `approve`). Returns false when the person stopped the run meanwhile:
    /// it then ends as stopped. (Syncs that waited for this one are started by the autopilot
    /// within seconds: starting them here could go back and forth between two waiting jobs.)
    pub(crate) fn not_begun(&self, job: &Job, crossing: &Crossing, who: &str) -> bool {
        let id = &job.id;
        let stopped = self.change_runtime(id, |rt| {
            if rt.cancel.as_ref().is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                return true;
            }
            rt.cancel = None;
            rt.progress = None;
            Self::look_again_if_changed(rt);
            if !crossing.approved {
                rt.wanted = Some(crossing.reason.clone());
                rt.waiting_for = Some(who.to_string());
            }
            false
        });
        if stopped {
            self.unfinished(job, crossing, Error::Cancelled);
            return false;
        }
        if !crossing.approved {
            self.inner.journal.info(id, &format!("waits for “{who}” to finish syncing these folders"));
        }
        self.publish(id);
        true
    }

    /// Which other jobs share a folder with `id` (asks the disk; done before locking).
    pub(crate) fn neighbours(&self, id: &JobId) -> Neighbours {
        let jobs = self.job_list();
        let Some(me) = jobs.iter().find(|job| &job.id == id) else {
            return Neighbours { mine: Self::nowhere(), others: HashMap::new() };
        };
        let others = jobs
            .iter()
            .filter(|other| other.id != me.id)
            .map(|other| (other.id.clone(), (other.setup.clone(), me.setup.shares_folders_with(&other.setup))))
            .collect();
        Neighbours { mine: me.setup.clone(), others }
    }

    /// The name of a running job that shares a folder with `job`, if there is one. A job
    /// whose setup changed since the neighbours were worked out counts as sharing.
    pub(crate) fn busy_neighbour(jobs: &[Job], runtime: &HashMap<JobId, JobRuntime>, job: &Job, neighbours: &Neighbours) -> Option<String> {
        jobs.iter()
            .filter(|other| other.id != job.id && runtime.get(&other.id).is_some_and(|rt| rt.is_running()))
            .find(|other| match neighbours.others.get(&other.id) {
                Some((setup, shares)) if *setup == other.setup && neighbours.mine == job.setup => *shares,
                _ => true,
            })
            .map(|other| other.setup.name.clone())
    }

    fn nowhere() -> JobSetup {
        JobSetup {
            name: String::new(),
            source: Default::default(),
            destination: Default::default(),
            mode: crate::model::SyncMode::Backup,
            conflict: crate::model::ConflictRule::Ask,
            trigger: crate::model::Trigger::Manual,
            excludes: Vec::new(),
            safe_delete: true,
            full_automation: false,
        }
    }

    /// Runs `work` on its own thread for a job marked as running. The thread never downloads
    /// files that are only in the cloud: one the cloud took back since the list was made is
    /// left alone, as the scan leaves such files alone.
    pub(crate) fn spawn_crossing(
        &self,
        job: Job,
        cancel: Arc<AtomicBool>,
        crossing: Crossing,
        work: impl FnOnce(&Engine, Job, Arc<AtomicBool>, Crossing) + Send + 'static,
    ) -> Result<()> {
        let engine = self.clone();
        let id = job.id.clone();
        let spawned = thread::Builder::new().name("charon-sync".into()).spawn(move || {
            never_download_on_this_thread();
            let (id, started_at, approved) = (job.id.clone(), Utc::now(), crossing.approved);
            let ended = std::panic::catch_unwind(AssertUnwindSafe(|| work(&engine, job, cancel, crossing)));
            if ended.is_err() {
                engine.after_a_panic(&id, started_at, approved);
            }
        });
        if let Err(e) = spawned {
            self.change_runtime(&id, |rt| {
                rt.cancel = None;
                rt.progress = None;
            });
            self.publish(&id);
            return Err(Error::Io(e));
        }
        Ok(())
    }

    /// A sync ended on a mistake in Charon itself (a panic) before it was recorded. What it
    /// carried isn't known: the job's record still has the mark set before anything was
    /// carried, so the next look doesn't trust the two-way memory, and the next sync waits
    /// for the person's OK.
    fn after_a_panic(&self, id: &JobId, started_at: DateTime<Utc>, approved: bool) {
        log::error!("a sync of job {id} ended on a panic");
        let mut summary = RunSummary::starting(started_at, approved);
        summary.finished_at = Utc::now();
        summary.error = Some(CRASHED.into());
        let held = self.change_job_anyway(id, |stored| {
            stored.last_run = Some(summary.clone());
            stored.needs_review = true;
            stored.held = Some(CRASHED.into());
        });
        if let Err(e) = held {
            log::error!("couldn't save that job {id} waits for an OK: {e}");
        }
        self.inner.journal.error(id, "the sync stopped on an error of Charon's own; the next sync waits for your OK");
        self.change_runtime(id, |rt| {
            rt.cancel = None;
            rt.stopped_by_person = false;
            rt.must_ask = true;
            rt.progress = None;
            rt.finished_at = Some(Instant::now());
            rt.problem = summary.error.clone();
            rt.last_checked = None;
        });
        if let Ok(job) = self.stored_job(id) {
            self.emit(EngineEvent::Finished(Finished { id: id.clone(), name: job.setup.name, summary }));
        }
        self.publish(id);
    }

    // ---------------------------------------------------------------- carrying out

    /// Carries out the plan of `survey`, makes it durable and records it. Runs on the
    /// job's sync thread, with the job marked as running and its folders claimed (`claim`,
    /// given up before the run's end is announced, so a sync waiting for it can start).
    pub(crate) fn cross(&self, job: &Job, survey: &Survey, cancel: &AtomicBool, crossing: &Crossing, claim: Claim) {
        let id = &job.id;
        let journal = &self.inner.journal;
        let started_at = Utc::now();
        let mut summary = RunSummary::starting(started_at, crossing.approved);
        let steps = survey.plan.work().count() as u64;
        journal.info(id, &format!("sync {}: {}", crossing.reason, plural(steps, "step")));
        self.change_runtime(id, |rt| rt.own_writes = Some(OwnWrites::of(&survey.plan, survey.folded, rt.own_writes.as_ref())));

        // Until this run is recorded, the job's record says files are being carried.
        if let Err(e) = self.change_job(id, |job| job.run_in_progress = true) {
            summary.error = Some(format!("Charon couldn't save its state before syncing ({e}), so it changed nothing."));
            drop(claim);
            self.finish(job, survey, crossing, summary, None);
            return;
        }

        let two_way = survey.setup.mode.is_two_way();
        let resolving = crossing.kind == ReviewKind::Resolve;
        let shores = Shores { source: survey.setup.source.clone(), destination: survey.setup.destination.clone() };
        let care = Care { safe_delete: survey.setup.safe_delete || resolving, keep_replaced: two_way || resolving, two_way };
        let observer = |progress: &Progress| self.report_progress(id, progress);
        let mut meter = ProgressMeter::for_plan(&survey.plan, &observer);
        let executor = Executor::new(&shores, &survey.source, &survey.destination, care, cancel).excluding(survey.filter.clone());
        let mut outcome = executor.run(&survey.plan, &mut meter);

        // The housekeeping the list named, only after a run that went all the way.
        let complete = !outcome.cancelled && outcome.stopped.is_none();
        let mut tidied = 0;
        if complete {
            for (side, leftovers) in &survey.housekeeping.leftovers {
                tidied += executor.tidy(*side, leftovers);
            }
        }
        let flushed = executor.flush(&mut outcome);
        // Old trash goes only after a list that said so was confirmed. A run nobody looked at
        // never empties it: the originals in there may be the only good copies of files a
        // broken app is slowly damaging (see `Guard::since_confirmed`).
        if complete && flushed.is_ok() && crossing.approved {
            for (side, look) in &survey.housekeeping.trash {
                let names: Vec<String> = look.expired.iter().map(|e| e.name.clone()).collect();
                Trash::sweep(survey.setup.root(*side), &names);
            }
        }

        summary.copied = outcome.copied;
        summary.deleted = outcome.deleted;
        summary.bytes = outcome.bytes;
        summary.conflicts = survey.plan.conflicts().len() as u64;
        summary.failed = outcome.failed.len() as u64;
        summary.failed_items = outcome.failed.iter().take(FAILED_NAMED).map(|(path, _)| survey.spelling(path)).collect();
        summary.skipped = outcome.skipped.len() as u64;
        summary.left_out = outcome.left_out.len() as u64;
        summary.kept = outcome.kept.len() as u64;
        // Places the look couldn't read, and files this run couldn't.
        summary.found_unreadable = outcome.unread.len() as u64;
        summary.unreadable = survey.plan.notes.iter().filter(|n| n.kind == NoteKind::Unreadable).count() as u64 + summary.found_unreadable;
        summary.cancelled = outcome.cancelled;
        let settled = if crossing.kind == ReviewKind::Sync { Self::settled_by_newest(survey, &outcome) } else { Vec::new() };
        summary.settled = settled.len() as u64;
        summary.unkept = outcome.unkept.len() as u64;
        summary.left_for_later = survey.left_for_later.actions.len() as u64;

        self.change_runtime(id, |rt| {
            if let Some(own) = rt.own_writes.as_mut() {
                own.add(outcome.settled.iter().map(|step| step.path.as_str()));
            }
        });

        let mut problems: Vec<String> = outcome.stopped.iter().cloned().collect();
        let recorded = match &flushed {
            Ok(()) => match self.remember(job, survey, &outcome, crossing) {
                Ok(()) => true,
                Err(problem) => {
                    problems.push(problem);
                    false
                }
            },
            Err(Error::Undone(reason)) => {
                problems.push(format!("{reason} Charon recorded nothing of this sync: its next look compares both folders afresh and waits for your OK."));
                false
            }
            Err(e) => {
                problems.push(format!("Charon couldn't make sure the changes reached the drive ({e}). Its next look compares both folders afresh."));
                false
            }
        };
        if !problems.is_empty() {
            summary.error = Some(problems.join(" "));
        }
        for (path, reason) in outcome.failed.iter().take(LOGGED_ITEMS) {
            journal.warn(id, &format!("{path}: {reason}"));
        }
        for (path, reason) in outcome.skipped.iter().chain(&outcome.left_out).take(LOGGED_ITEMS) {
            journal.info(id, &format!("{path}: {reason}"));
        }
        let unread: Vec<Place> = outcome
            .unread
            .iter()
            .map(|item| Place::new(NoteKind::Unreadable, item.side, survey.snapshot(item.side).spelling(&item.key), item.reason.clone()))
            .collect();
        for place in unread.iter().take(LOGGED_ITEMS) {
            journal.info(id, &format!("{}: couldn't be read, so it's left alone until it can be ({})", place.path, place.reason));
        }
        for (path, to) in settled.iter().take(LOGGED_ITEMS) {
            journal.info(
                id,
                &format!(
                    "{path}: changed on both sides; Newest wins kept the {} version, and the {}'s is in .charon-trash",
                    to.opposite().word(),
                    to.word()
                ),
            );
        }
        for path in outcome.unrecorded.iter().take(LOGGED_ITEMS) {
            journal.info(
                id,
                &format!(
                    "{path}: copied, but by the end of the sync its original was gone or an older version (moved or removed meanwhile, or a change its drive lost), so Charon doesn't count it as synced and compares it afresh next time"
                ),
            );
        }
        for (path, what) in outcome.unkept.iter().take(LOGGED_ITEMS) {
            journal.info(id, &format!("{path}: copied without its {what}, which the receiving drive can't keep"));
        }
        if tidied > 0 {
            journal.info(id, &format!("moved {} of interrupted syncs into .charon-trash", plural(tidied, "leftover file")));
        }
        self.change_runtime(id, |rt| {
            for side in &outcome.no_links {
                match side {
                    Side::Source => rt.no_links.0 = true,
                    Side::Destination => rt.no_links.1 = true,
                }
            }
            // An item this run carried after all was taken, and read. One refused waits for
            // a change, one unread until it can be read.
            if !rt.refused.is_empty() || !rt.unread.is_empty() {
                let carried: HashSet<&str> = outcome.settled.iter().map(|step| step.path.as_str()).collect();
                rt.refused.retain(|(_, key), _| !carried.contains(key.as_str()));
                rt.unread.retain(|(_, key), _| !carried.contains(key.as_str()));
            }
            for refused in &outcome.refused {
                rt.refused.insert((refused.side, refused.key.clone()), (refused.clone(), survey.volume(refused.side).cloned()));
            }
            for unread in &outcome.unread {
                rt.unread.insert((unread.side, unread.key.clone()), unread.clone());
            }
        });
        // The drives, with what this run found out about links on them.
        let drives = match (survey.volume(Side::Source), survey.volume(Side::Destination)) {
            (Some(source), Some(destination)) if recorded => {
                let mut drives = Drives { source: source.clone(), destination: destination.clone() };
                for side in &outcome.no_links {
                    match side {
                        Side::Source => drives.source.refuses_links = true,
                        Side::Destination => drives.destination.refuses_links = true,
                    }
                }
                Some(drives)
            }
            _ => None,
        };
        let left = !outcome.failed.is_empty() || !outcome.skipped.is_empty() || !outcome.left_out.is_empty() || !unread.is_empty();
        drop(claim);
        self.finish(job, survey, crossing, summary, Some(Recorded { recorded, drives, destroyed: outcome.destroyed, left, unread }));
    }

    /// The conflicts this run settled by Newest wins, with the side the newer version was
    /// copied to (the other version went to .charon-trash).
    fn settled_by_newest<'a>(survey: &'a Survey, outcome: &Outcome) -> Vec<(&'a str, Side)> {
        let done: BTreeSet<&str> = outcome.settled.iter().map(|step| step.path.as_str()).collect();
        survey
            .plan
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::Copy { to, path, settles_conflict: true, .. } if done.contains(path.as_str()) => Some((path.as_str(), *to)),
                _ => None,
            })
            .collect()
    }

    /// Saves what this run did: the markers, then the memory (two-way), or the markers of a
    /// Mirror or Backup. Each happens only once the run's changes are on the drives, and only
    /// while both folders still carry the markers the survey found: otherwise another drive
    /// came in under the same name, and recording this sync would make the next one treat
    /// that drive's differences as changes to carry. The error says why nothing was recorded.
    fn remember(&self, job: &Job, survey: &Survey, outcome: &Outcome, crossing: &Crossing) -> std::result::Result<(), String> {
        let id = &job.id;
        let setup = &survey.setup;
        let now = Markers::read(&setup.source, &setup.destination, id);
        if let Some(side) = now.changed_since(&survey.markers) {
            let _ = self.inner.baselines.forget(id);
            return Err(format!(
                "Charon's marker in the {} folder changed during the sync, so it may be another drive under the same name. \
                 Charon recorded nothing of this sync: its next look compares both folders afresh and waits for your OK.",
                side.word()
            ));
        }
        if !setup.mode.is_two_way() {
            return self.remember_one_way(job, survey, outcome);
        }
        let memory = survey.memory.clone().unwrap_or_default();
        let next = match crossing.kind {
            ReviewKind::Sync => Baseline::after_run(&survey.baseline, &survey.source, &survey.destination, &survey.plan, &outcome.settled),
            // Settling one item changes the memory of that item only, when there is a memory.
            // Whatever under the settled item didn't make it (a failed copy, a stop) has no
            // record afterwards, so the next look copies it rather than removing it.
            ReviewKind::Resolve if survey.trusted => {
                let mut baseline = memory.baseline.clone();
                if let Some(key) = &crossing.resolving {
                    baseline.forget_within(key);
                }
                baseline.record(&outcome.settled);
                baseline
            }
            // Without a memory it can trust (a sync didn't finish, a marker changed), settling
            // one item can't be recorded. And the memory from before must never be trusted
            // again: a file restored since the interrupted sync would look removed on its
            // side, and the next sync would remove its only copy. So it is forgotten: until
            // a whole list is confirmed, each look compares both folders afresh and removes
            // nothing.
            ReviewKind::Resolve => {
                return self.inner.baselines.forget(id).map_err(|e| {
                    format!("The item was settled, but Charon couldn't put aside what it remembers of the last sync ({e}), so it waits for your OK before the next sync.")
                });
            }
        };
        let (tokens, generation) = match (&memory.tokens, survey.trusted) {
            (Some(tokens), true) => (tokens.clone(), memory.generation + 1),
            _ => ((Marker::fresh_token(), Marker::fresh_token()), 1),
        };
        let fail = |what: String| {
            // Without both markers and the memory, the next look starts afresh (it removes nothing).
            let _ = self.inner.baselines.forget(id);
            format!("The changes were synced, but Charon {what}, so it won't remember this sync: its next look compares both folders afresh and waits for your OK.")
        };
        for (side, token) in [(Side::Source, &tokens.0), (Side::Destination, &tokens.1)] {
            if let Err(e) = Marker::new(id, side, token, generation).write(setup.root(side)) {
                return Err(fail(format!("couldn't leave its marker in the {} ({e})", side.word())));
            }
        }
        let memory = Memory { baseline: next, tokens: Some(tokens), generation, folded: survey.folded, distrust: None, earlier: Default::default() };
        self.inner.baselines.save(id, setup, &memory).map_err(|e| fail(format!("couldn't save what was synced ({e})")))
    }

    /// Mirror and Backup: the destination's marker, then the source's where Charon can
    /// write there. Each is written to the folder before the job's record names it, so a
    /// record that couldn't be saved leaves the next look asking, never trusting.
    fn remember_one_way(&self, job: &Job, survey: &Survey, outcome: &Outcome) -> std::result::Result<(), String> {
        let id = &job.id;
        let setup = &survey.setup;
        let token = job.destination_token.clone().unwrap_or_else(Marker::fresh_token);
        if let Err(e) = Marker::new(id, Side::Destination, &token, 0).write(&setup.destination) {
            // The job keeps the token it had: the next look then finds the folder without
            // it and asks.
            return Err(format!(
                "The files were synced, but Charon couldn't leave its marker in the destination ({e}), so it can't recognise that folder next time and will ask first."
            ));
        }
        self.remember_landings(job, survey, outcome, &token);
        let mark = match &job.source_mark {
            Some(mark) => Mark { token: mark.token.clone(), generation: mark.generation + 1 },
            None => Mark { token: Marker::fresh_token(), generation: 1 },
        };
        // A source Charon can't write to (a card, a read-only share) simply has no marker;
        // its record then stays as it was, and so does what the next look expects.
        let source_marked = match Marker::new(id, Side::Source, &mark.token, mark.generation).write(&setup.source) {
            Ok(()) => true,
            Err(e) => {
                log::info!("no marker in the source of {} ({e})", setup.name);
                false
            }
        };
        self.change_job(id, |job| {
            job.destination_token = Some(token);
            if source_marked {
                job.source_mark = Some(mark);
            }
        })
        .map_err(|e| format!("The files were synced, but Charon couldn't save its record of this sync ({e}), so it will ask before the next one."))
    }

    /// Mirror and Backup: records the copies whose dates didn't stick on the destination,
    /// so the next look doesn't copy them again while neither side changes (see
    /// `store::Landings`). Losing this record only means copying those files once more.
    fn remember_landings(&self, job: &Job, survey: &Survey, outcome: &Outcome, destination_token: &str) {
        let (landed, undated) = Landings::after_run(&survey.landed, &outcome.settled, &survey.source);
        if undated > 0 {
            self.inner.journal.info(
                &job.id,
                &format!(
                    "{} got other dates on the destination's drive than in the source (the drive doesn't keep the dates Charon sets); Charon remembers them, so they aren't copied again until one side changes",
                    plural(undated, "copied file")
                ),
            );
        }
        if let Err(e) = self.inner.landings.save(&job.id, &survey.setup, survey.folded, destination_token, &landed) {
            log::warn!("couldn't save the copies whose dates didn't stick for job {}: {e}", job.id);
            let _ = self.inner.landings.forget(&job.id);
        }
    }

    /// Records the end of a run: the job's record, the log, the live state, and the window.
    fn finish(&self, job: &Job, survey: &Survey, crossing: &Crossing, mut summary: RunSummary, recorded: Option<Recorded>) {
        let id = &job.id;
        // A Stop from the person means something looked wrong: the next sync waits for an
        // OK. A run stopped only because Charon quit carries on as before next time.
        let stopped_by_person =
            self.runtime().get(id).is_some_and(|rt| rt.stopped_by_person) || (summary.cancelled && !self.is_shutting_down());
        let syncing = crossing.kind == ReviewKind::Sync;
        let confirmed_clean = syncing && crossing.approved && summary.error.is_none() && !summary.cancelled && !stopped_by_person;
        // Files were carried, but what they did couldn't be recorded (or another drive came
        // in): the next sync waits for an OK.
        let unrecorded = recorded.as_ref().is_some_and(|recorded| !recorded.recorded);
        summary.finished_at = Utc::now();
        let finished_at = summary.finished_at;
        // Full automation: what a run nobody looked at removed or replaced joins the long
        // count (see `Unattended`). If that can't be saved, the guard can't see damage that
        // spreads slowly, so the next sync waits for the person's OK.
        let uncounted = match recorded.as_ref().filter(|_| !crossing.approved) {
            Some(recorded) => {
                let (source, destination) = &recorded.destroyed;
                self.inner.unattended.note(id, finished_at, source, destination, self.settings().trash_days).err()
            }
            None => None,
        };
        let saved = self.change_job_anyway(id, |stored| {
            stored.last_run = Some(summary.clone());
            if summary.error.is_none() && !summary.cancelled {
                stored.last_synced = Some(finished_at);
            }
            if let Some(recorded) = &recorded {
                // A run that couldn't be made durable leaves the mark: the next look won't
                // trust the two-way memory.
                stored.run_in_progress = !recorded.recorded;
                if syncing {
                    // What still waits after this run is for the next look to say.
                    stored.waiting_since = None;
                }
                // A settled conflict says nothing about the drives as a whole.
                if let Some(drives) = recorded.drives.as_ref().filter(|_| syncing) {
                    stored.drives = Some(drives.clone());
                }
                if !crossing.approved {
                    stored.note_automatic(finished_at, &recorded.destroyed.0, &recorded.destroyed.1);
                }
            }
            // Settling one conflict doesn't confirm the rest of the job's changes.
            if stopped_by_person {
                stored.needs_review = true;
                stored.held = Some(STOPPED.into());
            } else if unrecorded {
                stored.needs_review = true;
                stored.held = Some(UNRECORDED.into());
            } else if uncounted.is_some() {
                stored.needs_review = true;
                stored.held = Some(UNCOUNTED.into());
            } else if confirmed_clean {
                stored.needs_review = false;
                stored.held = None;
                // The person just confirmed a list that counted what automatic syncs did
                // today (see `Guard::concerns`): the count starts again from here.
                if stored.setup.full_automation {
                    stored.automatic_changes.clear();
                }
            }
        });
        if let Err(e) = saved {
            log::error!("couldn't save the result of a sync of {}: {e}", job.setup.name);
            self.inner.journal.error(id, &format!("Charon couldn't save the result of this sync ({e}); it holds while Charon runs"));
        }
        if let Some(e) = uncounted {
            log::error!("couldn't count what an automatic sync of {} changed: {e}", job.setup.name);
            self.inner.journal.error(id, &format!("Charon couldn't count what this automatic sync removed or replaced ({e}); the next sync waits for your OK"));
        }
        if confirmed_clean {
            // The person saw and confirmed a list: the long count starts again from here.
            if let Err(e) = self.inner.unattended.forget(id) {
                log::warn!("couldn't start the count of automatic changes of {} again: {e}", job.setup.name);
            }
        }
        let line = summary.describe();
        match (&summary.error, summary.failed) {
            (Some(_), _) => self.inner.journal.error(id, &line),
            (None, 0) => self.inner.journal.info(id, &line),
            (None, _) => self.inner.journal.warn(id, &line),
        }
        if stopped_by_person {
            self.inner.journal.info(id, "stopped: the next sync waits for your OK");
        } else if summary.cancelled {
            self.inner.journal.info(id, "stopped because Charon quit");
        }

        // Live state last: whoever sees the run end finds its results.
        let carried = recorded.is_some();
        let left = recorded.as_ref().is_some_and(|r| r.left);
        let unread = recorded.map(|r| r.unread).unwrap_or_default();
        let mut look = Look::of(survey);
        let resolved: Vec<String> = survey.plan.actions.iter().map(|a| a.path().to_string()).collect();
        self.change_runtime(id, |rt| {
            // Something besides the run's own items changed while it ran: it's looked at
            // again at once (see below).
            let changed_meanwhile = rt.own_writes.as_ref().is_some_and(OwnWrites::others_changed);
            rt.cancel = None;
            rt.stopped_by_person = false;
            if stopped_by_person || unrecorded {
                rt.must_ask = true;
            } else if confirmed_clean {
                rt.must_ask = false;
            }
            rt.progress = None;
            rt.finished_at = Some(Instant::now());
            rt.problem = summary.error.clone();
            // An automatic run that never got to carry anything: the changes its look found
            // wait for a newer look before a run tries them again (see `has_pending`).
            if !carried && !crossing.approved {
                rt.tried = rt.look.as_ref().map(|look| look.at);
            } else if carried {
                rt.tried = None;
            }
            match crossing.kind {
                // A run that never got to carry anything leaves the last look as it was.
                _ if !carried => {}
                ReviewKind::Sync => {
                    // Only the removals this run left for a later one still wait.
                    let later = &survey.left_for_later;
                    look.items = later.summary().changes();
                    look.bytes = 0;
                    look.removals = Default::default();
                    for side in [Side::Source, Side::Destination] {
                        look.removals.add(side, later.removed_on(side));
                    }
                    look.replacements = Default::default();
                    rt.reminder.count = 0;
                    rt.look = Some(look);
                }
                ReviewKind::Resolve => {
                    if let Some(look) = rt.look.as_mut() {
                        look.conflicts.retain(|c| !resolved.contains(&c.key));
                    }
                }
            }
            // Files this run couldn't read show at once, as the next look will list them.
            if let Some(look) = rt.look.as_mut() {
                for place in unread {
                    if !look.unreadable.contains(&place) {
                        look.unreadable.push(place);
                    }
                }
                look.unreadable.truncate(PLACES_SHOWN);
            }
            // After a stopped run, a confirmed run that left something, or a run during which
            // something else changed, look again at once, so the row shows what's still
            // waiting. Runs by themselves wait for the regular look otherwise: a file that
            // fails every time mustn't start a run every few seconds.
            if summary.cancelled || changed_meanwhile || (crossing.approved && (left || summary.error.is_some())) {
                rt.last_checked = None;
            } else {
                rt.last_checked = Some(Instant::now());
            }
        });
        self.emit(EngineEvent::Finished(Finished { id: id.clone(), name: job.setup.name.clone(), summary }));
        self.publish(id);
        self.start_waiting_confirmed();
    }

    /// Starts the confirmed syncs that waited for another sync, where nothing stands in their
    /// way anymore (a sync just ended).
    fn start_waiting_confirmed(&self) {
        let waiting: Vec<JobId> =
            self.runtime().iter().filter(|(_, rt)| rt.confirmed.is_some() && !rt.is_running()).map(|(id, _)| id.clone()).collect();
        for id in waiting {
            self.start_confirmed(&id);
        }
    }

    /// Whether a change at `path` in the job's folders, heard while the job syncs or a
    /// moment after, counts as a change now (see `note_change`). Most such changes are the
    /// sync's own writes. A change anywhere else is the person's, or another app's: during
    /// the sync it's kept for its end, which then has the folders looked at again at once.
    pub fn note_change_while_syncing(&self, id: &JobId, path: &Path) -> bool {
        let Ok(job) = self.stored_job(id) else { return false };
        let key = match self.change_at(&job, path) {
            // The sync writes into the folder itself too.
            ChangeAt::Folder | ChangeAt::Ignored => return false,
            ChangeAt::Item(key) => key,
        };
        let counts = self.change_runtime(id, |rt| {
            let running = rt.is_running();
            match rt.own_writes.as_mut() {
                Some(own) if own.is_own(&key) => false,
                Some(own) if running => {
                    own.note_other(&key);
                    false
                }
                _ => !running,
            }
        });
        counts && self.note_change(id, path)
    }

    /// A run that ended without carrying anything (held back, or it found another sync at
    /// work): if something changed in the folders while it looked, they're looked at again
    /// at once.
    fn look_again_if_changed(rt: &mut JobRuntime) {
        if rt.own_writes.take().is_some_and(|own| own.others_changed()) {
            rt.last_checked = None;
        }
    }

    /// Forgets which items the job's last sync wrote, once that's long past.
    pub(crate) fn forget_own_writes(rt: &mut JobRuntime) {
        if !rt.is_running() && rt.finished_at.is_none_or(|at| at.elapsed() >= OWN_WRITES_KEPT) {
            rt.own_writes = None;
        }
    }

    /// A run that ended before carrying anything (a folder went away, it was stopped).
    fn unfinished(&self, job: &Job, crossing: &Crossing, error: Error) {
        let mut summary = RunSummary::starting(Utc::now(), crossing.approved);
        match error {
            Error::Cancelled => summary.cancelled = true,
            e => summary.error = Some(e.to_string()),
        }
        let empty = Survey::empty(job);
        self.finish(job, &empty, crossing, summary, None);
    }

    /// Holds back a run nobody looked at: nothing is touched, and the job waits for the
    /// person's OK, saying why.
    fn hold(&self, job: &Job, survey: &Survey) {
        let id = &job.id;
        let mut reason = survey.refusal.clone().unwrap_or_else(|| survey.holds[0].clone());
        let more = survey.holds.len().saturating_sub(1);
        if more > 0 {
            reason.push_str(&format!(" ({} more {} in the list.)", more, if more == 1 { "reason" } else { "reasons" }));
        }
        self.inner.journal.warn(id, &format!("held back for your OK: {reason}"));
        if let Err(e) = self.change_job_anyway(id, |job| {
            job.needs_review = true;
            job.held = Some(reason.clone());
        }) {
            log::error!("couldn't save that {} was held back: {e}", job.setup.name);
            self.inner.journal.error(id, &format!("Charon couldn't save that this job waits for your OK ({e}); it keeps waiting while Charon runs"));
        }
        self.note_look(job, survey, None);
        self.change_runtime(id, |rt| {
            rt.cancel = None;
            rt.progress = None;
            rt.finished_at = Some(Instant::now());
            rt.must_ask = true;
            Self::look_again_if_changed(rt);
        });
        self.emit(EngineEvent::Held { id: id.clone(), name: job.setup.name.clone(), reason });
        self.publish(id);
        self.start_waiting_confirmed();
    }

    /// Keeps the latest progress and sends it to the window now and then.
    fn report_progress(&self, id: &JobId, progress: &Progress) {
        let send = self.change_runtime(id, |rt| {
            rt.progress = Some(progress.clone());
            let due = rt.last_progress_sent.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL);
            let finished = progress.phase == Phase::Finishing;
            if due || finished {
                rt.last_progress_sent = Some(Instant::now());
            }
            due || finished
        });
        if send {
            self.emit(EngineEvent::Progress { id: id.clone(), progress: progress.clone() });
        }
    }
}

/// What a finished run left for the job's record.
struct Recorded {
    /// The run's changes are on the drives.
    recorded: bool,
    drives: Option<Drives>,
    /// Files removed or replaced on the source and on the destination.
    destroyed: (Vec<String>, Vec<String>),
    /// Steps failed, were skipped or left out, or files couldn't be read.
    left: bool,
    /// The files this run couldn't read, as places for the row.
    unread: Vec<Place>,
}

impl Survey {
    /// A survey of nothing, for a run that ended before it could look.
    fn empty(job: &Job) -> Survey {
        Survey {
            setup: job.setup.clone(),
            source: Default::default(),
            destination: Default::default(),
            folded: false,
            filter: ExcludeFilter::none(),
            markers: Markers { source: Found::Missing, destination: Found::Missing },
            memory: None,
            trusted: false,
            baseline: Baseline::new(),
            landed: Baseline::new(),
            plan: Default::default(),
            left_for_later: Default::default(),
            agreed: 0,
            refusal: None,
            warnings: Vec::new(),
            holds: Vec::new(),
            needs_tick: false,
            housekeeping: Default::default(),
            made_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConflictRule, JobStatus, SyncMode, Trigger};
    use crate::SyncStart;
    use crate::scan::{Entry, Snapshot, Volume};
    use chrono::DateTime;
    use std::path::Path;

    fn minutes(n: i64) -> chrono::Duration {
        chrono::Duration::minutes(n)
    }

    /// A Mirror that runs by itself whenever its drive is connected, synced once.
    fn synced_mirror(data: &Path, src: &Path, dst: &Path) -> (Engine, JobId) {
        for folder in [src, dst] {
            std::fs::create_dir_all(folder).unwrap();
        }
        for name in ["a.txt", "b.txt", "c.txt", "gone.txt"] {
            std::fs::write(src.join(name), name).unwrap();
        }
        let engine = Engine::open(data, Arc::new(|_| {})).unwrap();
        let setup = JobSetup {
            name: "Photos".into(),
            source: src.to_path_buf(),
            destination: dst.to_path_buf(),
            mode: SyncMode::Mirror,
            conflict: ConflictRule::Ask,
            trigger: Trigger::DriveConnected,
            excludes: JobSetup::default_excludes(),
            safe_delete: true,
            full_automation: true,
        };
        let id = engine.add_job(setup).unwrap().job.id;
        let review = engine.prepare_review(&id).unwrap();
        engine.approve(&id, &review.review_id, true).unwrap();
        while engine.is_running(&id) {
            thread::sleep(Duration::from_millis(10));
        }
        (engine, id)
    }

    /// Puts `snapshot` of `folder` on a network share: the folder's drive, as if it were one.
    fn on_a_share(snapshot: &mut Snapshot, folder: &Path) {
        snapshot.facts_mut().volume = Some(Volume { network: true, ..Volume::of(folder) });
    }

    /// A run nobody looks at, as the autopilot starts one, with the source folder on a
    /// network share. The folders are on this computer's own drive, so the run's look notes
    /// nothing by itself: what a look at the share notes is added, as of `missing_since`.
    fn run_from_a_share(engine: &Engine, id: &JobId, missing_since: DateTime<Utc>) {
        let (job, cancel) = engine
            .begin_run(id, |job, _| {
                assert!(job.runs_by_itself());
                Ok(())
            })
            .unwrap();
        let mut survey = engine.survey(&job, &cancel, Purpose::Automatic).unwrap();
        assert!(survey.holds.is_empty(), "{:?}", survey.holds);
        on_a_share(&mut survey.source, &job.setup.source);
        engine.change_missing(&job, |missing| missing.note(&survey.plan, &survey.source, &survey.destination, missing_since));
        engine.hold_back_recent_removals(&job, &mut survey);
        let crossing = Crossing { kind: ReviewKind::Sync, approved: false, reason: "drive connected".into(), resolving: None };
        let claim = engine.claim_folders(&job, &survey).unwrap();
        engine.cross(&job, &survey, &cancel, &crossing, claim);
    }

    #[test]
    fn removals_left_for_later_stay_in_the_jobs_row_and_are_carried_out_after_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
        let (engine, id) = synced_mirror(&data, &src, &dst);
        std::fs::remove_file(src.join("gone.txt")).unwrap();

        // The share has shown the file missing for 9 minutes: the removal waits.
        let start = Utc::now();
        run_from_a_share(&engine, &id, start - minutes(9));
        assert!(dst.join("gone.txt").exists());
        let row = engine.job(&id).unwrap();
        assert_eq!(row.status, JobStatus::Pending, "never “In sync” while a removal waits");
        assert_eq!(row.pending.map(|pending| pending.items), Some(1));
        assert!(!engine.has_pending(&id), "a removal that waits starts no run by itself");
        assert!(engine.log(&id, 10).iter().any(|line| line.text.starts_with("left 1 removal for a later sync")));
        assert_eq!(row.job.last_run.as_ref().map(|run| run.left_for_later), Some(1), "the run doesn't say “already in sync”");
        assert!(!engine.take_follow_up_at(&id, start), "not due yet");

        // Charon restarts (a drive-connected job may not sync again for days): the wait
        // goes on from when the file first went missing.
        drop(engine);
        let engine = Engine::open(&data, Arc::new(|_| {})).unwrap();
        let kept = engine.runtime().get(&id).map(|rt| rt.missing.kept()).unwrap();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].left_for_later && kept[0].since == start - minutes(9));
        assert!(!engine.take_follow_up_at(&id, start + minutes(60)), "no look since the launch has found it missing yet");
        // A look after the launch finds the file missing still.
        let job = engine.stored_job(&id).unwrap();
        let mut share: Snapshot = ["a.txt", "b.txt", "c.txt"].into_iter().map(|name| (name.to_string(), Entry::file(5, 1))).collect();
        on_a_share(&mut share, &src);
        let mine: Snapshot = [("gone.txt".to_string(), Entry::file(8, 1))].into_iter().collect();
        let mut plan = Plan::default();
        plan.push(Action::delete(Side::Destination, "gone.txt", Entry::file(8, 1), None));
        engine.change_missing(&job, |missing| missing.note(&plan, &share, &mine, start));
        assert!(!engine.take_follow_up_at(&id, start + minutes(1)));
        assert!(engine.take_follow_up_at(&id, start + minutes(2)), "the job syncs once more, whatever its trigger");
        assert!(!engine.take_follow_up_at(&id, start + minutes(3)), "once");

        // That run carries out the removal.
        run_from_a_share(&engine, &id, start - minutes(11));
        assert!(!dst.join("gone.txt").exists());
        let row = engine.job(&id).unwrap();
        assert_eq!(row.status, JobStatus::InSync);
        assert!(row.pending.is_none());
    }

    #[test]
    fn a_sync_that_ends_on_a_panic_waits_for_an_ok_and_distrusts_its_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let engine = Engine::open(tmp.path().join("data"), Arc::new(|_: EngineEvent| {})).unwrap();
        let setup = JobSetup {
            name: "Panics".into(),
            source: a,
            destination: b,
            mode: SyncMode::TwoWay,
            conflict: ConflictRule::Ask,
            trigger: Trigger::Manual,
            excludes: Vec::new(),
            safe_delete: true,
            full_automation: true,
        };
        let id = engine.add_job(setup).unwrap().job.id;
        engine.change_job(&id, |job| job.needs_review = false).unwrap();
        assert!(engine.stored_job(&id).unwrap().runs_by_itself());

        // A run starts carrying files, then hits a mistake in Charon itself.
        let (job, cancel) = engine.begin_run(&id, |_, _| Ok(())).unwrap();
        let crossing = Crossing { kind: ReviewKind::Sync, approved: false, reason: "a test".into(), resolving: None };
        engine
            .spawn_crossing(job, cancel, crossing, |engine, job, _, _| {
                engine.change_job(&job.id, |job| job.run_in_progress = true).unwrap();
                panic!("a mistake in the middle of a sync");
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.is_running(&id) {
            assert!(Instant::now() < deadline, "the job stayed marked as syncing");
            thread::sleep(Duration::from_millis(10));
        }

        let view = engine.job(&id).unwrap();
        assert_eq!(view.job.held.as_deref(), Some(CRASHED));
        assert!(view.job.needs_review && !view.runs_by_itself);
        assert!(view.job.run_in_progress, "the next look doesn't trust the two-way memory");
        assert_eq!(view.job.last_run.as_ref().and_then(|run| run.error.as_deref()), Some(CRASHED));
        assert_ne!(view.status, JobStatus::Syncing);
        assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Review);
        assert!(engine.sync_all().contains(&id), "Sync all asks for it");
    }
}
