//! Looking for changes without touching anything, and telling the person when changes wait
//! for their OK.

use crate::engine::survey::{Purpose, INTERNAL_LOOK};
use crate::engine::{Engine, EngineEvent};
use crate::error::Error;
use crate::model::{JobId, JobStatus, Trigger, WaitingJob};
use crate::scan::{ExcludeFilter, Keys};
use chrono::{DateTime, Utc};
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

impl Engine {
    /// Looks for changes in the background: scans both folders and plans, but changes
    /// nothing. Does nothing while the job runs, is being looked at, or a look is running.
    pub fn check(&self, id: &JobId) {
        let Ok(job) = self.stored_job(id) else { return };
        let stop = Arc::new(AtomicBool::new(false));
        let quitting = self.is_shutting_down();
        let started = self.change_runtime(id, |rt| {
            let busy = quitting || rt.is_running() || rt.is_checking() || rt.preparing.is_some();
            if !busy {
                rt.checking = Some(stop.clone());
            }
            !busy
        });
        if !started {
            return;
        }
        let engine = self.clone();
        let flag = stop.clone();
        let spawned = thread::Builder::new().name("charon-check".into()).spawn(move || {
            let looked = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let result = engine.survey(&job, &flag, Purpose::Check);
                // A look made for a setup that changed meanwhile tells nothing.
                let current = engine.stored_job(&job.id).ok().is_some_and(|now| now.setup == job.setup);
                match result {
                    Ok(survey) if current => engine.note_look(&job, &survey),
                    Ok(_) | Err(Error::Cancelled) | Err(Error::Offline(_)) => {}
                    Err(e) if current => engine.change_runtime(&job.id, |rt| rt.problem = Some(e.to_string())),
                    Err(_) => {}
                }
            }));
            // A mistake in Charon itself never leaves the job marked as being looked at.
            if looked.is_err() {
                log::error!("a look at job {} ended on a panic", job.id);
                engine.change_runtime(&job.id, |rt| rt.problem = Some(INTERNAL_LOOK.into()));
            }
            engine.change_runtime(&job.id, |rt| {
                if rt.checking.as_ref().is_some_and(|f| Arc::ptr_eq(f, &flag)) {
                    rt.checking = None;
                }
                rt.last_checked = Some(Instant::now());
            });
            engine.publish(&job.id);
        });
        if spawned.is_err() {
            self.change_runtime(id, |rt| rt.checking = None);
        }
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
        let now = Utc::now();
        self.runtime().get(id).is_some_and(|rt| {
            rt.look.as_ref().is_some_and(|look| look.items > rt.missing.waiting(now) && rt.tried.is_none_or(|tried| look.at > tried))
        })
    }

    /// Whether removals a run left for later can be carried out now (see `Missing`): the job
    /// then syncs once more, whatever starts its syncs otherwise.
    pub fn take_follow_up(&self, id: &JobId) -> bool {
        self.take_follow_up_at(id, Utc::now())
    }

    /// `take_follow_up` at the time `now`.
    pub(crate) fn take_follow_up_at(&self, id: &JobId, now: DateTime<Utc>) -> bool {
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
        let relative = [&job.setup.source, &job.setup.destination].into_iter().find_map(|root| path.strip_prefix(root).ok());
        // The folder itself (a drive mounted or unmounted there), or a path Charon can't
        // place: a list made before can't be trusted anymore.
        let Some(relative) = relative.and_then(|r| r.to_str()) else {
            self.mark_review_stale(id);
            return true;
        };
        let key = Keys::normalize(&relative.replace('\\', "/"));
        if key.is_empty() {
            self.mark_review_stale(id);
            return true;
        }
        let excluded = self.change_runtime(id, |rt| {
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
            return false;
        }
        self.mark_review_stale(id);
        true
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
            if matches!(view.status, JobStatus::Offline | JobStatus::Syncing) {
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
    /// look is put aside, and the job is looked at again at once.
    pub fn forget_look(&self, id: &JobId) {
        self.change_runtime(id, |rt| {
            rt.look = None;
            rt.tried = None;
            rt.last_checked = None;
        });
    }
}
