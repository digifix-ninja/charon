//! The autopilot: looks at jobs by themselves, and syncs the ones with full automation.
//!
//! * `mod.rs`      – `Autopilot`: a background loop that looks at every job once a second
//! * `schedule.rs` – `Schedule`: when an interval or daily job is due next
//! * `watcher.rs`  – `FolderWatcher`: hears about changes inside the folders
//!
//! A job *runs by itself* only with full automation on and nothing waiting for a look
//! (`Job::runs_by_itself`). Every other job is only *looked at*: Charon scans and plans in
//! the background and, when changes wait, shows them and tells the person. Nothing changes
//! in its folders until the person confirms the list.
//!
//! Every second, for each job, in this order:
//! 1. **Drive connected:** both folders just became reachable → sync, or look. A folder
//!    that is there but on another drive than at the last sync (the folder an unplugged
//!    drive leaves behind) doesn't count as reachable.
//! 2. **Changes settled:** the folders changed and have been quiet for a moment → sync
//!    (on-change jobs that run by themselves) or look (every other job).
//! 3. **Schedule:** an interval or daily job is due → sync, or look. Either way the next
//!    time is counted from now.
//! 4. **Regular look:** every few minutes, in case the watcher missed something (network
//!    shares, sleep). An on-change job that runs by itself and has changes waiting syncs.
//!
//! A sync that is due waits (it isn't dropped) while another job uses the same folders, and
//! starts as soon as that job is done.
//!
//! Paused jobs (and everything under "Pause everything") are still looked at, so the
//! window shows what's waiting, but never sync by themselves.

mod schedule;
mod watcher;

pub use schedule::Schedule;
pub use watcher::FolderWatcher;

use crate::engine::Engine;
use crate::model::{Job, JobId, Trigger};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(1);
/// Wait after launch, so the window can appear before any work starts.
const STARTUP_DELAY: Duration = Duration::from_secs(3);
/// After a run, changes seen this soon are Charon's own writes.
const QUIET_AFTER_RUN: Duration = Duration::from_secs(3);
/// How long changes must settle before a job that doesn't sync on change is looked at.
const CHECK_SETTLE: Duration = Duration::from_secs(5);

pub struct Autopilot {
    engine: Engine,
    watcher: Option<FolderWatcher>,
    changes: Receiver<(JobId, PathBuf)>,
    /// When each job's folders last changed (cleared once handled).
    changed: HashMap<JobId, Instant>,
    /// Whether both folders of each job were reachable at the last tick.
    reachable: HashMap<JobId, bool>,
    /// When each job's schedule last came due, also kept here: if the job's record can't be
    /// saved, the schedule still doesn't come due again every second.
    scheduled: HashMap<JobId, DateTime<Utc>>,
}

impl Autopilot {
    /// Starts the autopilot on its own thread. It runs until the app quits.
    pub fn start(engine: Engine) {
        let spawned = thread::Builder::new().name("charon-autopilot".into()).spawn(move || {
            let (sender, changes) = mpsc::channel();
            let watcher = FolderWatcher::new(sender)
                .map_err(|e| log::error!("folder watching is unavailable: {e}"))
                .ok();
            let mut pilot =
                Autopilot { engine, watcher, changes, changed: HashMap::new(), reachable: HashMap::new(), scheduled: HashMap::new() };
            thread::sleep(STARTUP_DELAY);
            loop {
                pilot.tick();
                thread::sleep(TICK);
            }
        });
        if let Err(e) = spawned {
            log::error!("couldn't start the autopilot: {e}");
        }
    }

    /// One look at every job.
    fn tick(&mut self) {
        let settings = self.engine.settings();
        let jobs = self.engine.job_list();
        self.follow_folders(&jobs);
        self.collect_changes();
        let now = Utc::now();

        for job in &jobs {
            let id = &job.id;
            // A folder on another drive than at the last sync (a leftover mount folder) isn't
            // the job's folder: nothing is looked at or synced there by itself.
            let reachable = job.setup.source.is_dir() && job.setup.destination.is_dir() && Engine::drive_moved(job).is_none();
            let was_reachable = self.reachable.insert(id.clone(), reachable);
            if was_reachable.is_some() && was_reachable != Some(reachable) {
                // A drive went away or came (maybe another one under the same name): a list
                // made before can't be confirmed anymore, and what was found before it left
                // says nothing about it now.
                self.engine.mark_review_stale(id);
                if reachable {
                    self.engine.forget_look(id);
                }
                self.engine.publish(id); // the row turns offline / back online
            }
            let automatic = !settings.paused_all && !job.paused;
            if !reachable || !automatic || !job.runs_by_itself() {
                // A due sync waits only while the job could still run by itself.
                self.engine.unwant(id);
            }
            if !reachable || self.engine.is_running(id) {
                continue;
            }
            // Runs by itself now: full automation, nothing waiting for a look, and no list
            // of its changes open for the person.
            let by_itself = automatic && job.runs_by_itself() && !self.engine.review_open(id);
            if by_itself {
                self.engine.start_wanted(id);
                if self.engine.is_running(id) {
                    continue;
                }
            }

            // 1. Drive connected (also at launch, when the drive was already there).
            if job.setup.trigger == Trigger::DriveConnected && was_reachable != Some(true) {
                if by_itself {
                    self.run(id, "because the drive was connected");
                } else {
                    // Launching Charon with the drive already there isn't news.
                    if was_reachable.is_some() {
                        self.engine.mention_next_look(id);
                    }
                    self.engine.check(id);
                }
                continue;
            }
            // 2. Changes settled.
            if let Some(since) = self.changed.get(id) {
                let settle = match job.setup.trigger {
                    Trigger::OnChange { settle_seconds } => Duration::from_secs(settle_seconds as u64),
                    _ => CHECK_SETTLE,
                };
                if since.elapsed() >= settle {
                    self.changed.remove(id);
                    if by_itself && job.setup.trigger.syncs_on_change() {
                        self.run(id, "after changes");
                    } else {
                        self.engine.check(id);
                    }
                    continue;
                }
            }
            // 3. Schedule. The next time counts from now, whether it synced or only looked,
            //    so a job waiting for an OK isn't looked at again every second.
            let since = self.scheduled.get(id).copied();
            if automatic && Schedule::is_due_after(job, since, now) {
                self.scheduled.insert(id.clone(), now);
                self.engine.mark_scheduled(id);
                if by_itself {
                    self.run(id, "on schedule");
                } else {
                    self.engine.check(id);
                }
                continue;
            }
            // 4. Regular look.
            if by_itself && job.setup.trigger.syncs_on_change() && self.engine.has_pending(id) {
                self.run(id, "for changes found by a look");
            } else if self.engine.check_due(id, settings.check_minutes) {
                self.engine.check(id);
            }
        }
        self.reachable.retain(|id, _| jobs.iter().any(|job| &job.id == id));
        self.scheduled.retain(|id, _| jobs.iter().any(|job| &job.id == id));
        self.engine.remind();
    }

    /// Starts a sync of a job that runs by itself, or keeps it waiting until nothing stands
    /// in its way.
    fn run(&self, id: &JobId, reason: &str) {
        self.engine.want(id, reason);
        self.engine.start_wanted(id);
    }

    /// Watches the folders of every job whose folders are reachable.
    fn follow_folders(&mut self, jobs: &[Job]) {
        let Some(watcher) = self.watcher.as_mut() else { return };
        let mut routes: Vec<(PathBuf, JobId)> = Vec::new();
        for job in jobs {
            for root in [&job.setup.source, &job.setup.destination] {
                if root.is_dir() {
                    routes.push((root.clone(), job.id.clone()));
                }
            }
        }
        watcher.follow(routes);
    }

    /// Notes which jobs saw changes that matter, ignoring Charon's own writes during and
    /// just after a run, and changes to excluded items.
    fn collect_changes(&mut self) {
        while let Ok((id, path)) = self.changes.try_recv() {
            if !self.engine.is_settling(&id, QUIET_AFTER_RUN) && self.engine.note_change(&id, &path) {
                self.changed.insert(id, Instant::now());
            }
        }
    }
}
