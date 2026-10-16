//! Syncing, previews (dry runs), checks for pending changes, and settling conflicts.

use crate::engine::{Engine, EngineEvent};
use crate::error::{Error, Result};
use crate::model::{
    ConflictChoice, Finished, Job, JobId, Pending, Phase, Progress, RunSummary,
};
use crate::plan::{self, Action, Plan, PlanSummary, Side, TwoWayPlanner};
use crate::run::{Executor, Outcome, ProgressMeter, Shores, Trash};
use crate::scan::{Entry, ExcludeFilter, Scanner, Snapshot};
use chrono::Utc;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// A preview shows at most this many individual steps.
const PREVIEW_STEPS: usize = 400;
/// Live progress reaches the window at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);
/// At most this many failed files are written to the log per run.
const LOGGED_FAILURES: usize = 50;

/// What a dry run found: the counts, and the first steps in detail.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub summary: PlanSummary,
    pub steps: Vec<Action>,
    pub total_steps: usize,
}

/// Both folders as they are now, and what it takes to bring them together.
struct Survey {
    source: Snapshot,
    destination: Snapshot,
    baseline: Snapshot,
    plan: Plan,
}

impl Engine {
    // ---------------------------------------------------------------- syncing

    /// Starts a sync in the background. Works for paused jobs too: pausing only stops
    /// automatic runs.
    pub fn sync_now(&self, id: &JobId) -> Result<()> {
        self.start_run(id, "started by hand")
    }

    /// Starts a sync of every job that isn't paused and whose folders are reachable.
    pub fn sync_all(&self) {
        for job in self.job_list() {
            if !job.paused && job.setup.source.is_dir() && job.setup.destination.is_dir() {
                let _ = self.start_run(&job.id, "started with Sync all");
            }
        }
    }

    /// Stops a running sync (what's already copied stays).
    pub fn cancel(&self, id: &JobId) {
        if let Some(cancel) = self.runtime().get(id).and_then(|rt| rt.cancel.clone()) {
            cancel.store(true, Ordering::Relaxed);
        }
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

    pub(crate) fn start_run(&self, id: &JobId, reason: &str) -> Result<()> {
        let job = self.stored_job(id)?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.change_runtime(id, |rt| {
            if rt.is_running() {
                return Err(Error::Busy);
            }
            rt.cancel = Some(cancel.clone());
            rt.progress = Some(Progress::scanning());
            rt.problem = None;
            Ok(())
        })?;
        self.publish(id);

        let engine = self.clone();
        let reason = reason.to_string();
        thread::Builder::new()
            .name("charon-sync".into())
            .spawn(move || engine.run_job(job, cancel, &reason))
            .map_err(Error::Io)?;
        Ok(())
    }

    /// The whole life of one run, on its own thread.
    fn run_job(&self, job: Job, cancel: Arc<AtomicBool>, reason: &str) {
        let id = job.id.clone();
        let journal = &self.inner.journal;
        journal.info(&id, &format!("sync {reason}"));
        let started_at = Utc::now();

        let result = self.cross(&job, &cancel);
        let summary = Self::summarise(&result, started_at);

        // Live state: the run is over.
        let conflicts = result.as_ref().map(|(_, survey)| survey.plan.conflicts()).unwrap_or_default();
        self.change_runtime(&id, |rt| {
            rt.cancel = None;
            rt.progress = None;
            rt.finished_at = Some(Instant::now());
            rt.last_checked = Some(Instant::now());
            rt.problem = summary.error.clone();
            if result.is_ok() {
                rt.conflicts = conflicts;
                rt.pending = None;
            }
        });

        // Stored state: the summary, and when it was last fully synced.
        let _ = self.change_job(&id, |job| {
            if summary.error.is_none() && !summary.cancelled {
                job.last_synced = Some(summary.finished_at);
            }
            job.last_run = Some(summary.clone());
        });

        // The log.
        let line = summary.describe();
        match (&summary.error, summary.failed) {
            (Some(_), _) => journal.error(&id, &line),
            (None, 0) => journal.info(&id, &line),
            (None, _) => journal.warn(&id, &line),
        }
        if let Ok((outcome, _)) = &result {
            for (path, reason) in outcome.failed.iter().take(LOGGED_FAILURES) {
                journal.warn(&id, &format!("{path}: {reason}"));
            }
        }

        self.emit(EngineEvent::Finished(Finished { id: id.clone(), name: job.setup.name.clone(), summary }));
        self.publish(&id);
    }

    /// Scans, plans and carries the files over.
    fn cross(&self, job: &Job, cancel: &AtomicBool) -> Result<(Outcome, Survey)> {
        let filter = ExcludeFilter::new(&job.setup.excludes)?;
        let survey = self.survey(job, &filter, Some(cancel))?;
        Self::refuse_wiping(&survey)?;

        let shores = Shores { source: job.setup.source.clone(), destination: job.setup.destination.clone() };
        let id = job.id.clone();
        let observer = |progress: &Progress| self.report_progress(&id, progress);
        let mut meter = ProgressMeter::for_plan(&survey.plan, &observer);
        let outcome = Executor::new(&shores, job.setup.safe_delete, &filter, cancel).run(&survey.plan, &mut meter);

        if job.setup.mode.is_two_way() {
            self.remember_agreement(job, &filter, &survey.baseline);
        }
        let trash_days = self.settings().trash_days;
        Trash::sweep(&shores.source, trash_days);
        Trash::sweep(&shores.destination, trash_days);
        Ok((outcome, survey))
    }

    /// After a two-way run: what both sides agree on now becomes the baseline.
    fn remember_agreement(&self, job: &Job, filter: &ExcludeFilter, old: &Snapshot) {
        let scanner = Scanner::new(filter);
        let (Ok(source), Ok(destination)) = (scanner.scan(&job.setup.source), scanner.scan(&job.setup.destination)) else {
            return; // a folder went away: keep the old baseline, the next run decides again
        };
        let next = TwoWayPlanner::next_baseline(&source, &destination, old);
        if let Err(e) = self.inner.baselines.save(&job.id, &next) {
            self.inner.journal.error(&job.id, &format!("couldn't save the two-way state: {e}"));
        }
    }

    /// Refuses a run that would delete every file on one side because the other side is
    /// completely empty, which is almost always an unmounted drive or a wrong folder.
    fn refuse_wiping(survey: &Survey) -> Result<()> {
        let deletes_on = |side: Side| survey.plan.actions.iter().any(|a| matches!(a, Action::Delete { side: s, .. } if *s == side));
        let wipes = |emptied: &Snapshot, other: &Snapshot, side: Side| {
            other.file_count() == 0 && emptied.file_count() > 0 && deletes_on(side)
        };
        if wipes(&survey.destination, &survey.source, Side::Destination) {
            return Err(Error::Invalid(
                "The source folder is empty, so this sync would delete everything in the destination. \
                 Charon stopped to be safe. If that's really what you want, empty the destination yourself."
                    .into(),
            ));
        }
        if wipes(&survey.source, &survey.destination, Side::Source) {
            return Err(Error::Invalid(
                "The destination folder is empty, so this sync would delete everything in the source. \
                 Charon stopped to be safe."
                    .into(),
            ));
        }
        Ok(())
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

    fn summarise(result: &Result<(Outcome, Survey)>, started_at: chrono::DateTime<Utc>) -> RunSummary {
        let finished_at = Utc::now();
        match result {
            Ok((outcome, survey)) => RunSummary {
                started_at,
                finished_at,
                copied: outcome.copied,
                deleted: outcome.deleted,
                bytes: outcome.bytes,
                conflicts: survey.plan.conflicts().len() as u64,
                failed: outcome.failed.len() as u64,
                cancelled: outcome.cancelled,
                error: None,
            },
            Err(Error::Cancelled) => RunSummary { started_at, finished_at, cancelled: true, ..Self::empty_summary(started_at) },
            Err(e) => RunSummary { error: Some(e.to_string()), ..Self::empty_summary(started_at) },
        }
    }

    fn empty_summary(started_at: chrono::DateTime<Utc>) -> RunSummary {
        RunSummary {
            started_at,
            finished_at: Utc::now(),
            copied: 0,
            deleted: 0,
            bytes: 0,
            conflicts: 0,
            failed: 0,
            cancelled: false,
            error: None,
        }
    }

    // ---------------------------------------------------------------- looking without touching

    /// A dry run: what the next sync would do, without changing anything.
    pub fn preview(&self, id: &JobId) -> Result<Preview> {
        let job = self.stored_job(id)?;
        let filter = ExcludeFilter::new(&job.setup.excludes)?;
        let survey = self.survey(&job, &filter, None)?;
        self.note_pending(id, &survey.plan);
        let steps: Vec<Action> = survey.plan.ordered().into_iter().take(PREVIEW_STEPS).cloned().collect();
        Ok(Preview { summary: survey.plan.summary(), steps, total_steps: survey.plan.actions.len() })
    }

    /// Looks for pending changes in the background (no-op while the job runs or checks).
    pub fn check(&self, id: &JobId) {
        let Ok(job) = self.stored_job(id) else { return };
        let busy = self.change_runtime(id, |rt| {
            let busy = rt.is_running() || rt.checking;
            if !busy {
                rt.checking = true;
            }
            busy
        });
        if busy {
            return;
        }
        let engine = self.clone();
        let spawned = thread::Builder::new().name("charon-check".into()).spawn(move || {
            let result = ExcludeFilter::new(&job.setup.excludes).and_then(|filter| engine.survey(&job, &filter, None));
            match result {
                Ok(survey) => engine.note_pending(&job.id, &survey.plan),
                Err(Error::Offline(_)) => {}
                Err(e) => engine.change_runtime(&job.id, |rt| rt.problem = Some(e.to_string())),
            }
            engine.change_runtime(&job.id, |rt| {
                rt.checking = false;
                rt.last_checked = Some(Instant::now());
            });
            engine.publish(&job.id);
        });
        if spawned.is_err() {
            self.change_runtime(id, |rt| rt.checking = false);
        }
    }

    /// Whether the last check is older than `minutes` (or there was none).
    pub fn check_due(&self, id: &JobId, minutes: u32) -> bool {
        let interval = Duration::from_secs(minutes.max(1) as u64 * 60);
        self.runtime()
            .get(id)
            .is_none_or(|rt| !rt.checking && !rt.is_running() && rt.last_checked.is_none_or(|t| t.elapsed() >= interval))
    }

    /// Whether the last check found changes waiting.
    pub fn has_pending(&self, id: &JobId) -> bool {
        self.runtime().get(id).and_then(|rt| rt.pending.as_ref()).is_some_and(|p| p.items > 0)
    }

    fn note_pending(&self, id: &JobId, plan: &Plan) {
        let summary = plan.summary();
        let conflicts = plan.conflicts();
        self.change_runtime(id, |rt| {
            rt.pending = Some(Pending { items: summary.changes(), bytes: summary.copy_bytes, checked_at: Utc::now() });
            rt.conflicts = conflicts;
            if rt.problem.as_deref().is_some_and(|p| p.contains("isn't reachable")) {
                rt.problem = None;
            }
        });
    }

    /// Scans both folders and plans the run.
    fn survey(&self, job: &Job, filter: &ExcludeFilter, cancel: Option<&AtomicBool>) -> Result<Survey> {
        let mut scanner = Scanner::new(filter);
        if let Some(cancel) = cancel {
            scanner = scanner.cancellable(cancel);
        }
        let source = scanner.scan(&job.setup.source)?;
        let destination = scanner.scan(&job.setup.destination)?;
        let baseline = if job.setup.mode.is_two_way() { self.inner.baselines.load(&job.id) } else { Snapshot::new() };
        let plan = plan::plan_for(&job.setup, &source, &destination, &baseline);
        Ok(Survey { source, destination, baseline, plan })
    }

    // ---------------------------------------------------------------- conflicts

    /// Settles one two-way conflict the way the person chose.
    pub fn resolve_conflict(&self, id: &JobId, path: &str, choice: ConflictChoice) -> Result<()> {
        if self.is_running(id) {
            return Err(Error::Busy);
        }
        let job = self.stored_job(id)?;
        let shores = Shores { source: job.setup.source.clone(), destination: job.setup.destination.clone() };
        let filter = ExcludeFilter::new(&job.setup.excludes)?;
        let s = Self::stat(&shores, Side::Source, path);
        let d = Self::stat(&shores, Side::Destination, path);

        let mut plan = Plan::default();
        match choice {
            ConflictChoice::KeepSource => TwoWayPlanner::carry(&mut plan, path, s.as_ref(), d.as_ref(), Side::Destination),
            ConflictChoice::KeepDestination => TwoWayPlanner::carry(&mut plan, path, d.as_ref(), s.as_ref(), Side::Source),
            ConflictChoice::KeepBoth => match (s, d) {
                (Some(src), Some(dst)) if !src.is_dir() && !dst.is_dir() => plan.push(Action::KeepBoth {
                    path: path.to_string(),
                    source_bytes: src.size,
                    destination_bytes: dst.size,
                }),
                _ => return Err(Error::Invalid("Only files can be kept twice. Choose one side.".into())),
            },
        }

        let never = AtomicBool::new(false);
        let quiet = |_: &Progress| {};
        let mut meter = ProgressMeter::for_plan(&plan, &quiet);
        let outcome = Executor::new(&shores, job.setup.safe_delete, &filter, &never).run(&plan, &mut meter);
        if let Some((_, reason)) = outcome.failed.first() {
            return Err(Error::Invalid(reason.clone()));
        }

        // Both sides agree on this path now: remember it.
        let mut baseline = self.inner.baselines.load(id);
        match Self::stat(&shores, Side::Source, path) {
            Some(entry) => baseline.insert(path, entry),
            None => baseline.remove(path),
        }
        self.inner.baselines.save(id, &baseline)?;

        let word = match choice {
            ConflictChoice::KeepSource => "kept the source's version of",
            ConflictChoice::KeepDestination => "kept the destination's version of",
            ConflictChoice::KeepBoth => "kept both versions of",
        };
        self.inner.journal.info(id, &format!("{word} {path}"));
        self.change_runtime(id, |rt| rt.conflicts.retain(|c| c.path != path));
        self.publish(id);
        Ok(())
    }

    /// One item as it is on disk right now.
    fn stat(shores: &Shores, side: Side, path: &str) -> Option<Entry> {
        let meta = std::fs::symlink_metadata(shores.path(side, path)).ok()?;
        if meta.is_dir() {
            return Some(Entry::dir());
        }
        let modified = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as i64;
        Some(Entry::file(meta.len(), modified))
    }
}
