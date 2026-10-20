//! Jobs that share folders, and other syncs at work in the same folders: two syncs never
//! change the same files at once, and a job never undoes another job's work unnoticed,
//! whatever the folders' paths look like.
//!
//! Real folders on disk, the real engine. A share mounted under two names can't be made
//! without special rights: it's played here by a link made after the jobs were saved, and
//! the engine's own tests play a second mount past the path checks (see `engine::overlaps`).
//! A copy of a job's folder, hidden `.charon` folder and all, is made as the person would.

use charon_core::run::{BusyNote, BUSY_EXTENSION};
use charon_core::store::DataFolder;
use charon_core::{
    Approval, ConflictRule, Engine, EngineEvent, Error, JobSetup, JobStatus, JobView, Level, Phase, Review, SyncMode, SyncStart, Trigger,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// How long any wait may take before the test fails.
const DEADLINE: Duration = Duration::from_secs(60);

// ==================================================================== helpers

type Hook = Box<dyn FnOnce() + Send>;

/// An engine that can run a one-shot hook on its sync thread right before the first step
/// of a run acts (after the list was made and the run began).
struct Rig {
    engine: Engine,
    hook: Arc<Mutex<Option<Hook>>>,
    fired: Arc<AtomicBool>,
}

impl Rig {
    fn open(data: &Path) -> Rig {
        let hook: Arc<Mutex<Option<Hook>>> = Arc::default();
        let fired = Arc::new(AtomicBool::new(false));
        let (slot, flag) = (hook.clone(), fired.clone());
        let emitter = Arc::new(move |event: EngineEvent| {
            if let EngineEvent::Progress { progress, .. } = &event {
                // The first step is about to act: nothing of this run has been carried yet.
                if progress.phase == Phase::Copying && progress.done_items == 0 && progress.current.is_some() {
                    let armed = slot.lock().unwrap().take();
                    if let Some(hook) = armed {
                        hook();
                        flag.store(true, Ordering::SeqCst);
                    }
                }
            }
        });
        let engine = Engine::open(data, emitter).expect("the engine opens");
        Rig { engine, hook, fired }
    }

    /// Runs `hook` on the sync thread right before the next run's first step acts.
    fn at_first_step(&self, hook: impl FnOnce() + Send + 'static) {
        self.fired.store(false, Ordering::SeqCst);
        *self.hook.lock().unwrap() = Some(Box::new(hook));
    }

    fn hook_fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

/// The engine sends live progress at most every 120 ms. A hook needs the first step's
/// progress, so the previous run's last progress must be older than that. (This only lets
/// the window pass; whether the hook ran is always asserted.)
fn let_progress_throttle_pass() {
    thread::sleep(Duration::from_millis(150));
}

fn plain_engine(data: &Path) -> Engine {
    Engine::open(data, Arc::new(|_: EngineEvent| {})).expect("the engine opens")
}

/// Opens the engine once the previous one is completely gone (its threads may still hold it
/// for a moment after the last handle is dropped).
fn reopen(data: &Path) -> Engine {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match Engine::open(data, Arc::new(|_: EngineEvent| {})) {
            Ok(engine) => return engine,
            Err(Error::AlreadyRunning) => {
                assert!(Instant::now() < deadline, "the previous engine never let go of the data folder");
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("the engine doesn't open: {e}"),
        }
    }
}

fn job_setup(name: &str, src: &Path, dst: &Path, mode: SyncMode) -> JobSetup {
    fs::create_dir_all(src).unwrap();
    fs::create_dir_all(dst).unwrap();
    JobSetup {
        name: name.into(),
        source: src.to_path_buf(),
        destination: dst.to_path_buf(),
        mode,
        conflict: ConflictRule::KeepBoth,
        trigger: Trigger::Manual,
        excludes: JobSetup::default_excludes(),
        safe_delete: true,
        full_automation: false,
    }
}

fn add(engine: &Engine, setup: JobSetup) -> String {
    engine.add_job(setup).expect("the job is added").job.id
}

fn view(engine: &Engine, id: &String) -> JobView {
    engine.job(id).expect("the job exists")
}

/// Waits until no sync of the job runs.
fn wait_idle(engine: &Engine, id: &String) {
    let deadline = Instant::now() + DEADLINE;
    while engine.is_running(id) {
        assert!(Instant::now() < deadline, "the sync took too long");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Polls `condition` until it holds; fails with `what` at the deadline.
fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The person looks at the list and confirms it as it is; waits for the sync to finish.
fn confirm(engine: &Engine, id: &String) -> Review {
    let review = engine.prepare_review(id).expect("the list is made");
    assert!(review.blocked.is_none(), "the list is blocked: {:?}", review.blocked);
    assert_eq!(engine.approve(id, &review.review_id, true).expect("the list is confirmed"), Approval::Started);
    wait_idle(engine, id);
    let run = view(engine, id).job.last_run.expect("the confirmed sync ran");
    assert!(run.approved && !run.cancelled && run.error.is_none(), "the confirmed sync failed: {run:?}");
    review
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Copies the folder `from` to `to` with everything in it, hidden folders too, as the
/// person would with the Finder or Explorer.
fn copy_folder(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for item in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_folder(&item.path(), &target);
        } else {
            fs::copy(item.path(), &target).unwrap();
        }
    }
}

/// Leaves a note in `folder` as a running sync of another Charon would: on the computer
/// `host`, or on this one. Returns the note's file.
fn another_charons_note(folder: &Path, host: Option<&str>) -> PathBuf {
    let mut note = BusyNote::new(&"elsewhere".to_string(), "Photos", "another-charon");
    if let Some(host) = host {
        note.host = host.into();
    }
    let file = folder.join(".charon").join(format!("another-charon{BUSY_EXTENSION}"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, serde_json::to_vec(&note).unwrap()).unwrap();
    file
}

/// A busy note's file and what it held.
type LeftNote = (PathBuf, Vec<u8>);

/// The busy notes in the Charon folder of `root`.
fn busy_notes(root: &Path) -> usize {
    fs::read_dir(root.join(".charon"))
        .map(|items| items.flatten().filter(|item| item.file_name().to_string_lossy().ends_with(BUSY_EXTENSION)).count())
        .unwrap_or(0)
}

/// Whether file permissions bite in this process (not root with CAP_DAC_OVERRIDE). If not,
/// the test says so where everyone sees it and skips, or fails when
/// CHARON_REQUIRE_PERMISSION_TESTS=1.
#[cfg(unix)]
fn permissions_bite(test: &str) -> bool {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("x");
    fs::create_dir(&probe).unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o555)).unwrap();
    let bites = fs::write(probe.join("y"), "y").is_err();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
    if bites {
        return true;
    }
    let how = "capsh --drop=cap_dac_override,cap_dac_read_search -- -c \"cargo test -p charon-core --test overlapping_jobs\"";
    let why = "this process can write into read-only folders (root with CAP_DAC_OVERRIDE), so the test would prove nothing";
    if std::env::var("CHARON_REQUIRE_PERMISSION_TESTS").is_ok_and(|v| v == "1") {
        panic!("{test} can't run: {why}. CHARON_REQUIRE_PERMISSION_TESTS=1 forbids skipping it. Run it with: {how}");
    }
    // Straight to stderr: the test harness swallows the output of a test that passes.
    let _ = writeln!(
        std::io::stderr(),
        "SKIPPED {test}: {why}. It did NOT run. Run it with: {how} (or set CHARON_REQUIRE_PERMISSION_TESTS=1 to fail instead of skipping)"
    );
    false
}

/// Whether the review warns, as a danger, about the job named `other`.
fn warns_about(review: &Review, other: &str) -> bool {
    review.warnings.iter().any(|warning| warning.level == Level::Danger && warning.text.contains(&format!("“{other}”")))
}

/// Whether the review mentions, as information only, a copy of a hidden .charon folder in
/// `folder`.
fn mentions_copy_in(review: &Review, folder: &Path) -> bool {
    let folder = folder.display().to_string();
    review.warnings.iter().any(|warning| warning.level == Level::Info && warning.text.contains(&folder) && warning.text.contains("copy"))
}

// ==================================================================== jobs that undo each other's work

#[test]
fn a_mirror_job_cannot_be_copied_onto_its_own_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let mirror = add(&engine, job_setup("Mirror", &tmp.path().join("x"), &tmp.path().join("d"), SyncMode::Mirror));
    let refused = engine.duplicate_job(&mirror).expect_err("the copy would share the destination");
    assert!(matches!(&refused, Error::Invalid(text) if text.contains("“Mirror”")), "{refused}");
    assert_eq!(engine.jobs().len(), 1);
}

#[test]
fn a_copy_of_a_job_cannot_be_edited_into_a_clash() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (x, d) = (tmp.path().join("x"), tmp.path().join("d"));
    add(&engine, job_setup("Mirror", &x, &d, SyncMode::Mirror));
    let shared = job_setup("Shared", &d.join("sub"), &tmp.path().join("y"), SyncMode::TwoWay);
    assert!(engine.add_job(shared.clone()).is_err(), "a new job can't overlap the Mirror's destination");

    // A copy of an unrelated two-way job, edited into the same folders, is refused too.
    let notes = add(&engine, job_setup("Notes", &tmp.path().join("p"), &tmp.path().join("q"), SyncMode::TwoWay));
    let copy = engine.duplicate_job(&notes).expect("a two-way job can be copied").job;
    let mut edited = shared;
    edited.name = copy.setup.name.clone();
    let refused = engine.update_job(&copy.id, edited).expect_err("the copy would overlap the Mirror's destination");
    assert!(refused.to_string().contains("“Mirror”"), "{refused}");
    // Its name can still change.
    let mut renamed = copy.setup.clone();
    renamed.name = "Notes on the laptop".into();
    engine.update_job(&copy.id, renamed).expect("a new name changes no folders");
}

#[test]
fn a_clash_saved_by_an_older_charon_is_shown_and_its_jobs_can_still_be_renamed() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (x, d, y) = (tmp.path().join("x"), tmp.path().join("d"), tmp.path().join("y"));
    write(&x.join("a.txt"), "a");
    write(&d.join("sub/b.txt"), "b");
    fs::create_dir_all(&y).unwrap();
    // Charon 0.2 never checked folders against each other's jobs.
    let job = |id: &str, name: &str, source: &Path, destination: &Path, mode: &str| {
        serde_json::json!({
            "id": id, "name": name, "source": source, "destination": destination, "mode": mode,
            "conflict": "ask", "trigger": {"kind": "manual"}, "excludes": [], "safeDelete": true,
            "paused": false, "createdAt": "2026-10-01T10:00:00Z", "lastRun": null, "lastSynced": null
        })
    };
    let old = serde_json::json!({
        "version": 1,
        "settings": {"trashDays": 7},
        "jobs": [job("mirror", "Mirror", &x, &d, "mirror"), job("shared", "Shared", &d.join("sub"), &y, "two-way")],
    });
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("charon.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    let engine = plain_engine(&data);
    let (mirror, shared) = ("mirror".to_string(), "shared".to_string());

    // Every look says so, as a danger that needs the tick.
    let review = engine.prepare_review(&shared).unwrap();
    assert!(warns_about(&review, "Mirror"), "{:?}", review.warnings);
    assert!(review.needs_tick);
    let refused = engine.approve(&shared, &review.review_id, false);
    assert!(matches!(refused, Err(Error::Invalid(_))), "not without the tick");
    assert!(!y.join("b.txt").exists());
    let review = engine.prepare_review(&mirror).unwrap();
    assert!(warns_about(&review, "Shared"), "{:?}", review.warnings);
    assert!(review.needs_tick);

    // A new name or another schedule is fine; other overlapping folders are not.
    let mut renamed = view(&engine, &shared).job.setup;
    renamed.name = "Shared with the laptop".into();
    renamed.trigger = Trigger::Interval { minutes: 60 };
    engine.update_job(&shared, renamed.clone()).expect("the folders stay as they were");
    let mut moved = renamed;
    moved.source = d.join("other");
    assert!(engine.update_job(&shared, moved).is_err());
}

#[cfg(unix)]
#[test]
fn a_link_made_after_the_job_was_saved_cannot_turn_it_into_a_sync_into_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let dropbox = tmp.path().join("CloudStorage/Dropbox");
    write(&dropbox.join("photo.jpg"), "photo");
    // Saved while "Dropbox" was nothing yet: the paths don't overlap.
    let mut setup = job_setup("Dropbox backup", &dropbox, &tmp.path().join("elsewhere"), SyncMode::Mirror);
    setup.destination = tmp.path().join("Dropbox/Backup");
    let id = add(&engine, setup);
    // Then "Dropbox" became a link to the folder the job backs up.
    std::os::unix::fs::symlink(&dropbox, tmp.path().join("Dropbox")).unwrap();
    fs::create_dir_all(dropbox.join("Backup")).unwrap();

    let review = engine.prepare_review(&id).unwrap();
    let blocked = review.blocked.clone().expect("the list can't be confirmed");
    assert!(blocked.contains("inside the other"), "{blocked}");
    assert!(engine.approve(&id, &review.review_id, true).is_err());
    assert_eq!(fs::read_dir(dropbox.join("Backup")).unwrap().count(), 0, "nothing was copied into itself");
}

#[cfg(unix)]
#[test]
fn a_link_made_after_two_jobs_were_saved_cannot_let_them_undo_each_other_unnoticed() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (nas, nas_again) = (tmp.path().join("NAS"), tmp.path().join("NAS-1"));
    write(&tmp.path().join("laptop/report.txt"), "report");
    let docs = add(&engine, job_setup("Docs", &tmp.path().join("laptop"), &nas.join("Docs"), SyncMode::TwoWay));
    confirm(&engine, &docs);
    write(&tmp.path().join("work/plan.txt"), "plan");
    let work = add(&engine, job_setup("Work", &tmp.path().join("work"), &nas_again.join("Docs/Work"), SyncMode::Mirror));
    eventually("the first look", || !view(&engine, &work).looking);
    // Then NAS-1 turns out to be the share under another name, where Work's folder is too.
    fs::remove_dir_all(&nas_again).unwrap();
    std::os::unix::fs::symlink(&nas, &nas_again).unwrap();
    fs::create_dir_all(nas.join("Docs/Work")).unwrap();

    // The Mirror would remove what "Docs" brings in, and "Docs" would carry the removals back.
    let review = engine.prepare_review(&work).unwrap();
    assert!(warns_about(&review, "Docs"), "{:?}", review.warnings);
    assert!(review.needs_tick);
    assert!(engine.approve(&work, &review.review_id, false).is_err(), "not without the tick");
    let review = engine.prepare_review(&docs).unwrap();
    assert!(warns_about(&review, "Work"), "{:?}", review.warnings);

    // A Backup into the same place only adds: the two don't undo each other.
    let inbox = add(&engine, job_setup("Inbox", &tmp.path().join("scans"), &nas_again.join("Docs/Inbox"), SyncMode::Backup));
    let review = engine.prepare_review(&inbox).unwrap();
    assert!(!warns_about(&review, "Docs"), "{:?}", review.warnings);
}

#[cfg(unix)]
#[test]
fn a_conflict_cannot_be_settled_while_charon_cannot_tell_whether_the_folders_are_one() {
    use charon_core::ConflictChoice;
    use std::os::unix::fs::PermissionsExt;
    if !permissions_bite("a_conflict_cannot_be_settled_while_charon_cannot_tell_whether_the_folders_are_one") {
        return;
    }
    let chmod = |folder: &Path, mode: u32| fs::set_permissions(folder, fs::Permissions::from_mode(mode)).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (laptop, nas) = (tmp.path().join("Docs"), tmp.path().join("NAS/Docs"));
    write(&laptop.join("report.txt"), "report");
    let mut setup = job_setup("Docs", &laptop, &nas, SyncMode::TwoWay);
    setup.conflict = ConflictRule::Ask;
    let docs = add(&engine, setup);
    confirm(&engine, &docs);
    write(&laptop.join("report.txt"), "edited on the laptop");
    write(&nas.join("report.txt"), "edited on the share, longer");
    let review = engine.prepare_review(&docs).unwrap();
    engine.discard_review(&docs, &review.review_id);
    let conflict = view(&engine, &docs).conflicts.first().cloned().expect("a conflict");

    // The share's marker turns up inside the laptop's folder, and neither Charon folder takes
    // a file, so Charon can't tell a copy from the share reached under a second name.
    let found = laptop.join("NAS copy");
    copy_folder(&nas.join(".charon"), &found.join(".charon"));
    for folder in [nas.join(".charon"), found.join(".charon")] {
        chmod(&folder, 0o555);
    }
    let review = engine.prepare_review(&docs).unwrap();
    assert!(review.blocked.as_deref().is_some_and(|text| text.contains("couldn't tell")), "{:?}", review.blocked);
    engine.discard_review(&docs, &review.review_id);

    // Settling the conflict is refused just the same.
    let settle = engine.prepare_resolution(&docs, &conflict, ConflictChoice::KeepSource).unwrap();
    assert!(settle.blocked.as_deref().is_some_and(|text| text.contains("couldn't tell")), "{:?}", settle.blocked);
    assert!(engine.approve(&docs, &settle.review_id, true).is_err());
    assert_eq!(fs::read_to_string(nas.join("report.txt")).unwrap(), "edited on the share, longer", "nothing changed");
    for folder in [nas.join(".charon"), found.join(".charon")] {
        chmod(&folder, 0o755);
    }
}

// ==================================================================== copies of Charon's hidden folder

#[test]
fn a_backup_restored_by_copying_it_into_the_source_blocks_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (documents, usb) = (tmp.path().join("Documents"), tmp.path().join("USB/Documents"));
    write(&documents.join("letter.txt"), "letter");
    let backup = add(&engine, job_setup("Backup", &documents, &usb, SyncMode::Backup));
    confirm(&engine, &backup);

    // The person restores files by dragging the backup into the source: the hidden .charon
    // folder comes along, with the job's marker in it.
    let restored = documents.join("From the USB backup");
    copy_folder(&usb, &restored);
    assert!(restored.join(".charon").join(format!("{backup}.json")).exists());

    let review = confirm(&engine, &backup);
    assert!(review.warnings.iter().all(|warning| warning.level != Level::Danger), "{:?}", review.warnings);
    assert!(mentions_copy_in(&review, &restored), "{:?}", review.warnings);
    assert!(usb.join("From the USB backup/letter.txt").exists());
    assert!(!usb.join("From the USB backup/.charon").exists(), "the copy itself is never synced");
}

#[test]
fn a_copy_of_one_side_of_a_two_way_job_inside_the_other_blocks_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (laptop, nas) = (tmp.path().join("Docs"), tmp.path().join("NAS/Docs"));
    write(&laptop.join("report.txt"), "report");
    let docs = add(&engine, job_setup("Docs", &laptop, &nas, SyncMode::TwoWay));
    confirm(&engine, &docs);
    // A copy of the share's side, kept before a move.
    let kept = laptop.join("NAS copy");
    copy_folder(&nas, &kept);

    let review = confirm(&engine, &docs);
    assert!(mentions_copy_in(&review, &kept), "{:?}", review.warnings);
    assert!(nas.join("NAS copy/report.txt").exists());
}

#[test]
fn a_copy_of_a_mirrors_backup_inside_a_two_way_job_raises_no_alarm() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let usb = tmp.path().join("USB");
    write(&tmp.path().join("Photos/beach.jpg"), "beach");
    let photos = add(&engine, job_setup("Photos", &tmp.path().join("Photos"), &usb.join("Photos"), SyncMode::Mirror));
    confirm(&engine, &photos);
    let (laptop, nas) = (tmp.path().join("Docs"), tmp.path().join("NAS/Docs"));
    write(&laptop.join("report.txt"), "report");
    let mut setup = job_setup("Docs", &laptop, &nas, SyncMode::TwoWay);
    setup.full_automation = true;
    let docs = add(&engine, setup);
    confirm(&engine, &docs);

    // The person copies the Mirror's backup into the two-way job's folder.
    let old_photos = laptop.join("Old photos");
    copy_folder(&usb.join("Photos"), &old_photos);
    let review = engine.prepare_review(&docs).unwrap();
    assert!(!warns_about(&review, "Photos"), "{:?}", review.warnings);
    assert!(!review.needs_tick && review.blocked.is_none());
    assert!(mentions_copy_in(&review, &old_photos), "{:?}", review.warnings);
    engine.discard_review(&docs, &review.review_id);

    // Nothing holds back its automatic sync, with the backup drive plugged in or not.
    fs::rename(&usb, tmp.path().join("USB unplugged")).unwrap();
    let review = engine.prepare_review(&docs).unwrap();
    assert!(!warns_about(&review, "Photos") && !mentions_copy_in(&review, &old_photos), "{:?}", review.warnings);
    engine.discard_review(&docs, &review.review_id);
    assert_eq!(engine.sync_now(&docs).unwrap(), SyncStart::Started);
    wait_idle(&engine, &docs);
    let row = view(&engine, &docs);
    assert!(row.job.held.is_none(), "held: {:?}", row.job.held);
    assert!(nas.join("Old photos/beach.jpg").exists());
}

// ==================================================================== another sync at work in the folders

#[test]
fn a_confirmed_sync_waits_for_another_charon_and_starts_once_it_is_done() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let id = add(&engine, job_setup("Photos", &src, &dst, SyncMode::Backup));
    let review = engine.prepare_review(&id).unwrap();

    // Another computer starts syncing the destination before the person confirms.
    let note = another_charons_note(&dst, Some("studio-mac"));
    let approval = engine.approve(&id, &review.review_id, true).expect("the confirmation is kept");
    assert_eq!(approval, Approval::Waiting { waiting_for: "Charon on studio-mac".into() }, "the window says what it waits for");
    let row = view(&engine, &id);
    assert!(row.confirmed && !engine.is_running(&id));
    assert_eq!(row.waiting_for.as_deref(), Some("Charon on studio-mac"));
    assert!(row.waiting.is_none() && row.pending.is_some(), "it no longer waits for an OK, only for the other sync");
    assert_eq!(row.status, JobStatus::Pending, "never “New” or “Review” once confirmed");
    assert_eq!(engine.overview().waiting, 0);
    assert!(!engine.start_confirmed(&id), "still busy");
    assert!(!dst.join("a.txt").exists());

    // Meanwhile something new turns up; it isn't in the confirmed list.
    write(&src.join("b.txt"), "b");
    assert!(engine.note_change(&id, &src.join("b.txt")));

    fs::remove_file(note).unwrap();
    assert!(engine.start_confirmed(&id));
    wait_idle(&engine, &id);
    let row = view(&engine, &id);
    let run = row.job.last_run.clone().expect("the confirmed sync ran");
    assert!(run.approved && run.error.is_none(), "{run:?}");
    assert!(dst.join("a.txt").exists());
    assert!(!dst.join("b.txt").exists(), "only what the person confirmed");
    assert!(!row.confirmed && row.waiting_for.is_none());
    assert!(engine.log(&id, 20).iter().any(|line| line.text.contains("after waiting for another sync")));
}

#[test]
fn a_waiting_confirmed_sync_is_called_off_by_stop_or_by_closing_its_list() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let id = add(&engine, job_setup("Photos", &src, &dst, SyncMode::Backup));

    let review = engine.prepare_review(&id).unwrap();
    let note = another_charons_note(&src, None);
    engine.approve(&id, &review.review_id, true).unwrap();
    assert_eq!(view(&engine, &id).waiting_for.as_deref(), Some("another Charon"));
    engine.cancel(&id);
    assert!(!view(&engine, &id).confirmed);
    fs::remove_file(&note).unwrap();
    assert!(!engine.start_confirmed(&id));

    let review = engine.prepare_review(&id).unwrap();
    let note = another_charons_note(&src, None);
    engine.approve(&id, &review.review_id, true).unwrap();
    assert!(view(&engine, &id).confirmed);
    engine.discard_review(&id, &review.review_id);
    assert!(!view(&engine, &id).confirmed);
    fs::remove_file(&note).unwrap();
    assert!(!engine.start_confirmed(&id));
    assert!(!engine.is_running(&id) && !dst.join("a.txt").exists(), "nothing ran");
}

#[test]
fn pausing_everything_calls_off_a_waiting_confirmed_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let id = add(&engine, job_setup("Photos", &src, &dst, SyncMode::Backup));

    let review = engine.prepare_review(&id).unwrap();
    let note = another_charons_note(&dst, None);
    assert!(matches!(engine.approve(&id, &review.review_id, true).unwrap(), Approval::Waiting { .. }));
    engine.set_all_paused(true).unwrap();
    let row = view(&engine, &id);
    assert!(!row.confirmed && row.waiting_for.is_none());
    assert!(engine.log(&id, 20).iter().any(|line| line.text.contains("called off (Pause everything)")));

    // The other sync ends, and everything is resumed: nothing starts by itself.
    fs::remove_file(&note).unwrap();
    engine.set_all_paused(false).unwrap();
    assert!(!engine.start_confirmed(&id));
    assert!(!engine.is_running(&id) && !dst.join("a.txt").exists(), "nothing ran");

    // Confirmed again, the list syncs, paused or not: the person asked for it.
    engine.set_all_paused(true).unwrap();
    assert_eq!(engine.approve(&id, &review.review_id, true).unwrap(), Approval::Started);
    wait_idle(&engine, &id);
    assert!(dst.join("a.txt").exists());
}

#[test]
fn a_sync_by_itself_waits_while_another_charon_is_at_work() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let mut setup = job_setup("Photos", &src, &dst, SyncMode::Backup);
    setup.full_automation = true;
    let id = add(&engine, setup);
    confirm(&engine, &id);

    write(&src.join("b.txt"), "b");
    let note = another_charons_note(&dst, None);
    assert!(engine.sync_all().is_empty());
    assert!(!engine.is_running(&id));
    assert_eq!(view(&engine, &id).waiting_for.as_deref(), Some("another Charon"));
    assert!(!dst.join("b.txt").exists());

    fs::remove_file(note).unwrap();
    engine.start_wanted(&id);
    wait_idle(&engine, &id);
    assert!(dst.join("b.txt").exists());
    assert!(view(&engine, &id).job.last_run.is_some_and(|run| !run.approved && run.error.is_none()));
}

#[test]
fn another_charons_sync_in_a_folder_inside_makes_the_job_wait() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = plain_engine(&tmp.path().join("data"));
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("Work/plan.txt"), "plan");
    let id = add(&engine, job_setup("Everything", &src, &dst, SyncMode::Backup));
    eventually("the first look", || !view(&engine, &id).looking);
    // Another Charon syncs src/Work (reached on its computer under another name). It began
    // after the first look, so only the list's look meets it.
    another_charons_note(&src.join("Work"), Some("studio-mac"));
    let review = engine.prepare_review(&id).unwrap();
    engine.approve(&id, &review.review_id, true).unwrap();
    assert!(view(&engine, &id).confirmed);
    assert_eq!(view(&engine, &id).waiting_for.as_deref(), Some("Charon on studio-mac"));
    assert!(!dst.join("Work").exists());
}

#[test]
fn notes_left_by_a_sync_cut_short_count_for_nothing_once_charon_starts_again() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let rig = Rig::open(&data);
    let id = add(&rig.engine, job_setup("Photos", &src, &dst, SyncMode::Backup));
    // What a crash in the middle of the sync leaves: its busy notes, as they are then.
    let left: Arc<Mutex<Vec<LeftNote>>> = Arc::default();
    let (kept, folder) = (left.clone(), dst.clone());
    rig.at_first_step(move || {
        for item in fs::read_dir(folder.join(".charon")).unwrap().flatten() {
            if item.file_name().to_string_lossy().ends_with(BUSY_EXTENSION) {
                kept.lock().unwrap().push((item.path(), fs::read(item.path()).unwrap()));
            }
        }
    });
    let_progress_throttle_pass();
    confirm(&rig.engine, &id);
    assert!(rig.hook_fired());
    drop(rig);
    let left = left.lock().unwrap().clone();
    assert_eq!(left.len(), 1);
    for (file, bytes) in &left {
        fs::write(file, bytes).unwrap();
    }

    // Charon starts again: the notes are its own, of a sync that's long over.
    let engine = reopen(&data);
    assert!(DataFolder::new(&data).instances().exists());
    write(&src.join("b.txt"), "b");
    confirm(&engine, &id);
    assert!(dst.join("b.txt").exists());
    assert!(view(&engine, &id).waiting_for.is_none());
    assert_eq!(busy_notes(&dst), 0, "tidied away");
}

#[test]
fn a_confirmed_sync_waits_for_its_neighbour_and_starts_by_itself_when_it_is_done() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let engine = &rig.engine;
    let (s, t, u) = (tmp.path().join("s"), tmp.path().join("t"), tmp.path().join("u"));
    write(&s.join("a.txt"), "a");
    write(&t.join("t.txt"), "t");
    // A chain of backups: allowed, and the two never run at once.
    let first = add(engine, job_setup("First", &s, &t, SyncMode::Backup));
    let second = add(engine, job_setup("Second", &t, &u, SyncMode::Backup));
    let second_list = engine.prepare_review(&second).unwrap();

    let (release, released) = mpsc::channel::<()>();
    let (arrived, at_first_step) = mpsc::channel::<()>();
    rig.at_first_step(move || {
        arrived.send(()).unwrap();
        let _ = released.recv_timeout(DEADLINE);
    });
    let_progress_throttle_pass();
    let first_list = engine.prepare_review(&first).unwrap();
    engine.approve(&first, &first_list.review_id, true).unwrap();
    at_first_step.recv_timeout(DEADLINE).expect("the first job's sync reached its first step");
    assert!(busy_notes(&s) == 1 && busy_notes(&t) == 1, "the running sync's notes are in its folders");

    // While "First" syncs, the person confirms the list of "Second": it waits.
    let approval = engine.approve(&second, &second_list.review_id, true).expect("the confirmation is kept");
    assert_eq!(approval, Approval::Waiting { waiting_for: "First".into() });
    let row = view(engine, &second);
    assert!(row.confirmed && !engine.is_running(&second));
    assert_eq!(row.waiting_for.as_deref(), Some("First"));

    release.send(()).unwrap();
    eventually("the second job synced by itself", || {
        !engine.is_running(&first) && !engine.is_running(&second) && view(engine, &second).job.last_run.is_some_and(|run| run.approved)
    });
    assert!(rig.hook_fired());
    assert!(t.join("a.txt").exists());
    assert!(u.join("t.txt").exists());
    assert!(!u.join("a.txt").exists(), "only what the person confirmed");
    assert!(!view(engine, &second).confirmed);
    assert_eq!(busy_notes(&s) + busy_notes(&t) + busy_notes(&u), 0, "every note was taken away");
}

// ==================================================================== what happens during a sync

#[test]
fn changes_made_during_a_sync_have_the_folders_looked_at_once_it_is_done() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let engine = &rig.engine;
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let id = add(engine, job_setup("Photos", &src, &dst, SyncMode::Backup));

    // During a sync, its own items changing tell nothing; another item changing does.
    let heard: Arc<Mutex<Vec<bool>>> = Arc::default();
    let (during, (watcher, folder, job)) = (heard.clone(), (engine.clone(), src.clone(), id.clone()));
    rig.at_first_step(move || {
        write(&folder.join("later.txt"), "made while syncing");
        let mut heard = during.lock().unwrap();
        heard.push(watcher.note_change_while_syncing(&job, &folder.join("a.txt")));
        heard.push(watcher.note_change_while_syncing(&job, &folder.join("later.txt")));
    });
    let_progress_throttle_pass();
    confirm(engine, &id);
    assert!(rig.hook_fired());
    assert_eq!(*heard.lock().unwrap(), vec![false, false], "kept for the sync's end");
    assert!(engine.check_due(&id, 15), "the folders are looked at again at once");

    // Right after it, the sync's own items are still told apart; anything else counts.
    assert!(!engine.note_change_while_syncing(&id, &src.join("a.txt")));
    assert!(engine.note_change_while_syncing(&id, &src.join("later.txt")));

    // A sync during which nothing else changed waits for the regular look.
    write(&src.join("b.txt"), "b");
    let (watcher, folder, job) = (engine.clone(), src.clone(), id.clone());
    rig.at_first_step(move || {
        watcher.note_change_while_syncing(&job, &folder.join("b.txt"));
    });
    let_progress_throttle_pass();
    confirm(engine, &id);
    assert!(rig.hook_fired());
    assert!(!engine.check_due(&id, 15));
}

// ==================================================================== Charon itself can't work as it should

#[test]
fn an_older_charon_saving_its_jobs_stops_every_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let engine = plain_engine(&data);
    let id = add(&engine, job_setup("Photos", &src, &dst, SyncMode::Backup));
    assert!(!engine.older_charon_running());
    assert!(engine.overview().problem.is_none());

    // Charon 0.2, still running, saves its jobs where it always did.
    write(&data.join("charon.json"), "{\"version\":1,\"jobs\":[]}");
    assert!(engine.older_charon_running());
    let problem = engine.overview().problem.expect("the header says so");
    assert!(problem.contains("older version of Charon"), "{problem}");
    let review = engine.prepare_review(&id).expect("looking is still fine");
    let refused = engine.approve(&id, &review.review_id, true).expect_err("nothing syncs");
    assert!(refused.to_string().contains("older version of Charon"), "{refused}");
    assert!(!dst.join("a.txt").exists());

    // A file the older Charon left behind earlier counts only once it changes again.
    drop(engine);
    let engine = reopen(&data);
    assert!(!engine.older_charon_running());
    let later = SystemTime::now() + Duration::from_secs(5);
    filetime::set_file_mtime(data.join("charon.json"), filetime::FileTime::from_system_time(later)).unwrap();
    assert!(engine.older_charon_running());
}

#[test]
fn while_charon_cannot_save_nothing_syncs_by_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("a.txt"), "a");
    let engine = plain_engine(&data);
    let mut setup = job_setup("Photos", &src, &dst, SyncMode::Backup);
    setup.full_automation = true;
    let id = add(&engine, setup);
    confirm(&engine, &id);

    // The state file can't be replaced anymore (a full disk would do the same).
    let state = data.join("charon-state.json");
    fs::remove_file(&state).unwrap();
    write(&state.join("in the way"), "x");
    assert!(engine.set_paused(&id, true).is_err());
    let problem = engine.overview().problem.expect("the header says so");
    assert!(problem.contains("can't save"), "{problem}");
    write(&src.join("b.txt"), "b");
    let refused = engine.sync_now(&id).expect_err("no sync by itself");
    assert!(refused.to_string().contains("can't save"), "{refused}");
    assert!(!dst.join("b.txt").exists());

    // Once a save works again, so does everything else.
    fs::remove_dir_all(&state).unwrap();
    engine.set_paused(&id, false).unwrap();
    assert!(engine.overview().problem.is_none());
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Started);
    wait_idle(&engine, &id);
    assert!(dst.join("b.txt").exists());
}
