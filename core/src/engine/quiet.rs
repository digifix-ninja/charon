//! Drives kept quiet while a sync writes to them.
//!
//! A drive run by a driver Apple didn't make (Paragon's or Tuxera's NTFS, macFUSE: see
//! `Volume::run_by_an_add_on_driver`) works inside the system's own core, where a mistake
//! brings the whole Mac down. Paragon's NTFS driver did, a few seconds into a sync onto a USB
//! stick: it threw away one of its locks while Spotlight, reading the files the sync had just
//! written, still waited for it. Only the driver's maker can fix the driver, and Charon can't
//! keep the system's own services away, but it can keep from adding work of its own there.
//! So Charon works on one job at a time on such a drive (see `Engine::drive_used_by_another`):
//! while a job's run (its look, then its sync) or a list being made for it reads the drive,
//! no other job's run, list or look starts there. A run nobody asked for also waits for
//! another job's look in the background there; a run or a list the person asked for stops
//! that look instead, and it comes again once they're done. A run that waits says so in its
//! row, and starts by itself. And while a sync writes to the drive, its thread is meant to be
//! the only one of Charon's at work there:
//! * the sync refreshes its busy notes there itself, as it goes (see `run::Claim::beat`; the
//!   heartbeat does only when the sync is held up so long that they'd go stale);
//! * what the window, the autopilot and the reminders ask about folders on the drive is
//!   answered from what was found before (see `Engine::whereabouts_of`, `Engine::folders_of`);
//! * the autopilot stops watching the folders on the drive for changes until the sync is done.
//!   What changes there meanwhile isn't heard, as on a network share: the next regular look
//!   finds it, at most one look interval after the folders stopped being watched (see
//!   `Engine::finish`), and a list made before is carried out as one that waited (see
//!   `Engine::let_looks_end`).
//!
//! Before it writes, the sync waits a moment for other looks there to stop and for the
//! autopilot to stop watching; when either takes too long, it writes all the same, and the
//! app's log says so.
//!
//! A drive is told by where it's mounted, as the disk last said: a folder that leads there
//! through a link is asked about as ever, and so is one Charon knows nothing of yet. The lock
//! here is taken last: no other lock is taken while it's held.

use crate::engine::reach::Presence;
use crate::engine::runtime::JobRuntime;
use crate::engine::survey::Survey;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{Folder, Job, JobId};
use crate::plan::Side;
use crate::run::Claim;
use crate::scan::Volume;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// How long a sync waits for the looks it stopped to end, before it writes to a drive it
/// keeps quiet. A look stops at its next item, so this is only a bound for a slow drive.
const LOOKS_END_WITHIN: Duration = Duration::from_secs(5);
/// How long a sync waits for the autopilot to stop watching the folders on a drive it keeps
/// quiet, before it writes there. The autopilot does at its next tick, every second, unless a
/// share that doesn't answer holds it up.
const UNWATCHED_WITHIN: Duration = Duration::from_secs(3);
/// The name of the autopilot's thread, which follows the drives kept quiet itself.
pub(crate) const AUTOPILOT_THREAD: &str = "charon-autopilot";

/// The drives kept quiet now, and what's known of the folders on them.
#[derive(Default)]
pub(crate) struct QuietDrives {
    known: Mutex<Known>,
    /// How many keeps there were: every keep gets the next number, under the lock of `known`.
    keeps: AtomicU64,
    /// How many of them the autopilot followed: it stopped watching their folders.
    followed: AtomicU64,
    /// Whether the autopilot follows them at all (it runs, and watches folders).
    following: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct Known {
    drives: Vec<QuietDrive>,
    /// The disk's last answers about each folder asked about: what's said about a folder of
    /// another job on a drive kept quiet.
    presences: HashMap<PathBuf, Presence>,
    folders: HashMap<PathBuf, Folder>,
}

/// A drive one sync keeps quiet.
struct QuietDrive {
    keep: u64,
    job: JobId,
    /// The job's name, for the rows of the jobs that wait for it.
    name: String,
    /// Where the drive is mounted.
    mount: PathBuf,
    /// The sync's own folders on the drive, with the drive as the sync's look found it, and
    /// the folder as found right before the sync began (`None` when it didn't answer).
    roots: Vec<(PathBuf, Volume, Option<Folder>)>,
}

impl QuietDrive {
    /// Whether `folder` is on this drive: one of the sync's own folders, or a folder under
    /// the place it's mounted.
    fn holds(&self, folder: &Path) -> bool {
        folder.starts_with(&self.mount) || self.roots.iter().any(|(root, ..)| root == folder)
    }
}

impl Known {
    fn drive_of(&self, folder: &Path) -> Option<&QuietDrive> {
        self.drives.iter().find(|drive| drive.holds(folder))
    }

    /// What's said about `folder` while its drive is kept quiet: for the sync's own folders,
    /// there on the drive it found; for another job's, the last answer (there, on a drive not
    /// known, when it was never asked about). `None` when the folder isn't on a quiet drive.
    fn presence(&self, folder: &Path) -> Option<Presence> {
        let drive = self.drive_of(folder)?;
        if let Some((_, volume, _)) = drive.roots.iter().find(|(root, ..)| root == folder) {
            return Some(Presence::There(Some(volume.clone())));
        }
        Some(self.presences.get(folder).cloned().unwrap_or(Presence::There(None)))
    }

    /// `folder` as a folder two paths can be compared in, while its drive is kept quiet:
    /// `Some(None)` when that isn't known, which counts as maybe one place with anything.
    fn folder(&self, folder: &Path) -> Option<Option<Folder>> {
        let drive = self.drive_of(folder)?;
        let last = || self.folders.get(folder).cloned();
        if let Some((.., found)) = drive.roots.iter().find(|(root, ..)| root == folder) {
            return Some(found.clone().or_else(last));
        }
        Some(last())
    }

    /// Where the drive of `folder` is mounted, if it's run by an add-on driver, as far as
    /// Charon knows without asking: the drive kept quiet that holds it, or the disk's last
    /// answer about it.
    fn add_on_mount(&self, folder: &Path) -> Option<PathBuf> {
        if let Some(drive) = self.drive_of(folder) {
            return Some(drive.mount.clone());
        }
        match self.presences.get(folder) {
            Some(Presence::There(Some(volume))) if volume.run_by_an_add_on_driver() => Some(Engine::mount_of(folder, volume)),
            _ => None,
        }
    }
}

impl QuietDrives {
    fn known(&self) -> MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether `folder` is on a drive kept quiet now.
    pub fn holds(&self, folder: &Path) -> bool {
        self.known().drive_of(folder).is_some()
    }

    /// Whether one of `these` folders and one of `those` are on the same drive run by an
    /// add-on driver, as far as Charon knows without asking (see `Known::add_on_mount`).
    pub fn share_an_add_on_drive(&self, these: &[&Path], those: &[&Path]) -> bool {
        let known = self.known();
        let mounts = |folders: &[&Path]| folders.iter().filter_map(|folder| known.add_on_mount(folder)).collect::<Vec<_>>();
        let (these, those) = (mounts(these), mounts(those));
        these.iter().any(|one| those.iter().any(|other| one.starts_with(other) || other.starts_with(one)))
    }

    /// `folder` as last found (see `Known::folder`), and the name of the job whose sync keeps
    /// its drive quiet, if it's on a drive kept quiet now.
    pub fn remembered(&self, folder: &Path) -> Option<(Option<Folder>, String)> {
        let known = self.known();
        let name = known.drive_of(folder)?.name.clone();
        Some((known.folder(folder).flatten(), name))
    }

    /// The name of the job whose sync keeps the drive of one of `folders` quiet, if it isn't
    /// the job `id`.
    pub fn kept_by_another(&self, id: &JobId, folders: &[&Path]) -> Option<String> {
        let known = self.known();
        folders.iter().find_map(|folder| known.drive_of(folder).filter(|drive| &drive.job != id).map(|drive| drive.name.clone()))
    }

    /// Where each of `folders` is: from what's known for those on a drive kept quiet, from
    /// the disk (`ask`) for the others, whose answers are kept for when their drive is.
    pub fn presences(&self, folders: &[&Path], ask: impl FnOnce(&[&Path]) -> Vec<Option<Presence>>) -> Vec<Option<Presence>> {
        self.answer(folders, |known, folder| known.presence(folder).map(Some), |known, folder, found| {
            known.presences.insert(folder.to_path_buf(), found.clone());
        }, ask)
    }

    /// Each of `folders` as a folder two paths can be compared in (see `presences`).
    pub fn folders(&self, folders: &[&Path], ask: impl FnOnce(&[&Path]) -> Vec<Option<Folder>>) -> Vec<Option<Folder>> {
        self.answer(folders, Known::folder, |known, folder, found| {
            known.folders.insert(folder.to_path_buf(), found.clone());
        }, ask)
    }

    fn answer<T: Clone>(
        &self,
        folders: &[&Path],
        from_memory: impl Fn(&Known, &Path) -> Option<Option<T>>,
        remember: impl Fn(&mut Known, &Path, &T),
        ask: impl FnOnce(&[&Path]) -> Vec<Option<T>>,
    ) -> Vec<Option<T>> {
        let mut answers: Vec<Option<Option<T>>> = {
            let known = self.known();
            folders.iter().map(|folder| from_memory(&known, folder)).collect()
        };
        let asked: Vec<&Path> = folders.iter().zip(&answers).filter(|(_, answer)| answer.is_none()).map(|(folder, _)| *folder).collect();
        if !asked.is_empty() {
            let mut found = ask(&asked).into_iter();
            let mut known = self.known();
            for (folder, answer) in folders.iter().zip(answers.iter_mut()).filter(|(_, answer)| answer.is_none()) {
                let given = found.next().flatten();
                if let Some(given) = &given {
                    remember(&mut known, folder, given);
                }
                *answer = Some(given);
            }
        }
        answers.into_iter().map(Option::flatten).collect()
    }
}

/// A sync's keep on the drives it keeps quiet: they're kept quiet until it's dropped.
pub(crate) struct Quiet {
    engine: Engine,
    keep: u64,
    /// When the keep began: the folders on those drives may not have been watched since.
    since: Instant,
    /// The sync's folders on those drives.
    roots: Vec<PathBuf>,
}

impl Quiet {
    /// The sync's folders on the drives it keeps quiet.
    pub fn roots(&self) -> Vec<&Path> {
        self.roots.iter().map(PathBuf::as_path).collect()
    }
}

impl Drop for Quiet {
    /// Ends the keep.
    fn drop(&mut self) {
        self.engine.inner.quiet.known().drives.retain(|drive| drive.keep != self.keep);
    }
}

/// A sync's hold on its folders while it runs: its busy notes in them (see `run::Claim`), and
/// its keep on the drives it keeps quiet. Dropped once the sync is done, the notes first.
pub(crate) struct Claimed {
    notes: Claim,
    quiet: Option<Quiet>,
}

impl Claimed {
    pub fn new(notes: Claim, quiet: Option<Quiet>) -> Self {
        Self { notes, quiet }
    }

    /// When the sync began to keep drives quiet, if it does: what changed in its folders
    /// there since then, besides its own items, may not have been heard.
    pub fn unwatched_since(&self) -> Option<Instant> {
        self.quiet.as_ref().map(|quiet| quiet.since)
    }

    /// Refreshes the busy notes on the drives kept quiet when they're due (see `Claim::beat`).
    /// Called by the sync's own thread as it goes.
    pub fn beat(&self) {
        self.notes.beat();
    }
}

impl Engine {
    /// Keeps quiet, for the sync of `survey`, the drives of the job's folders that are run by
    /// an add-on driver, once background looks still reading them have stopped. `None` when
    /// there are none.
    pub(crate) fn keep_quiet(&self, job: &Job, survey: &Survey) -> Result<Option<Quiet>> {
        let sides: Vec<(PathBuf, Volume)> = [Side::Source, Side::Destination]
            .into_iter()
            .filter_map(|side| survey.volume(side).filter(|volume| volume.run_by_an_add_on_driver()).map(|volume| (survey.setup.root(side).to_path_buf(), volume.clone())))
            .collect();
        if sides.is_empty() {
            return Ok(None);
        }
        // Asked before the drives go quiet: other jobs' folders there are compared with these.
        let paths: Vec<&Path> = sides.iter().map(|(root, _)| root.as_path()).collect();
        let folders = self.folders_of(&paths);
        let mut drives: Vec<QuietDrive> = Vec::new();
        for ((root, volume), folder) in sides.into_iter().zip(folders) {
            let mount = Self::mount_of(&root, &volume);
            match drives.iter_mut().find(|drive| drive.mount == mount) {
                Some(drive) => drive.roots.push((root, volume, folder)),
                None => drives.push(QuietDrive { keep: 0, job: job.id.clone(), name: job.setup.name.clone(), mount, roots: vec![(root, volume, folder)] }),
            }
        }
        let roots: Vec<PathBuf> = drives.iter().flat_map(|drive| drive.roots.iter().map(|(root, ..)| root.clone())).collect();
        let keep = {
            let mut known = self.inner.quiet.known();
            let other = known.drives.iter().find(|kept| drives.iter().any(|drive| kept.mount.starts_with(&drive.mount) || drive.mount.starts_with(&kept.mount)));
            if let Some(other) = other {
                return Err(Error::DriveBusy(other.name.clone()));
            }
            // Numbered with the lock held: the autopilot that reads the count before it asks
            // which folders are kept quiet finds every keep counted there.
            let keep = self.inner.quiet.keeps.fetch_add(1, Ordering::SeqCst);
            for drive in &mut drives {
                drive.keep = keep;
            }
            log::info!("job {}: keeps {} quiet while it syncs (a driver Apple didn't make)", job.id, drives.iter().map(|d| d.mount.display().to_string()).collect::<Vec<_>>().join(", "));
            known.drives.extend(drives);
            keep
        };
        let quiet = Quiet { engine: self.clone(), keep, since: Instant::now(), roots };
        self.let_looks_end(job);
        self.wait_until_unwatched(job, keep);
        Ok(Some(quiet))
    }

    /// Waits a moment for the autopilot to stop watching the folders on the drives of the
    /// keep `keep`: watching them has the system work on the drive for Charon too.
    fn wait_until_unwatched(&self, job: &Job, keep: u64) {
        let quiet = &self.inner.quiet;
        if !quiet.following.load(Ordering::SeqCst) || thread::current().name() == Some(AUTOPILOT_THREAD) {
            return;
        }
        let until = Instant::now() + UNWATCHED_WITHIN;
        while quiet.followed.load(Ordering::SeqCst) <= keep {
            if Instant::now() >= until {
                log::warn!("job {}: the folder watcher still follows a drive it keeps quiet", job.id);
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// How many keeps there were so far: read by the autopilot before it asks which folders
    /// are kept quiet (see `kept_quiet`), and handed back to `quiet_followed` once it stopped
    /// watching them.
    pub(crate) fn quiet_keeps(&self) -> u64 {
        self.inner.quiet.keeps.load(Ordering::SeqCst)
    }

    /// The autopilot stopped watching the folders of the first `keeps` keeps.
    pub(crate) fn quiet_followed(&self, keeps: u64) {
        self.inner.quiet.followed.fetch_max(keeps, Ordering::SeqCst);
        self.inner.quiet.following.store(true, Ordering::SeqCst);
    }

    /// Where the drive of `root` (found as `volume`) is mounted; `root` itself when that
    /// can't be told, so nothing but the job's own folder counts as on it.
    fn mount_of(root: &Path, volume: &Volume) -> PathBuf {
        let mount = Path::new(&volume.mount);
        if mount.is_absolute() && mount.parent().is_some() {
            mount.to_path_buf()
        } else {
            root.to_path_buf()
        }
    }

    /// Stops the background looks of other jobs whose folders are on the drives `job`'s sync
    /// now keeps quiet (they count for nothing, and come again once the sync is done), and
    /// waits a moment for them and any look of the job's own (stopped as its run began) to
    /// end. A look the person asked for isn't stopped, only waited for. Most were stopped or
    /// waited for as the run began (see `drive_used_by_another`): this is for those that began
    /// before Charon knew the drive.
    ///
    /// The other jobs' folders there aren't watched from now on, so a list of theirs made
    /// before can't be told outgrown by what changes there meanwhile: it's carried out as one
    /// that waited, its folders looked at again once it's done (see `carry_out`). It isn't
    /// refused for that: each step checks its items right before it acts.
    fn let_looks_end(&self, job: &Job) {
        let on_drive: Vec<JobId> = self
            .job_list()
            .into_iter()
            .filter(|other| [&other.setup.source, &other.setup.destination].iter().any(|folder| self.kept_quiet(folder)))
            .map(|other| other.id)
            .collect();
        for id in on_drive.iter().filter(|other| *other != &job.id) {
            self.change_known_runtime(id, |rt| {
                if rt.checking.is_some() && rt.preparing.is_none() {
                    rt.outdate_looks();
                    rt.last_checked = None;
                }
                if let Some(review) = &rt.review {
                    review.unheard.store(true, Ordering::Relaxed);
                }
            });
        }
        let until = Instant::now() + LOOKS_END_WITHIN;
        let looking = || {
            let runtime = self.runtime();
            on_drive.iter().any(|id| runtime.get(id).is_some_and(|rt| rt.checking.is_some() || rt.preparing.is_some()))
        };
        while looking() {
            if Instant::now() >= until {
                log::warn!("job {}: a look at a folder on a drive it keeps quiet still reads it", job.id);
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The name of another job at work on a drive run by an add-on driver that one of the
    /// folders of the job `id` is on: its run (its look or its sync), a list being made for
    /// it, or with `looks` a look of it in the background. The job isn't looked at, listed or
    /// synced until that's done (see the module's notes). Its folders there are still asked
    /// about meanwhile (where they are, and whether they overlap another job's), until a sync
    /// keeps the drive quiet: from then on, they're answered from memory.
    pub(crate) fn drive_used_by_another(&self, id: &JobId, looks: bool) -> Option<String> {
        let state = self.state();
        let runtime = self.runtime();
        let job = state.jobs.iter().find(|job| &job.id == id)?;
        self.drive_used_among(&state.jobs, &runtime, job, looks)
    }

    /// `drive_used_by_another` for `job`, with the live state locked (`jobs`, `runtime`).
    pub(crate) fn drive_used_among(&self, jobs: &[Job], runtime: &HashMap<JobId, JobRuntime>, job: &Job, looks: bool) -> Option<String> {
        let mine = [job.setup.source.as_path(), job.setup.destination.as_path()];
        jobs.iter()
            .filter(|other| other.id != job.id)
            .filter(|other| runtime.get(&other.id).is_some_and(|rt| rt.is_running() || rt.preparing.is_some() || (looks && rt.is_checking())))
            .find(|other| self.inner.quiet.share_an_add_on_drive(&mine, &[&other.setup.source, &other.setup.destination]))
            .map(|other| other.setup.name.clone())
            .or_else(|| self.kept_quiet_by_another(job))
    }

    /// Stops the background looks of other jobs on a drive run by an add-on driver that one
    /// of `job`'s folders is on, as its run or a list being made for it begins: they count
    /// for nothing, and come again once it's done. Called with the live state locked.
    pub(crate) fn stop_looks_sharing_a_drive(&self, jobs: &[Job], runtime: &mut HashMap<JobId, JobRuntime>, job: &Job) {
        let mine = [job.setup.source.as_path(), job.setup.destination.as_path()];
        for other in jobs.iter().filter(|other| other.id != job.id) {
            let Some(rt) = runtime.get_mut(&other.id) else { continue };
            if rt.is_checking() && rt.preparing.is_none() && self.inner.quiet.share_an_add_on_drive(&mine, &[&other.setup.source, &other.setup.destination]) {
                rt.outdate_looks();
                rt.last_checked = None;
            }
        }
    }

    /// Each of `paths` as a folder two paths can be compared in, for saving a job (see
    /// `JobSetup::clashes_as`): from the disk, but as last found for those on a drive a sync
    /// keeps quiet. One of those that was never found can't be compared, so the save waits:
    /// `Err` names the job whose sync keeps the drive quiet.
    pub(crate) fn folders_for_saving(&self, paths: &[&Path]) -> std::result::Result<Vec<Option<Folder>>, String> {
        paths
            .iter()
            .map(|path| match self.inner.quiet.remembered(path) {
                Some((Some(folder), _)) => Ok(Some(folder)),
                Some((None, name)) => Err(name),
                None => Ok(Some(Folder::of(path))),
            })
            .collect()
    }

    /// The name of the job whose sync keeps the drive of one of `job`'s folders quiet, if
    /// another job's does: `job` doesn't look or sync until it's done.
    pub(crate) fn kept_quiet_by_another(&self, job: &Job) -> Option<String> {
        self.inner.quiet.kept_by_another(&job.id, &[&job.setup.source, &job.setup.destination])
    }

    /// Whether `folder` is on a drive a sync keeps quiet now (see the module's notes).
    pub(crate) fn kept_quiet(&self, folder: &Path) -> bool {
        self.inner.quiet.holds(folder)
    }
}

#[cfg(test)]
mod testing {
    use super::*;
    use crate::engine::runs::Crossing;
    use crate::engine::survey::Purpose;

    /// A sync that has looked at its job's folders and claimed them, and goes no further:
    /// dropped, it ends without changing anything.
    pub(crate) struct HeldSync<'a> {
        engine: &'a Engine,
        id: JobId,
        claim: Option<Claimed>,
    }

    impl Drop for HeldSync<'_> {
        fn drop(&mut self) {
            self.claim.take();
            self.engine.change_runtime(&self.id, |rt| {
                Engine::unmark_run(rt);
            });
        }
    }

    impl Engine {
        /// Marks the job as just looked at.
        pub(crate) fn just_looked_at(&self, id: &JobId) {
            self.change_runtime(id, |rt| rt.last_checked = Some(Instant::now()));
        }

        /// Whether the job's list `review_id` waits for an OK but can't be confirmed anymore,
        /// as its folders may have changed.
        pub(crate) fn list_is_stale(&self, id: &JobId, review_id: &str) -> bool {
            self.runtime().get(id).and_then(|rt| rt.review.as_ref()).is_some_and(|review| review.id == review_id && review.stale.load(Ordering::Relaxed))
        }

        /// Begins a sync of `id` the person asked for, and holds it right after it claimed
        /// its folders (see `HeldSync`).
        pub(crate) fn hold_a_sync(&self, id: &JobId) -> Result<HeldSync<'_>> {
            let (job, cancel) = self.begin_run(id, &Crossing::unattended("for a test", true), |_, _| Ok(()))?;
            let claimed = self.survey(&job, &cancel, Purpose::Automatic).and_then(|survey| self.claim_folders(&job, &survey));
            match claimed {
                Ok(claim) => Ok(HeldSync { engine: self, id: id.clone(), claim: Some(claim) }),
                Err(e) => {
                    self.change_runtime(id, |rt| {
                        Engine::unmark_run(rt);
                    });
                    Err(e)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::runs::Crossing;
    use crate::engine::survey::Purpose;
    use crate::engine::Whereabouts;
    use crate::model::{ConflictRule, JobSetup, SyncMode, Trigger};
    use crate::scan::pretend::AddOnDrive;
    use std::fs;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Three jobs: "Documents" and "Photos" back up into folders on a stick run by an add-on
    /// driver, "Notes" from one folder of this computer's own disk to another.
    struct Jobs {
        _tmp: tempfile::TempDir,
        _stick: AddOnDrive,
        engine: Engine,
        documents: JobId,
        photos: JobId,
        notes: JobId,
        /// The two jobs' folders on the stick.
        on_stick: [PathBuf; 2],
    }

    fn jobs() -> Jobs {
        let tmp = tempfile::tempdir().unwrap();
        let (home, stick) = (tmp.path().join("home"), tmp.path().join("stick"));
        let pretending = AddOnDrive::at(&stick);
        let folders = [home.join("docs"), stick.join("docs"), home.join("photos"), stick.join("photos"), home.join("notes"), home.join("copies")];
        for folder in &folders {
            fs::create_dir_all(folder).unwrap();
        }
        for source in [&folders[0], &folders[2], &folders[4]] {
            fs::write(source.join("letter.txt"), "Dear…").unwrap();
        }
        let engine = Engine::open(tmp.path().join("data"), Arc::new(|_| {})).unwrap();
        let add = |name: &str, source: &PathBuf, destination: &PathBuf| {
            let setup = JobSetup {
                name: name.into(),
                source: source.clone(),
                destination: destination.clone(),
                mode: SyncMode::Backup,
                conflict: ConflictRule::Ask,
                trigger: Trigger::Manual,
                excludes: vec![],
                safe_delete: true,
                full_automation: false,
            };
            engine.add_job(setup).unwrap().job.id
        };
        let documents = add("Documents", &folders[0], &folders[1]);
        let photos = add("Photos", &folders[2], &folders[3]);
        let notes = add("Notes", &folders[4], &folders[5]);
        let on_stick = [folders[1].clone(), folders[3].clone()];
        Jobs { _tmp: tmp, _stick: pretending, engine, documents, photos, notes, on_stick }
    }

    fn until(done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "took too long");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_sync_on_a_drive_run_by_an_add_on_driver_keeps_it_quiet_until_it_is_done() {
        let Jobs { _tmp, _stick, engine, documents, photos, notes, on_stick: [docs_on_stick, photos_on_stick] } = jobs();
        let all = engine.job_list();
        let before = engine.whereabouts_of(&all);
        assert!(before.iter().all(Whereabouts::both_there), "{before:?}");
        let held_at = Instant::now();
        let held = engine.hold_a_sync(&documents).unwrap();
        assert!(engine.kept_quiet(&docs_on_stick) && engine.kept_quiet(&photos_on_stick));
        let home_docs = engine.stored_job(&documents).unwrap().setup.source;
        assert!(!engine.kept_quiet(&home_docs), "the source is on another drive");

        // Its busy note on the stick is refreshed by the sync itself as it goes (here, never;
        // the heartbeat would only once that's overdue), the one at home by the heartbeat.
        let heartbeat = |root: &Path| {
            let items = fs::read_dir(root.join(".charon")).unwrap().flatten();
            let file = items.map(|item| item.path()).find(|path| path.extension().is_some_and(|e| e == "busy")).unwrap();
            serde_json::from_slice::<serde_json::Value>(&fs::read(file).unwrap()).unwrap()["heartbeat"].as_str().unwrap().to_string()
        };
        let (on_stick, at_home) = (heartbeat(&docs_on_stick), heartbeat(&home_docs));
        until(|| heartbeat(&home_docs) != at_home);
        thread::sleep(Duration::from_millis(200));
        let still = heartbeat(&docs_on_stick);
        if held_at.elapsed() < crate::run::QUIET_OVERDUE {
            assert_eq!(still, on_stick);
        }

        // Nothing asks the drive anything: folders there that stopped answering are still
        // said to be as they were.
        let silenced = [engine.silence(&docs_on_stick), engine.silence(&photos_on_stick)];
        let found = engine.whereabouts_of(&all);
        assert!(found.iter().all(Whereabouts::both_there), "{found:?}");
        assert!(matches!(&found[0].destination, Presence::There(Some(volume)) if volume.run_by_an_add_on_driver()));
        assert!(engine.folders_of(&[docs_on_stick.as_path(), photos_on_stick.as_path()]).iter().all(Option::is_some));

        // The other job on the stick waits, and isn't looked at; the job elsewhere goes on.
        let refused = engine.prepare_review(&photos).unwrap_err();
        assert!(matches!(&refused, Error::DriveBusy(name) if name == "Documents"), "{refused:?}");
        assert!(refused.to_string().contains("same drive"));
        assert!(matches!(engine.begin_run(&photos, &Crossing::unattended("for a test", true), |_, _| Ok(())), Err(Error::DriveBusy(_))));
        engine.change_runtime(&photos, |rt| rt.last_checked = Some(Instant::now()));
        engine.check(&photos);
        assert!(engine.runtime().get(&photos).is_some_and(|rt| rt.checking.is_none() && rt.last_checked.is_none()), "looked at once the sync is done");
        engine.prepare_review(&notes).unwrap();
        // A sync that got past those checks before (its look began first) can't keep the
        // drive quiet too, so it waits at its claim.
        let job = engine.stored_job(&photos).unwrap();
        let survey = engine.survey(&job, &AtomicBool::new(false), Purpose::Check).unwrap();
        assert!(matches!(engine.claim_folders(&job, &survey), Err(Error::DriveBusy(name)) if name == "Documents"));

        drop(held);
        assert!(!engine.kept_quiet(&docs_on_stick) && !engine.kept_quiet(&photos_on_stick));
        drop(silenced);
        until(|| !engine.is_silent(&docs_on_stick) && !engine.is_silent(&photos_on_stick));
        engine.prepare_review(&photos).unwrap();
    }

    #[test]
    fn a_sync_stops_background_looks_on_the_drive_and_waits_for_them_to_end() {
        let Jobs { _tmp, _stick, engine, documents, photos, .. } = jobs();
        // A background look at "Photos", as if it were reading the stick: its thread ends a
        // moment after it's told to stop.
        let flag = Arc::new(AtomicBool::new(false));
        engine.change_runtime(&photos, |rt| rt.checking = Some(flag.clone()));
        let epoch = engine.runtime()[&photos].epoch;
        let looker = {
            let (engine, flag, photos) = (engine.clone(), flag.clone(), photos.clone());
            thread::spawn(move || {
                until(|| flag.load(Ordering::Relaxed));
                thread::sleep(Duration::from_millis(100));
                engine.change_runtime(&photos, |rt| rt.checking = None);
            })
        };
        let started = Instant::now();
        let held = engine.hold_a_sync(&documents).unwrap();
        assert!(engine.runtime()[&photos].checking.is_none(), "the sync didn't wait for the look to end");
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(engine.runtime()[&photos].epoch != epoch, "what the look found counts for nothing");
        looker.join().unwrap();
        drop(held);

        // A list the person asked for isn't stopped: the sync waits until it's made.
        let asked = Arc::new(AtomicBool::new(false));
        engine.change_runtime(&photos, |rt| rt.preparing = Some(asked.clone()));
        assert!(matches!(engine.hold_a_sync(&documents), Err(Error::DriveBusy(name)) if name == "Photos"));
        assert!(!asked.load(Ordering::Relaxed));
        engine.change_runtime(&photos, |rt| rt.preparing = None);
        drop(engine.hold_a_sync(&documents).unwrap());
    }

    #[test]
    fn one_job_at_a_time_works_on_the_drive_from_the_start_of_its_run() {
        let Jobs { _tmp, _stick, engine, documents, photos, notes, .. } = jobs();
        // What the autopilot asks every second tells which drives the folders are on.
        engine.whereabouts_of(&engine.job_list());
        // A background look at Photos reads the stick when Documents' run begins: the run
        // comes first, and the look comes again once it's done.
        let flag = Arc::new(AtomicBool::new(false));
        engine.change_runtime(&photos, |rt| {
            rt.checking = Some(flag.clone());
            rt.last_checked = Some(Instant::now());
        });
        let epoch = engine.runtime()[&photos].epoch;
        let crossing = Crossing::unattended("for a test", true);
        engine.begin_run(&documents, &crossing, |_, _| Ok(())).unwrap();
        assert!(flag.load(Ordering::Relaxed), "the look was stopped");
        assert!(engine.runtime()[&photos].epoch != epoch && engine.runtime()[&photos].last_checked.is_none());
        engine.change_runtime(&photos, |rt| rt.checking = None);

        // Still looking at its folders, Documents' run keeps the others off the stick.
        let refused = engine.begin_run(&photos, &crossing, |_, _| Ok(())).unwrap_err();
        assert!(matches!(&refused, Error::DriveBusy(name) if name == "Documents"), "{refused:?}");
        assert!(matches!(engine.prepare_review(&photos), Err(Error::DriveBusy(_))));
        // No look either, and the person who asks for one is told why.
        let refused = engine.look_now(&photos).unwrap_err();
        assert!(refused.to_string().contains("“Documents” is using the same drive"), "{refused}");
        engine.check(&photos);
        assert!(engine.runtime()[&photos].checking.is_none() && engine.runtime()[&photos].last_checked.is_none(), "it comes once the run is done");
        engine.prepare_review(&notes).unwrap();
        // The caller's checks still come first: a run that can't go ahead says why.
        let refused = engine.begin_run(&photos, &crossing, |_, _| Err(Error::Invalid("not now".into()))).unwrap_err();
        assert_eq!(refused.to_string(), "not now");

        engine.change_runtime(&documents, |rt| {
            Engine::unmark_run(rt);
        });
        engine.look_now(&photos).unwrap();
        let looked = engine.runtime().get(&photos).is_some_and(|rt| rt.checking.is_some() || rt.last_checked.is_some());
        assert!(looked, "looked at once the run is done");
        until(|| engine.runtime()[&photos].checking.is_none());
    }

    #[test]
    fn a_run_nobody_asked_for_waits_for_a_look_on_the_drive_and_one_asked_for_stops_it() {
        let Jobs { _tmp, _stick, engine, documents, photos, .. } = jobs();
        engine.whereabouts_of(&engine.job_list());
        let flag = Arc::new(AtomicBool::new(false));
        engine.change_runtime(&photos, |rt| rt.checking = Some(flag.clone()));
        // Stopping the look for every run a trigger starts could keep Photos from ever being
        // looked at while Documents syncs on change.
        let refused = engine.begin_run(&documents, &Crossing::unattended("after changes", false), |_, _| Ok(())).unwrap_err();
        assert!(matches!(&refused, Error::DriveBusy(name) if name == "Photos"), "{refused:?}");
        assert!(!flag.load(Ordering::Relaxed), "the look goes on");
        // The person asked for this one: it comes first.
        engine.begin_run(&documents, &Crossing::unattended("started by hand", true), |_, _| Ok(())).unwrap();
        assert!(flag.load(Ordering::Relaxed), "the look was stopped");
        engine.change_runtime(&documents, |rt| {
            Engine::unmark_run(rt);
        });
    }

    #[test]
    fn a_look_that_began_before_charon_knew_the_drive_is_stopped_at_the_keep() {
        let Jobs { _tmp, _stick, engine, documents, photos, .. } = jobs();
        // Nothing is known yet of where the folders are, as right after a launch (once the
        // first looks are done: they say).
        until(|| engine.runtime().values().all(|rt| rt.checking.is_none()));
        engine.inner.quiet.known().presences.clear();
        let flag = Arc::new(AtomicBool::new(false));
        engine.change_runtime(&photos, |rt| {
            rt.checking = Some(flag.clone());
            rt.last_checked = Some(Instant::now());
        });
        let epoch = engine.runtime()[&photos].epoch;
        let looker = {
            let (engine, flag, photos) = (engine.clone(), flag.clone(), photos.clone());
            thread::spawn(move || {
                until(|| flag.load(Ordering::Relaxed));
                thread::sleep(Duration::from_millis(100));
                engine.change_runtime(&photos, |rt| rt.checking = None);
            })
        };
        let crossing = Crossing::unattended("for a test", true);
        let (job, cancel) = engine.begin_run(&documents, &crossing, |_, _| Ok(())).unwrap();
        assert!(!flag.load(Ordering::Relaxed), "the run couldn't tell the look was on the same drive");
        let survey = engine.survey(&job, &cancel, Purpose::Automatic).unwrap();
        let claim = engine.claim_folders(&job, &survey).unwrap();
        let (now, last_checked, checking) = engine.runtime().get(&photos).map(|rt| (rt.epoch, rt.last_checked, rt.checking.is_some())).unwrap();
        assert!(flag.load(Ordering::Relaxed) && now != epoch && last_checked.is_none(), "the keep stopped it");
        assert!(!checking, "and the sync waited for it to end");
        looker.join().unwrap();
        drop(claim);
        engine.change_runtime(&documents, |rt| {
            Engine::unmark_run(rt);
        });
    }

    #[test]
    fn a_list_confirmed_while_the_drive_is_busy_is_checked_then_waits_for_it() {
        let Jobs { _tmp, _stick, engine, documents, photos, .. } = jobs();
        engine.whereabouts_of(&engine.job_list());
        let list = engine.prepare_review(&photos).unwrap();
        let held = engine.hold_a_sync(&documents).unwrap();
        assert!(engine.runtime()[&photos].review.as_ref().unwrap().unheard.load(Ordering::Relaxed), "its folder on the stick isn't watched");
        let stale = |stale: bool| engine.change_runtime(&photos, |rt| rt.review.as_ref().unwrap().stale.store(stale, Ordering::Relaxed));
        stale(true);
        let refused = engine.approve(&photos, &list.review_id, true).unwrap_err();
        assert!(refused.to_string().contains("Things changed"), "a list that can't go ahead is refused, not kept waiting: {refused}");
        stale(false);
        let approved = engine.approve(&photos, &list.review_id, true).unwrap();
        assert!(matches!(&approved, crate::engine::Approval::Waiting { waiting_for } if waiting_for == "Documents"), "{approved:?}");
        // It waits as the drive is busy, and starts once it's free.
        assert!(!engine.start_confirmed(&photos));
        drop(held);
        assert!(engine.start_confirmed(&photos));
        until(|| !engine.is_running(&photos));
    }

    #[test]
    fn a_list_made_before_another_sync_kept_the_drive_quiet_can_be_confirmed_and_is_looked_at_again() {
        let Jobs { _tmp, _stick, engine, documents, photos, .. } = jobs();
        let list = engine.prepare_review(&photos).unwrap();
        drop(engine.hold_a_sync(&documents).unwrap());
        // Photos' folder on the stick wasn't watched meanwhile. Nothing was heard, so the list
        // can be confirmed (each step checks its items right before it acts), but what changed
        // there isn't in it: once it's carried out, the folders are looked at again at once.
        assert!(!engine.list_is_stale(&photos, &list.review_id));
        let approved = engine.approve(&photos, &list.review_id, true).unwrap();
        assert!(matches!(approved, crate::engine::Approval::Started), "{approved:?}");
        until(|| !engine.is_running(&photos));
        let rt = &engine.runtime()[&photos];
        assert!(rt.look.is_none() && rt.last_checked.is_none(), "looked at again at once");
    }

    #[test]
    fn after_a_sync_that_kept_the_drive_quiet_the_regular_look_counts_from_its_keep() {
        let Jobs { _tmp, _stick, engine, documents, .. } = jobs();
        let crossing = Crossing::unattended("for a test", true);
        let (job, cancel) = engine.begin_run(&documents, &crossing, |_, _| Ok(())).unwrap();
        let survey = engine.survey(&job, &cancel, Purpose::Automatic).unwrap();
        let claim = engine.claim_folders(&job, &survey).unwrap();
        thread::sleep(Duration::from_millis(100));
        engine.cross(&job, &survey, &cancel, &crossing, claim);
        // Nothing heard counts against what it did: its row says what's left, as after any
        // sync, and it isn't looked at again at once (with a file that fails every time, a
        // job that syncs on change would sync again and again). But what changed on the stick
        // meanwhile wasn't heard: the regular look counts from when it stopped being watched.
        let rt = &engine.runtime()[&documents];
        assert!(rt.look.is_some(), "its look still says what's left");
        let (checked, finished) = (rt.last_checked.unwrap(), rt.finished_at.unwrap());
        assert!(finished.duration_since(checked) >= Duration::from_millis(100), "{checked:?} {finished:?}");
    }

    #[test]
    fn removing_or_saving_a_job_leaves_a_drive_kept_quiet_alone() {
        let Jobs { _tmp, _stick, engine, documents, photos, on_stick: [docs_on_stick, photos_on_stick], .. } = jobs();
        engine.whereabouts_of(&engine.job_list());
        let stick = docs_on_stick.parent().unwrap().to_path_buf();
        crate::run::Marker::new(&photos, Side::Destination, "token", 1).write(&photos_on_stick).unwrap();
        let marked = || matches!(crate::run::Marker::read(&photos_on_stick, &photos), crate::run::Found::Marker(_));
        let held = engine.hold_a_sync(&documents).unwrap();
        let refused = engine.remove_job(&photos).unwrap_err();
        assert!(refused.to_string().contains("Remove “Photos” once it's done"), "{refused}");
        assert!(marked(), "nothing was taken out of its folder");
        // A Mirror job into a folder on the stick that was never asked about: whether it
        // would clash with a job there can't be told without asking the drive.
        let mut mirror = engine.stored_job(&photos).unwrap().setup;
        (mirror.name, mirror.mode, mirror.destination) = ("Mirror".into(), SyncMode::Mirror, docs_on_stick.join("inside"));
        crate::model::asking::taken();
        let refused = engine.add_job(mirror.clone()).unwrap_err();
        assert!(refused.to_string().contains("Save this job once it's done"), "{refused}");
        // Folders found before are compared as they were found: a Mirror into Documents'
        // folder clashes with it, and saving says so at once.
        mirror.destination = docs_on_stick.clone();
        let refused = engine.add_job(mirror).unwrap_err();
        assert!(refused.to_string().contains("overlap with those of “Documents”"), "{refused}");
        let asked = crate::model::asking::taken();
        assert!(!asked.is_empty() && !asked.iter().any(|path| path.starts_with(&stick)), "the stick wasn't asked about: {asked:?}");

        drop(held);
        engine.remove_job(&photos).unwrap();
        assert!(!marked(), "its marker went with it");
    }

    #[test]
    fn a_folder_the_drive_kept_quiet_didnt_answer_for_is_as_it_was_last_found() {
        let (stick, docs) = (Path::new("/Volumes/STICK"), Path::new("/Volumes/STICK/docs"));
        let mut known = Known::default();
        known.folders.insert(docs.to_path_buf(), Folder::of(Path::new("/")));
        let volume = Volume { mount: stick.display().to_string(), ..Volume::of(Path::new("/")) };
        known.drives.push(QuietDrive { keep: 1, job: "a".into(), name: "A".into(), mount: stick.into(), roots: vec![(docs.into(), volume, None)] });
        assert!(matches!(known.folder(docs), Some(Some(_))));
        assert!(matches!(known.folder(Path::new("/Volumes/STICK/photos")), Some(None)), "never asked: maybe one place with anything");
        assert!(known.folder(Path::new("/Users")).is_none(), "not on the drive");
    }

    #[test]
    fn a_sync_waits_for_the_autopilot_to_stop_watching_the_drive() {
        let Jobs { _tmp, _stick, engine, documents, on_stick: [docs_on_stick, _], .. } = jobs();
        // The autopilot follows the keeps, and has followed all of them so far.
        engine.quiet_followed(engine.quiet_keeps());
        let syncing = {
            let engine = engine.clone();
            thread::spawn(move || {
                let started = Instant::now();
                let held = engine.hold_a_sync(&documents).unwrap();
                let waited = started.elapsed();
                drop(held);
                waited
            })
        };
        // Its next tick stops watching the stick, and says so.
        until(|| engine.kept_quiet(&docs_on_stick));
        thread::sleep(Duration::from_millis(200));
        engine.quiet_followed(engine.quiet_keeps());
        let waited = syncing.join().unwrap();
        assert!(waited >= Duration::from_millis(200) && waited < UNWATCHED_WITHIN, "{waited:?}");
    }

    #[test]
    fn a_sync_elsewhere_keeps_nothing_quiet() {
        let Jobs { _tmp, _stick, engine, notes, on_stick, .. } = jobs();
        let held = engine.hold_a_sync(&notes).unwrap();
        assert!(on_stick.iter().all(|folder| !engine.kept_quiet(folder)));
        assert!(engine.inner.quiet.known().drives.is_empty());
        drop(held);
    }

    #[test]
    fn a_mount_that_cant_be_told_keeps_only_the_folder_itself_quiet() {
        let (mount, root) = if cfg!(windows) { (r"E:\STICK", Path::new(r"E:\STICK\docs")) } else { ("/Volumes/STICK", Path::new("/Volumes/STICK/docs")) };
        let volume = |mount: &str| Volume { mount: mount.into(), ..Volume::of(Path::new("/")) };
        assert_eq!(Engine::mount_of(root, &volume(mount)), PathBuf::from(mount));
        for unknown in ["", "/", "C1A2-33F0"] {
            assert_eq!(Engine::mount_of(root, &volume(unknown)), root, "{unknown:?}");
        }
        let drive = QuietDrive { keep: 0, job: "a".into(), name: "A".into(), mount: "/Volumes/STICK".into(), roots: vec![] };
        assert!(drive.holds(Path::new("/Volumes/STICK/photos")) && drive.holds(Path::new("/Volumes/STICK")));
        assert!(!drive.holds(Path::new("/Volumes/STICK 1/photos")), "another drive's name only starts the same");
    }
}
