//! Looking for changes without touching anything, and telling the person when changes wait
//! for their OK.

use crate::engine::survey::{Purpose, Survey, INTERNAL_LOOK};
use crate::engine::{Engine, EngineEvent, Moment};
use crate::error::{Error, Result};
use crate::model::{Job, JobId, JobStatus, Trigger, WaitingJob};
use crate::scan::{ExcludeFilter, Keys};
use chrono::Utc;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// The first notice that changes wait comes at most this often per job…
const FIRST_NOTICE: Duration = Duration::from_secs(30 * 60);
/// …the first reminder after this long…
const SECOND_NOTICE: Duration = Duration::from_secs(2 * 60 * 60);
/// …and then one a day.
const LATER_NOTICES: Duration = Duration::from_secs(24 * 60 * 60);
/// After a start, reminders wait for every job's first look, at most this long.
const FIRST_LOOKS: Duration = Duration::from_secs(2 * 60);

/// Where in a job's folders the folder watcher heard a change.
pub(crate) enum ChangeAt {
    /// At one of the folders themselves (a drive mounted or unmounted there), or at a path
    /// Charon can't place: anything in them may have changed.
    Folder,
    /// At an item inside, by its key (as spelled on disk).
    Item(String),
    /// At an item the job leaves out.
    Ignored,
}

impl Engine {
    /// Looks for changes in the background: scans both folders and plans, but changes
    /// nothing. Does nothing while the job runs, is being looked at, or a look is running.
    /// What it finds counts only if nothing outdated it meanwhile (see `note_look`).
    pub fn check(&self, id: &JobId) {
        let stop = Arc::new(AtomicBool::new(false));
        let quitting = self.is_shutting_down();
        // The setup and the epoch are read together, as an edit changes them together: a look
        // of the old setup with the new epoch would count.
        let (job, epoch) = {
            let state = self.state();
            let Some(job) = state.jobs.iter().find(|job| &job.id == id).cloned() else { return };
            let mut runtime = self.runtime();
            let rt = runtime.entry(id.clone()).or_default();
            Self::forget_own_writes(rt);
            let busy = quitting || rt.is_running() || rt.is_checking() || rt.preparing.is_some();
            if busy {
                return;
            }
            rt.checking = Some(stop.clone());
            (job, rt.epoch)
        };
        let engine = self.clone();
        let flag = stop.clone();
        let spawned = thread::Builder::new().name("charon-check".into()).spawn(move || {
            let looked = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let result = engine.survey(&job, &flag, Purpose::Check);
                engine.end_check(&job, &flag, epoch, result);
            }));
            // A mistake in Charon itself never leaves the job marked as being looked at.
            if looked.is_err() {
                log::error!("a look at job {} ended on a panic", job.id);
                engine.change_known_runtime(&job.id, |rt| {
                    rt.problem = Some(INTERNAL_LOOK.into());
                    if rt.checking.as_ref().is_some_and(|f| Arc::ptr_eq(f, &flag)) {
                        rt.checking = None;
                    }
                    rt.last_checked = Some(Instant::now());
                });
                engine.publish(&job.id);
            }
        });
        if spawned.is_err() {
            self.change_runtime(id, |rt| rt.checking = None);
        }
    }

    /// Ends a background look that began at `epoch` with `flag` as its stop flag: what it
    /// found, or what went wrong, counts only if nothing outdated it meanwhile (see
    /// `note_look`).
    fn end_check(&self, job: &Job, flag: &Arc<AtomicBool>, epoch: u64, result: Result<Survey>) {
        // A look made for a setup that changed meanwhile tells nothing.
        let setup_current = self.stored_job(&job.id).is_ok_and(|now| now.setup == job.setup);
        let counted = match &result {
            Ok(survey) => self.note_look(job, survey, Some(epoch)),
            Err(Error::Cancelled) | Err(Error::Offline(_)) => false,
            Err(e) => self
                .change_known_runtime(&job.id, |rt| {
                    let current = setup_current && rt.epoch == epoch && !rt.is_running();
                    if current {
                        rt.problem = Some(e.to_string());
                    }
                    current
                })
                .unwrap_or(false),
        };
        self.change_known_runtime(&job.id, |rt| {
            if rt.checking.as_ref().is_some_and(|f| Arc::ptr_eq(f, flag)) {
                rt.checking = None;
            }
            // A look that was outdated meanwhile doesn't count as one: the job is looked at
            // again as soon as nothing else is looking. (One that found a folder away does,
            // or it would be tried again every second.)
            if counted || (setup_current && rt.epoch == epoch) {
                rt.last_checked = Some(Instant::now());
            }
        });
        self.publish(&job.id);
    }

    /// Whether the last look is older than `minutes` (or there was none), and nothing is
    /// looking or running now.
    pub fn check_due(&self, id: &JobId, minutes: u32) -> bool {
        let interval = Duration::from_secs(minutes.max(1) as u64 * 60);
        self.runtime().get(id).is_none_or(|rt| {
            !rt.is_checking() && !rt.is_running() && rt.preparing.is_none() && rt.last_checked.is_none_or(|t| t.elapsed() >= interval)
        })
    }

    /// Whether the last look found changes a job that runs by itself would carry, and no
    /// automatic run has tried them yet and ended before carrying anything. Removals that
    /// wait for an item to be missing a while longer don't count (see `Missing`).
    pub fn has_pending(&self, id: &JobId) -> bool {
        let now = Moment::now();
        self.runtime().get(id).is_some_and(|rt| {
            rt.look.as_ref().is_some_and(|look| look.items > rt.missing.waiting(now) && rt.tried.is_none_or(|tried| look.at > tried))
        })
    }

    /// Whether removals a run left for later can be carried out now (see `Missing`): the job
    /// then syncs once more, whatever starts its syncs otherwise.
    pub fn take_follow_up(&self, id: &JobId) -> bool {
        self.take_follow_up_at(id, Moment::now())
    }

    /// `take_follow_up` at the moment `now`.
    pub(crate) fn take_follow_up_at(&self, id: &JobId, now: Moment) -> bool {
        if !self.runtime().get(id).is_some_and(|rt| rt.missing.follow_up_due(now)) {
            return false;
        }
        let Ok(job) = self.stored_job(id) else { return false };
        self.change_missing(&job, |missing| missing.take_follow_up(now))
    }

    /// Whether a list of the job's changes is being prepared or waits for the person's OK
    /// (automatic runs of the job wait meanwhile).
    pub fn review_open(&self, id: &JobId) -> bool {
        self.runtime()
            .get(id)
            .is_some_and(|rt| rt.preparing.is_some() || rt.review.as_ref().is_some_and(|review| Utc::now() < review.expires_at))
    }

    /// Records that the job's schedule came due now, whether it ran or only looked, so the
    /// next time is counted from here.
    pub fn mark_scheduled(&self, id: &JobId) {
        if let Err(e) = self.change_job(id, |job| job.last_scheduled = Some(Utc::now())) {
            log::warn!("couldn't save the schedule of job {id}: {e}");
        }
    }

    /// Something changed at `path` inside one of the job's folders. Returns whether the
    /// change matters (it isn't an excluded item or one of Charon's own files); a list
    /// made before it can't be confirmed anymore.
    pub fn note_change(&self, id: &JobId, path: &Path) -> bool {
        let Ok(job) = self.stored_job(id) else { return false };
        if let ChangeAt::Ignored = self.change_at(&job, path) {
            return false;
        }
        self.mark_review_stale(id);
        true
    }

    /// Where in the job's folders `path` is (see `ChangeAt`).
    pub(crate) fn change_at(&self, job: &Job, path: &Path) -> ChangeAt {
        let relative = [&job.setup.source, &job.setup.destination].into_iter().find_map(|root| path.strip_prefix(root).ok());
        let Some(relative) = relative.and_then(|r| r.to_str()) else { return ChangeAt::Folder };
        let key = Keys::normalize(&relative.replace('\\', "/"));
        if key.is_empty() {
            return ChangeAt::Folder;
        }
        let excluded = self.change_runtime(&job.id, |rt| {
            let fresh = rt.filter.as_ref().is_some_and(|(setup, _)| setup == &job.setup);
            if !fresh {
                // Until a look finds out how the drives treat case, patterns match it exactly:
                // they then leave out the fewest items, so a change that matters is never missed.
                rt.filter = ExcludeFilter::new(&job.setup.excludes, false).ok().map(|filter| (job.setup.clone(), filter));
            }
            rt.filter.as_ref().is_some_and(|(_, filter)| {
                filter.excludes(&key) || Keys::ancestors(&key).any(|folder| filter.excludes(folder))
            })
        });
        if excluded {
            ChangeAt::Ignored
        } else {
            ChangeAt::Item(key)
        }
    }

    /// Tells the person (in one notification) about the jobs whose changes or conflicts
    /// wait for them and haven't been mentioned for a while. Paused and offline jobs aren't
    /// mentioned. Right after Charon starts it waits for the first looks, so the jobs found
    /// waiting then come in one notification.
    pub fn remind(&self) {
        if self.settings().paused_all {
            return;
        }
        let first_looks_done = self.inner.opened.elapsed() >= FIRST_LOOKS
            || self.job_list().iter().all(|job| job.paused || self.runtime().get(&job.id).is_some_and(|rt| rt.last_checked.is_some()));
        if !first_looks_done {
            return;
        }
        let mut due = Vec::new();
        for job in self.job_list() {
            if job.paused {
                continue;
            }
            // Only jobs whose last look found something, or that were held back.
            let candidate = job.held.is_some() || self.runtime().get(&job.id).and_then(|rt| rt.look.as_ref()).is_some_and(|look| look.has_work());
            if !candidate {
                continue;
            }
            let Ok(view) = self.job(&job.id) else { continue };
            // A list the person confirmed (its sync waits for another one) needs nothing more.
            if view.confirmed || matches!(view.status, JobStatus::Offline | JobStatus::Syncing) {
                continue;
            }
            let conflicts = view.conflicts.len() as u64;
            if view.waiting.is_none() && conflicts == 0 {
                continue;
            }
            let tell = self.change_runtime(&job.id, |rt| {
                let gap = match rt.reminder.count {
                    0 => FIRST_NOTICE,
                    1 => SECOND_NOTICE,
                    _ => LATER_NOTICES,
                };
                let now = Utc::now();
                // A clock set back counts as time gone by: a reminder is never put off for good.
                let tell = rt.reminder.last.is_none_or(|last| now < last || (now - last).to_std().is_ok_and(|gone| gone >= gap));
                if tell {
                    rt.reminder.last = Some(now);
                    rt.reminder.count += 1;
                }
                tell
            });
            if tell {
                due.push(WaitingJob {
                    id: job.id.clone(),
                    name: job.setup.name.clone(),
                    items: view.waiting.as_ref().map_or(0, |w| w.items),
                    removals: view.waiting.as_ref().map_or(0, |w| w.removals.total()),
                    conflicts,
                    drive_connected: job.setup.trigger == Trigger::DriveConnected,
                });
            }
        }
        if !due.is_empty() {
            self.emit(EngineEvent::Waiting(due));
        }
    }

    /// The job's drive was just connected: what its next look finds is mentioned at once
    /// (the person may be about to eject the drive again).
    pub fn mention_next_look(&self, id: &JobId) {
        self.change_runtime(id, |rt| rt.reminder.last = None);
    }

    /// A folder of the job came back (a drive plugged in again, a share reconnected). What
    /// the last look found, it found before: the row mustn't say "In sync" from then. The
    /// look is put aside, so is what a look still under way finds, and the job is looked
    /// at again at once. A share that just came back may list a folder before it has loaded,
    /// so the items found missing before have to be found missing again for a while before
    /// a run nobody looks at follows them (see `Missing::look_again`).
    pub fn forget_look(&self, id: &JobId) {
        self.change_runtime(id, |rt| {
            rt.epoch += 1;
            rt.look = None;
            rt.tried = None;
            rt.last_checked = None;
            rt.missing.look_again();
        });
    }

    /// Charon wasn't looking at the folders for a while (the computer slept, or the clock
    /// on the wall jumped): the shares may have gone away and come back meanwhile, so the
    /// items found missing on them have to be found missing again for a while before a run
    /// nobody looks at follows them (see `Missing::look_again`).
    pub fn after_a_gap(&self) {
        for rt in self.runtime().values_mut() {
            rt.missing.look_again();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConflictRule, JobSetup, ReviewKind, SyncMode};
    use std::fs;
    use std::sync::atomic::Ordering;

    /// An engine with one Backup job whose source holds a file that waits to be copied.
    fn engine_with_a_job(tmp: &Path) -> (Engine, Job) {
        let (source, destination) = (tmp.join("source"), tmp.join("destination"));
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("a.txt"), "a").unwrap();
        let engine = Engine::open(tmp.join("data"), Arc::new(|_| {})).unwrap();
        let setup = JobSetup {
            name: "Backup".into(),
            source,
            destination,
            mode: SyncMode::Backup,
            conflict: ConflictRule::Ask,
            trigger: Trigger::Manual,
            excludes: vec![],
            safe_delete: true,
            full_automation: false,
        };
        let id = engine.add_job(setup).unwrap().job.id;
        // The look `add_job` started is done before the test begins.
        while engine.runtime().get(&id).is_some_and(|rt| rt.is_checking()) {
            thread::sleep(Duration::from_millis(5));
        }
        let job = engine.stored_job(&id).unwrap();
        (engine, job)
    }

    fn look_at(engine: &Engine, job: &Job) -> Survey {
        engine.survey(job, &AtomicBool::new(false), Purpose::Check).unwrap()
    }

    fn epoch(engine: &Engine, id: &JobId) -> u64 {
        engine.runtime().get(id).map_or(0, |rt| rt.epoch)
    }

    #[test]
    fn a_look_begun_before_a_sync_never_replaces_what_came_after() {
        let tmp = tempfile::tempdir().unwrap();
        let (engine, job) = engine_with_a_job(tmp.path());
        let before = epoch(&engine, &job.id);
        let survey = look_at(&engine, &job);
        // A sync begins (and ends) while the look is still on its way.
        engine.begin_run(&job.id, |_, _| Ok(())).unwrap();
        engine.change_runtime(&job.id, |rt| {
            rt.cancel = None;
            rt.look = None;
        });
        assert!(!engine.note_look(&job, &survey, Some(before)));
        assert!(engine.runtime().get(&job.id).is_some_and(|rt| rt.look.is_none()), "the old look isn't recorded");
        let review = engine.keep_review(&job, survey, ReviewKind::Sync, None, None, before);
        assert!(matches!(review, Err(Error::ReviewOutdated(_))));
        assert!(engine.runtime().get(&job.id).is_some_and(|rt| rt.review.is_none()));
    }

    #[test]
    fn a_look_outdated_by_an_edit_leaves_the_new_setup_to_be_looked_at_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (engine, job) = engine_with_a_job(tmp.path());
        // A background look is on its way…
        let flag = Arc::new(AtomicBool::new(false));
        let before = engine.change_runtime(&job.id, |rt| {
            rt.look = None;
            rt.checking = Some(flag.clone());
            rt.epoch
        });
        let survey = look_at(&engine, &job);
        // …when the person edits the job.
        let mut setup = job.setup.clone();
        setup.excludes.push("*.tmp".into());
        engine.update_job(&job.id, setup).unwrap();
        assert!(flag.load(Ordering::Relaxed), "the look in progress was told to stop");
        engine.end_check(&job, &flag, before, Ok(survey));
        {
            let runtime = engine.runtime();
            let rt = runtime.get(&job.id).unwrap();
            assert!(rt.look.is_none() && !rt.is_checking());
            assert!(rt.last_checked.is_none(), "the next tick looks at the new setup");
        }
        assert!(engine.check_due(&job.id, 15));
    }

    #[test]
    fn a_look_of_the_old_setup_never_counts_for_the_edited_job() {
        let tmp = tempfile::tempdir().unwrap();
        let (engine, job) = engine_with_a_job(tmp.path());
        let flag = Arc::new(AtomicBool::new(false));
        engine.change_runtime(&job.id, |rt| {
            rt.look = None;
            rt.checking = Some(flag.clone());
        });
        let survey = look_at(&engine, &job);
        let mut setup = job.setup.clone();
        setup.excludes.push("*.tmp".into());
        engine.update_job(&job.id, setup).unwrap();
        // The look of the old setup ends with the epoch the edit left.
        let now = epoch(&engine, &job.id);
        engine.end_check(&job, &flag, now, Ok(survey));
        let rt = engine.runtime();
        let rt = rt.get(&job.id).unwrap();
        assert!(rt.look.is_none());
        assert!(rt.last_checked.is_none(), "the edited job is looked at on the next tick");
    }

    #[test]
    fn a_look_that_ends_after_its_job_was_removed_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let (engine, job) = engine_with_a_job(tmp.path());
        let before = epoch(&engine, &job.id);
        let survey = look_at(&engine, &job);
        engine.remove_job(&job.id).unwrap();
        assert!(!engine.note_look(&job, &survey, Some(before)));
        let review = engine.keep_review(&job, survey, ReviewKind::Sync, None, None, before);
        assert!(review.is_err());
        engine.end_check(&job, &Arc::new(AtomicBool::new(false)), before, Err(Error::Invalid("gone".into())));
        assert!(engine.runtime().get(&job.id).is_none(), "no live state is left for a removed job");
    }

    #[test]
    fn a_look_that_nothing_outdated_is_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let (engine, job) = engine_with_a_job(tmp.path());
        let flag = Arc::new(AtomicBool::new(false));
        let before = engine.change_runtime(&job.id, |rt| {
            rt.look = None;
            rt.checking = Some(flag.clone());
            rt.epoch
        });
        let survey = look_at(&engine, &job);
        engine.end_check(&job, &flag, before, Ok(survey));
        let rt = engine.runtime();
        let rt = rt.get(&job.id).unwrap();
        assert_eq!(rt.look.as_ref().map(|look| look.items), Some(1));
        assert!(rt.last_checked.is_some() && !rt.is_checking());
    }
}
