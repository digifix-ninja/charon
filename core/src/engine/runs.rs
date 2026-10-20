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

use crate::engine::runtime::MISSING_FOR;
use crate::engine::survey::{Purpose, Survey};
use crate::engine::{Engine, EngineEvent, JobRuntime, Look};
use crate::error::{Error, Result};
use crate::model::{plural, Drives, Finished, Job, JobId, JobSetup, Mark, Phase, Progress, ReviewKind, RunSummary};
use crate::plan::{Action, Baseline, NoteKind, Side};
use crate::run::{Care, Executor, Found, Marker, Markers, Outcome, ProgressMeter, Shores, Trash};
use crate::scan::never_download_on_this_thread;
use crate::store::{Landings, Memory};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
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
    /// list of changes first. Works for paused jobs too: pausing only stops automatic runs.
    pub fn sync_now(&self, id: &JobId) -> Result<SyncStart> {
        let job = self.stored_job(id)?;
        if !job.runs_by_itself() {
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

    /// Stops a running sync (what's already carried stays). The next sync of the job waits
    /// for the person's OK.
    pub fn cancel(&self, id: &JobId) {
        if let Some(rt) = self.runtime().get_mut(id) {
            if let Some(cancel) = &rt.cancel {
                cancel.store(true, Ordering::Relaxed);
                rt.stopped_by_person = true;
            }
        }
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

    /// Starts the job's wanted run, if it has one. While another job uses the folders it
    /// keeps waiting (the row says for which job); any other refusal drops it.
    pub fn start_wanted(&self, id: &JobId) {
        let Some(reason) = self.runtime().get(id).and_then(|rt| rt.wanted.clone()) else { return };
        let result = self.start_unreviewed(id, &reason);
        let (now_waiting, changed) = self.change_runtime(id, |rt| {
            let before = rt.waiting_for.clone();
            match &result {
                Err(Error::FoldersBusy(name)) => rt.waiting_for = Some(name.clone()),
                Err(Error::Busy) => {}
                _ => {
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
        let (job, cancel) = self.begin_run(id, |job, rt| {
            if !job.runs_by_itself() || rt.must_ask {
                return Err(Error::Invalid("This job waits for your OK before it syncs.".into()));
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
                engine.cross(&job, &survey, &cancel, &crossing);
            }
            Err(e) => engine.unfinished(&job, &crossing, e),
        })
    }

    /// Leaves for a later run the removals that follow an item missing for only a few
    /// minutes on a drive that may show changes late (see `Missing`), and says so in the log.
    fn hold_back_recent_removals(&self, job: &Job, survey: &mut Survey) {
        let held = self.change_runtime(&job.id, |rt| rt.missing.hold_back_recent(&mut survey.plan, &survey.source, &survey.destination, Instant::now()));
        if held > 0 {
            self.inner.journal.info(
                &job.id,
                &format!(
                    "left {} for a later sync: the items went missing less than {} minutes ago on a network share or a cloud drive, which can show changes late",
                    plural(held, "removal"),
                    MISSING_FOR.as_secs() / 60
                ),
            );
        }
    }

    /// Marks the job as running, if it may run now: it isn't running already, and no
    /// running job uses one of its folders. `check` adds the caller's own conditions.
    pub(crate) fn begin_run(&self, id: &JobId, check: impl FnOnce(&Job, &mut JobRuntime) -> Result<()>) -> Result<(Job, Arc<AtomicBool>)> {
        let neighbours = self.neighbours(id);
        let state = self.state();
        let job = state.jobs.iter().find(|job| &job.id == id).cloned().ok_or_else(|| Error::UnknownJob(id.clone()))?;
        let mut runtime = self.runtime();
        if self.is_shutting_down() {
            return Err(Error::Invalid("Charon is quitting.".into()));
        }
        if runtime.get(id).is_some_and(|rt| rt.is_running()) {
            return Err(Error::Busy);
        }
        if let Some(name) = Self::busy_neighbour(&state.jobs, &runtime, &job, &neighbours) {
            return Err(Error::FoldersBusy(name));
        }
        let rt = runtime.entry(id.clone()).or_default();
        check(&job, rt)?;
        let cancel = Arc::new(AtomicBool::new(false));
        rt.cancel = Some(cancel.clone());
        rt.stopped_by_person = false;
        rt.progress = Some(Progress::scanning());
        rt.problem = None;
        // A look at folders that are about to change would be wrong at once.
        rt.stop_looking();
        Ok((job, cancel))
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
    /// job's sync thread, with the job marked as running.
    pub(crate) fn cross(&self, job: &Job, survey: &Survey, cancel: &AtomicBool, crossing: &Crossing) {
        let id = &job.id;
        let journal = &self.inner.journal;
        let started_at = Utc::now();
        let mut summary = RunSummary::starting(started_at, crossing.approved);
        let steps = survey.plan.work().count() as u64;
        journal.info(id, &format!("sync {}: {}", crossing.reason, plural(steps, "step")));

        // Until this run is recorded, the job's record says files are being carried.
        if let Err(e) = self.change_job(id, |job| job.run_in_progress = true) {
            summary.error = Some(format!("Charon couldn't save its state before syncing ({e}), so it changed nothing."));
            self.finish(job, survey, crossing, summary, None);
            return;
        }

        let two_way = survey.setup.mode.is_two_way();
        let resolving = crossing.kind == ReviewKind::Resolve;
        let shores = Shores { source: survey.setup.source.clone(), destination: survey.setup.destination.clone() };
        let care = Care { safe_delete: survey.setup.safe_delete || resolving, keep_replaced: two_way || resolving };
        let observer = |progress: &Progress| self.report_progress(id, progress);
        let mut meter = ProgressMeter::for_plan(&survey.plan, &observer);
        let executor = Executor::new(&shores, &survey.source, &survey.destination, care, cancel);
        let outcome = executor.run(&survey.plan, &mut meter);

        // The housekeeping the list named, only after a run that went all the way.
        let complete = !outcome.cancelled && outcome.stopped.is_none();
        let mut tidied = 0;
        if complete {
            for (side, leftovers) in &survey.housekeeping.leftovers {
                tidied += executor.tidy(*side, leftovers);
            }
        }
        let flushed = executor.flush();
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
        summary.skipped = outcome.skipped.len() as u64;
        summary.left_out = outcome.left_out.len() as u64;
        summary.kept = outcome.kept.len() as u64;
        summary.unreadable = survey.plan.notes.iter().filter(|n| n.kind == NoteKind::Unreadable).count() as u64;
        summary.cancelled = outcome.cancelled;
        let settled = if crossing.kind == ReviewKind::Sync { Self::settled_by_newest(survey, &outcome) } else { Vec::new() };
        summary.settled = settled.len() as u64;
        summary.unkept = outcome.unkept.len() as u64;

        let mut problems: Vec<String> = outcome.stopped.iter().cloned().collect();
        let recorded = match &flushed {
            Ok(()) => match self.remember(job, survey, &outcome, crossing) {
                Ok(()) => true,
                Err(problem) => {
                    problems.push(problem);
                    false
                }
            },
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
        for (path, what) in outcome.unkept.iter().take(LOGGED_ITEMS) {
            journal.info(id, &format!("{path}: copied without its {what}, which the receiving drive can't keep"));
        }
        if tidied > 0 {
            journal.info(id, &format!("moved {} into .charon-trash", plural(tidied, "half-copied leftover")));
        }
        self.change_runtime(id, |rt| {
            for side in &outcome.no_links {
                match side {
                    Side::Source => rt.no_links.0 = true,
                    Side::Destination => rt.no_links.1 = true,
                }
            }
            // An item this run carried after all was taken; one refused waits for a change.
            if !rt.refused.is_empty() {
                let carried: HashSet<&str> = outcome.settled.iter().map(|step| step.path.as_str()).collect();
                rt.refused.retain(|(_, key), _| !carried.contains(key.as_str()));
            }
            for refused in &outcome.refused {
                rt.refused.insert((refused.side, refused.key.clone()), (refused.clone(), survey.volume(refused.side).cloned()));
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
        let left = !outcome.failed.is_empty() || !outcome.skipped.is_empty() || !outcome.left_out.is_empty();
        self.finish(job, survey, crossing, summary, Some(Recorded { recorded, drives, destroyed: outcome.destroyed, left }));
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
        let memory = Memory { baseline: next, tokens: Some(tokens), generation, folded: survey.folded, distrust: None };
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
        let mut look = Look::of(survey);
        let resolved: Vec<String> = survey.plan.actions.iter().map(|a| a.path().to_string()).collect();
        self.change_runtime(id, |rt| {
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
                    look.items = 0;
                    look.bytes = 0;
                    look.removals = Default::default();
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
            // After a stopped run, or a confirmed run that left something, look again at once,
            // so the row shows what's still waiting. Runs by themselves wait for the regular
            // look: a file that fails every time mustn't start a run every few seconds.
            if summary.cancelled || (crossing.approved && (left || summary.error.is_some())) {
                rt.last_checked = None;
            } else {
                rt.last_checked = Some(Instant::now());
            }
        });
        self.emit(EngineEvent::Finished(Finished { id: id.clone(), name: job.setup.name.clone(), summary }));
        self.publish(id);
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
        self.note_look(job, survey);
        self.change_runtime(id, |rt| {
            rt.cancel = None;
            rt.progress = None;
            rt.finished_at = Some(Instant::now());
            rt.must_ask = true;
        });
        self.emit(EngineEvent::Held { id: id.clone(), name: job.setup.name.clone(), reason });
        self.publish(id);
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
    /// Steps failed, were skipped or left out.
    left: bool,
}

impl Survey {
    /// A survey of nothing, for a run that ended before it could look.
    fn empty(job: &Job) -> Survey {
        Survey {
            setup: job.setup.clone(),
            source: Default::default(),
            destination: Default::default(),
            folded: false,
            markers: Markers { source: Found::Missing, destination: Found::Missing },
            memory: None,
            trusted: false,
            baseline: Baseline::new(),
            landed: Baseline::new(),
            plan: Default::default(),
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
