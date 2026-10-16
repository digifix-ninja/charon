//! The autopilot: runs jobs by themselves.
//!
//! * `mod.rs`      – `Autopilot`: a background loop that looks at every job once a second
//! * `schedule.rs` – `Schedule`: when an interval or daily job is due next
//! * `watcher.rs`  – `FolderWatcher`: hears about changes inside the folders
//!
//! Every second, for each job, in this order:
//! 1. **Drive connected:** both folders just became reachable → sync.
//! 2. **Changes settled:** the folders changed and have been quiet for a moment → sync
//!    (on-change jobs) or look for pending changes (all other jobs, so their rows turn amber).
//! 3. **Schedule:** an interval or daily job is due → sync.
//! 4. **Regular check:** look for pending changes every few minutes, in case the watcher
//!    missed something (network shares, sleep). An on-change job with changes waiting syncs.

mod schedule;
mod watcher;

pub use schedule::Schedule;
pub use watcher::FolderWatcher;

use crate::engine::Engine;
use crate::model::{Job, JobId, Trigger};
use chrono::Utc;
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
/// How long changes must settle before a job that doesn't sync on change is checked.
const CHECK_SETTLE: Duration = Duration::from_secs(5);

pub struct Autopilot {
    engine: Engine,
    watcher: Option<FolderWatcher>,
    changes: Receiver<JobId>,
    /// When each job's folders last changed (cleared once handled).
    changed: HashMap<JobId, Instant>,
    /// Whether both folders of each job were reachable at the last tick.
    reachable: HashMap<JobId, bool>,
}

impl Autopilot {
    /// Starts the autopilot on its own thread. It runs until the app quits.
    pub fn start(engine: Engine) {
        let spawned = thread::Builder::new().name("charon-autopilot".into()).spawn(move || {
            let (sender, changes) = mpsc::channel();
            let watcher = FolderWatcher::new(sender)
                .map_err(|e| log::error!("folder watching is unavailable: {e}"))
                .ok();
            let mut pilot = Autopilot { engine, watcher, changes, changed: HashMap::new(), reachable: HashMap::new() };
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

        for job in &jobs {
            let id = &job.id;
            let reachable = job.setup.source.is_dir() && job.setup.destination.is_dir();
            let was_reachable = self.reachable.insert(id.clone(), reachable);
            if was_reachable.is_some() && was_reachable != Some(reachable) {
                self.engine.publish(id); // the row turns offline / back online
            }
            if !reachable || self.engine.is_running(id) {
                continue;
            }
            let automatic = !settings.paused_all && !job.paused;

            // 1. Drive connected (also at launch, when the drive was already there).
            if automatic && job.setup.trigger == Trigger::DriveConnected && was_reachable != Some(true) {
                self.run(id, "because the drive was connected");
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
                    if automatic && job.setup.trigger.syncs_on_change() {
                        self.run(id, "after changes");
                    } else {
                        self.engine.check(id);
                    }
                    continue;
                }
            }
            // 3. Schedule.
            if automatic && Schedule::is_due(job, Utc::now()) {
                self.run(id, "on schedule");
                continue;
            }
            // 4. Regular check.
            if automatic && job.setup.trigger.syncs_on_change() && self.engine.has_pending(id) {
                self.run(id, "for changes found by a check");
            } else if self.engine.check_due(id, settings.check_minutes) {
                self.engine.check(id);
            }
        }
        self.reachable.retain(|id, _| jobs.iter().any(|job| &job.id == id));
    }

    fn run(&self, id: &JobId, reason: &str) {
        if let Err(e) = self.engine.start_run(id, &format!("automatically {reason}")) {
            log::debug!("automatic run skipped: {e}");
        }
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

    /// Notes which jobs saw changes, ignoring Charon's own writes during and just after a run.
    fn collect_changes(&mut self) {
        while let Ok(id) = self.changes.try_recv() {
            if !self.engine.is_settling(&id, QUIET_AFTER_RUN) {
                self.changed.insert(id, Instant::now());
            }
        }
    }
}
