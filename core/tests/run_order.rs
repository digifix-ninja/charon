//! The order a run carries out its steps in, and what it holds back: nothing may leave an
//! item on one side only while the run is under way, whether it is stopped, a copy fails,
//! or a file changes while the others are carried.
//!
//! An item that has to make room for one of another kind (a folder renamed, with a file
//! taking its old name) goes after every other copy, right before what takes its place, and
//! into .charon-trash even with safe deletes off. A copy that fails keeps every removal on
//! its side for the next sync. One left for the next sync because its file changed keeps
//! the removals there that could be its old copy, and only those: a file an app keeps
//! writing to never stops the rest of a job's removals. An app library the run removes
//! something of is carried whole after every other copy, and before anything else is
//! removed: what it removes waits like any removal, and what it brings lands before its
//! other copy goes.

use charon_core::model::{Review, StepFilter};
use charon_core::{ConflictRule, Engine, EngineEvent, JobSetup, JobView, Phase, SyncMode, Trigger};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long any wait may take before the test fails.
const DEADLINE: Duration = Duration::from_secs(60);

/// The photos of the folder the tests rename.
const PHOTOS: [&str; 4] = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"];

type Hook = Box<dyn FnOnce() + Send>;

/// An engine that can run a one-shot hook on its sync thread right before the first step of
/// a run acts (after the list was made and the run began).
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

/// A job with safe deletes off: what it removes is gone for good, unless it makes room.
fn job_setup(src: &Path, dst: &Path, mode: SyncMode) -> JobSetup {
    fs::create_dir_all(src).unwrap();
    fs::create_dir_all(dst).unwrap();
    JobSetup {
        name: String::new(),
        source: src.to_path_buf(),
        destination: dst.to_path_buf(),
        mode,
        conflict: ConflictRule::Ask,
        trigger: Trigger::Manual,
        excludes: JobSetup::default_excludes(),
        safe_delete: false,
        full_automation: false,
    }
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

/// The person looks at the list and confirms it as it is; waits for the sync to finish.
fn confirm(engine: &Engine, id: &String) -> Review {
    let review = engine.prepare_review(id).expect("the list is made");
    assert!(review.blocked.is_none(), "the list is blocked: {:?}", review.blocked);
    engine.approve(id, &review.review_id, true).expect("the list is confirmed");
    wait_idle(engine, id);
    review
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{} can't be read: {e}", path.display()))
}

/// Every file under `root`.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(items) = fs::read_dir(root) {
        for item in items.flatten() {
            let path = item.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// The contents of every file called `name` in `root`'s .charon-trash.
fn trashed(root: &Path, name: &str) -> Vec<String> {
    walk(&root.join(".charon-trash")).iter().filter(|path| path.file_name().is_some_and(|n| n == name)).map(|path| read(path)).collect()
}

/// The contents of a photo. Each photo has its own size, so none looks like another.
fn photo(name: &str) -> String {
    let place = PHOTOS.iter().position(|photo| *photo == name).expect("one of the photos");
    format!("the photo {name}").repeat(place + 1)
}

/// A job whose folders hold the same `Photos` folder after its first confirmed sync, then
/// `Photos` renamed to `Photos 2025` in the source. Returns the folders and the job.
fn renamed_photos(rig: &Rig, root: &Path, mode: SyncMode) -> (PathBuf, PathBuf, String) {
    let (src, dst) = (root.join("src"), root.join("dst"));
    for name in PHOTOS {
        write(&src.join("Photos").join(name), &photo(name));
    }
    let id = rig.engine.add_job(job_setup(&src, &dst, mode)).expect("the job is added").job.id;
    confirm(&rig.engine, &id);
    assert_eq!(read(&dst.join("Photos/d.jpg")), photo("d.jpg"));
    fs::rename(src.join("Photos"), src.join("Photos 2025")).unwrap();
    (src, dst, id)
}

/// Each photo is still somewhere in `dst`: in its old folder, its new one, or the trash.
fn assert_every_photo_kept(dst: &Path) {
    for name in PHOTOS {
        let kept = [dst.join("Photos").join(name), dst.join("Photos 2025").join(name)]
            .iter()
            .any(|path| path.is_file() && read(path) == photo(name))
            || trashed(dst, name).contains(&photo(name));
        assert!(kept, "the destination lost {name} while the source still has it");
    }
}

/// A folder renamed in the source, with a file taking its old name, and the person stops
/// the sync right as it starts: whatever ran first, every photo is still in the
/// destination.
fn a_folder_that_became_a_file_keeps_its_photos_when_the_run_stops(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst, id) = renamed_photos(&rig, tmp.path(), mode);
    write(&src.join("Photos"), "a note: moved to Photos 2025");
    let review = rig.engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.destination.removed, 4, "{:?}", review.tally);
    let (engine, stopped) = (rig.engine.clone(), id.clone());
    rig.at_first_step(move || engine.cancel(&stopped));
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    assert!(view(&rig.engine, &id).job.last_run.unwrap().cancelled);
    assert_every_photo_kept(&dst);
    assert!(dst.join("Photos").is_dir(), "the old folder goes only after every other copy");
}

#[test]
fn two_way_keeps_a_folder_that_became_a_file_when_the_run_stops() {
    a_folder_that_became_a_file_keeps_its_photos_when_the_run_stops(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_a_folder_that_became_a_file_when_the_run_stops() {
    a_folder_that_became_a_file_keeps_its_photos_when_the_run_stops(SyncMode::Mirror);
}

/// The same, but the copies into the new folder fail (a file took its name on the
/// destination meanwhile): the old folder, which only makes room, stays as it is.
fn a_folder_that_became_a_file_stays_when_copies_fail(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst, id) = renamed_photos(&rig, tmp.path(), mode);
    write(&src.join("Photos"), "a note: moved to Photos 2025");
    let review = rig.engine.prepare_review(&id).unwrap();
    let in_the_way = dst.join("Photos 2025");
    rig.at_first_step(move || write(&in_the_way, "a file where the new folder goes"));
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    assert!(view(&rig.engine, &id).job.last_run.unwrap().failed > 0);
    for name in PHOTOS {
        assert_eq!(read(&dst.join("Photos").join(name)), photo(name), "the old folder keeps {name}");
    }
}

#[test]
fn two_way_keeps_a_folder_that_became_a_file_when_copies_fail() {
    a_folder_that_became_a_file_stays_when_copies_fail(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_a_folder_that_became_a_file_when_copies_fail() {
    a_folder_that_became_a_file_stays_when_copies_fail(SyncMode::Mirror);
}

/// With nothing in the way, the file takes the folder's place, and the folder's items go
/// into .charon-trash although safe deletes are off: until the file landed, they were the
/// destination's only copy of what was there. The list says so.
fn what_makes_room_goes_to_the_trash_with_safe_deletes_off(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst, id) = renamed_photos(&rig, tmp.path(), mode);
    write(&src.join("Photos"), "a note: moved to Photos 2025");
    let review = rig.engine.prepare_review(&id).unwrap();
    let removals = rig.engine.review_steps(&id, &review.review_id, StepFilter::Removals, "", 0, 50).unwrap();
    assert_eq!(removals.total, 5, "four photos and their folder");
    for step in removals.steps.iter().filter(|step| step.path != "Photos") {
        assert!(step.reason.ends_with("goes to .charon-trash"), "{}: {}", step.path, step.reason);
    }
    assert!(!review.warnings.iter().any(|w| w.text.contains("Safe deletes are off")), "{:?}", review.warnings);
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert_eq!(view(&rig.engine, &id).job.last_run.unwrap().error, None);
    assert_eq!(read(&dst.join("Photos")), "a note: moved to Photos 2025");
    for name in PHOTOS {
        assert_eq!(read(&dst.join("Photos 2025").join(name)), photo(name));
        assert_eq!(trashed(&dst, name), vec![photo(name)], "{name} went into the trash");
    }
}

#[test]
fn two_way_keeps_what_makes_room_in_the_trash_with_safe_deletes_off() {
    what_makes_room_goes_to_the_trash_with_safe_deletes_off(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_what_makes_room_in_the_trash_with_safe_deletes_off() {
    what_makes_room_goes_to_the_trash_with_safe_deletes_off(SyncMode::Mirror);
}

/// A plain folder rename, and an app writes to one photo in the new folder while the run
/// carries the others: its copy waits for the next sync, and so does the removal of its old
/// copy, the destination's only one. The old copies of the photos that arrived go.
fn a_photo_edited_during_the_run_keeps_its_old_copy(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst, id) = renamed_photos(&rig, tmp.path(), mode);
    let review = rig.engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.destination.removed, 4);
    let edited = src.join("Photos 2025/d.jpg");
    rig.at_first_step(move || fs::OpenOptions::new().append(true).open(&edited).unwrap().write_all(b", with new tags").unwrap());
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    let run = view(&rig.engine, &id).job.last_run.unwrap();
    assert_eq!((run.copied, run.deleted), (3, 3), "{}", run.describe());
    assert_eq!(read(&dst.join("Photos/d.jpg")), photo("d.jpg"), "the old copy waits for the new one");
    assert!(!dst.join("Photos/a.jpg").exists(), "a.jpg arrived in its new folder, so its old copy went");

    // The next sync carries the edited photo, then removes the old folder.
    confirm(&rig.engine, &id);
    assert_eq!(read(&dst.join("Photos 2025/d.jpg")), format!("{}, with new tags", photo("d.jpg")));
    assert!(!dst.join("Photos").exists());
}

#[test]
fn two_way_keeps_the_old_copy_of_a_photo_edited_during_the_run() {
    a_photo_edited_during_the_run_keeps_its_old_copy(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_the_old_copy_of_a_photo_edited_during_the_run() {
    a_photo_edited_during_the_run_keeps_its_old_copy(SyncMode::Mirror);
}

/// A plain folder rename, and something new takes one photo's new place on the destination
/// while the run goes on: that copy is left for the next sync, and the removal of the
/// photo's old copy waits too, while the others go.
fn a_photo_whose_new_place_was_taken_keeps_its_old_copy(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (_src, dst, id) = renamed_photos(&rig, tmp.path(), mode);
    let review = rig.engine.prepare_review(&id).unwrap();
    let taken = dst.join("Photos 2025/d.jpg");
    rig.at_first_step(move || write(&taken, "saved by an app on the destination"));
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    assert_eq!(view(&rig.engine, &id).job.last_run.unwrap().deleted, 3);
    assert_eq!(read(&dst.join("Photos/d.jpg")), photo("d.jpg"), "the old copy waits for the new one");
    assert!(!dst.join("Photos/c.jpg").exists());
    assert_eq!(read(&dst.join("Photos 2025/d.jpg")), "saved by an app on the destination");
}

#[test]
fn two_way_keeps_the_old_copy_of_a_photo_whose_new_place_was_taken() {
    a_photo_whose_new_place_was_taken_keeps_its_old_copy(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_the_old_copy_of_a_photo_whose_new_place_was_taken() {
    a_photo_whose_new_place_was_taken_keeps_its_old_copy(SyncMode::Mirror);
}

/// An app keeps writing to a log while the person cleans up an old project: the log's copy
/// waits for the next sync on every run, and the project still goes. Nothing that was
/// removed could be an old copy of the log.
fn a_busy_file_doesnt_hold_back_other_removals(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
    write(&src.join("app/activity.log"), "started\n");
    write(&src.join("old-project/big.mov"), "a long film nobody needs anymore");
    let id = rig.engine.add_job(job_setup(&src, &dst, mode)).expect("the job is added").job.id;
    confirm(&rig.engine, &id);
    assert!(dst.join("old-project/big.mov").is_file());

    fs::remove_dir_all(src.join("old-project")).unwrap();
    let log = src.join("app/activity.log");
    fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(b"saved\n").unwrap();
    let review = rig.engine.prepare_review(&id).unwrap();
    assert_eq!((review.tally.destination.replaced, review.tally.destination.removed), (1, 1), "{:?}", review.tally);
    rig.at_first_step(move || fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(b"saved again\n").unwrap());
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    let run = view(&rig.engine, &id).job.last_run.unwrap();
    assert_eq!((run.copied, run.skipped), (0, 1), "{}", run.describe());
    assert!(!dst.join("old-project").exists(), "the old project went although the log was busy");
    assert_eq!(read(&dst.join("app/activity.log")), "started\n");
}

#[test]
fn two_way_removes_what_a_busy_file_couldnt_be_an_old_copy_of() {
    a_busy_file_doesnt_hold_back_other_removals(SyncMode::TwoWay);
}

#[test]
fn mirror_removes_what_a_busy_file_couldnt_be_an_old_copy_of() {
    a_busy_file_doesnt_hold_back_other_removals(SyncMode::Mirror);
}

/// A job whose folders hold the same Logic project, with its arrangement and a take, after
/// its first confirmed sync. Returns the folders and the job.
fn a_logic_project(rig: &Rig, root: &Path, mode: SyncMode) -> (PathBuf, PathBuf, String) {
    let (src, dst) = (root.join("src"), root.join("dst"));
    write(&src.join("Song.logicx/arrangement.txt"), "the arrangement");
    write(&src.join("Song.logicx/take.wav"), "the take");
    let id = rig.engine.add_job(job_setup(&src, &dst, mode)).expect("the job is added").job.id;
    confirm(&rig.engine, &id);
    assert_eq!(read(&dst.join("Song.logicx/take.wav")), "the take");
    (src, dst, id)
}

/// A take moved out of a Logic project whose arrangement changed too, and an app writes to
/// the take in its new place while the run goes on. The project is carried whole, and what
/// it removes waits for every other copy, like any removal: the take's copy is left for the
/// next sync, and so is the whole project, with the removal of the take's old copy (the
/// destination's only one).
fn a_take_moved_out_of_a_library_and_edited_during_the_run_keeps_its_old_copy(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst, id) = a_logic_project(&rig, tmp.path(), mode);
    fs::create_dir(src.join("Takes")).unwrap();
    fs::rename(src.join("Song.logicx/take.wav"), src.join("Takes/take.wav")).unwrap();
    write(&src.join("Song.logicx/arrangement.txt"), "the arrangement, with a new verse");
    let review = rig.engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.destination.removed, 1, "{:?}", review.tally);
    let take = src.join("Takes/take.wav");
    rig.at_first_step(move || fs::OpenOptions::new().append(true).open(&take).unwrap().write_all(b", recorded again").unwrap());
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    assert_eq!(read(&dst.join("Song.logicx/take.wav")), "the take", "the old copy waits for the new one");
    assert_eq!(read(&dst.join("Song.logicx/arrangement.txt")), "the arrangement", "the project isn't mixed");
    let run = view(&rig.engine, &id).job.last_run.unwrap();
    assert_eq!((run.copied, run.deleted), (0, 0), "{}", run.describe());

    // The next sync carries the take and the project.
    confirm(&rig.engine, &id);
    assert_eq!(read(&dst.join("Takes/take.wav")), "the take, recorded again");
    assert_eq!(read(&dst.join("Song.logicx/arrangement.txt")), "the arrangement, with a new verse");
    assert!(!dst.join("Song.logicx/take.wav").exists());
}

#[test]
fn two_way_keeps_the_old_copy_of_a_take_moved_out_of_a_library_and_edited_during_the_run() {
    a_take_moved_out_of_a_library_and_edited_during_the_run_keeps_its_old_copy(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_the_old_copy_of_a_take_moved_out_of_a_library_and_edited_during_the_run() {
    a_take_moved_out_of_a_library_and_edited_during_the_run_keeps_its_old_copy(SyncMode::Mirror);
}

/// The same, but the take's new folder can't be made (a file took its name on the
/// destination meanwhile), so its copy fails: the project stays as it was.
fn a_take_moved_out_of_a_library_keeps_its_old_copy_when_its_copy_fails(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst, id) = a_logic_project(&rig, tmp.path(), mode);
    fs::create_dir(src.join("Takes")).unwrap();
    fs::rename(src.join("Song.logicx/take.wav"), src.join("Takes/take.wav")).unwrap();
    write(&src.join("Song.logicx/arrangement.txt"), "the arrangement, with a new verse");
    let review = rig.engine.prepare_review(&id).unwrap();
    let in_the_way = dst.join("Takes");
    rig.at_first_step(move || write(&in_the_way, "a file where the new folder goes"));
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    assert_eq!(read(&dst.join("Song.logicx/take.wav")), "the take", "the old copy waits for the new one");
    assert_eq!(read(&dst.join("Song.logicx/arrangement.txt")), "the arrangement", "the project isn't mixed");
    let run = view(&rig.engine, &id).job.last_run.unwrap();
    assert!(run.failed > 0 && run.deleted == 0, "{}", run.describe());
}

#[test]
fn two_way_keeps_the_old_copy_of_a_take_moved_out_of_a_library_when_its_copy_fails() {
    a_take_moved_out_of_a_library_keeps_its_old_copy_when_its_copy_fails(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_the_old_copy_of_a_take_moved_out_of_a_library_when_its_copy_fails() {
    a_take_moved_out_of_a_library_keeps_its_old_copy_when_its_copy_fails(SyncMode::Mirror);
}

/// A take moved into a Logic project from a folder deeper down, another take removed from
/// the project, and an app writes to the moved take while the run goes on. The project is
/// carried after every other copy, but still before anything else is removed: it waits for
/// the next sync, and so does the removal of the moved take's old copy, the destination's
/// only one.
fn a_take_moved_into_a_library_keeps_its_old_copy_when_the_library_waits(mode: SyncMode) {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig::open(&tmp.path().join("data"));
    let (src, dst, id) = a_logic_project(&rig, tmp.path(), mode);
    write(&src.join("Old takes/2024/solo.wav"), "the solo");
    confirm(&rig.engine, &id);
    assert_eq!(read(&dst.join("Old takes/2024/solo.wav")), "the solo");
    fs::rename(src.join("Old takes/2024/solo.wav"), src.join("Song.logicx/solo.wav")).unwrap();
    fs::remove_file(src.join("Song.logicx/take.wav")).unwrap();
    let review = rig.engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.destination.removed, 2, "{:?}", review.tally);
    let solo = src.join("Song.logicx/solo.wav");
    rig.at_first_step(move || fs::OpenOptions::new().append(true).open(&solo).unwrap().write_all(b", recorded again").unwrap());
    let_progress_throttle_pass();
    rig.engine.approve(&id, &review.review_id, true).unwrap();
    wait_idle(&rig.engine, &id);
    assert!(rig.hook_fired());
    assert_eq!(read(&dst.join("Old takes/2024/solo.wav")), "the solo", "the old copy waits for the new one");
    assert_eq!(read(&dst.join("Song.logicx/take.wav")), "the take", "the project waits whole");
    let run = view(&rig.engine, &id).job.last_run.unwrap();
    assert_eq!((run.copied, run.deleted), (0, 0), "{}", run.describe());

    // The next sync carries the project, then removes the old copy.
    confirm(&rig.engine, &id);
    assert_eq!(read(&dst.join("Song.logicx/solo.wav")), "the solo, recorded again");
    assert!(!dst.join("Song.logicx/take.wav").exists() && !dst.join("Old takes/2024/solo.wav").exists());
}

#[test]
fn two_way_keeps_the_old_copy_of_a_take_moved_into_a_library_that_waits() {
    a_take_moved_into_a_library_keeps_its_old_copy_when_the_library_waits(SyncMode::TwoWay);
}

#[test]
fn mirror_keeps_the_old_copy_of_a_take_moved_into_a_library_that_waits() {
    a_take_moved_into_a_library_keeps_its_old_copy_when_the_library_waits(SyncMode::Mirror);
}
