//! End-to-end: the scanner, network shares and cloud folders, with real folders on disk,
//! real looks, lists and runs through the engine.

use charon_core::model::{Level, SyncMode};
use charon_core::plan::NoteKind;
use charon_core::{ConflictRule, Engine, EngineEvent, Error, JobSetup, SyncStart, Trigger};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

fn engine(data: &Path) -> Engine {
    Engine::open(data, Arc::new(|_: EngineEvent| {})).unwrap()
}

/// Every event an engine sent, with the thread it was sent from.
type Events = Arc<Mutex<Vec<(ThreadId, EngineEvent)>>>;

/// An engine that keeps every event it sends.
fn recording(data: &Path) -> (Engine, Events) {
    let events: Events = Arc::default();
    let sink = events.clone();
    let engine = Engine::open(data, Arc::new(move |event: EngineEvent| sink.lock().unwrap().push((thread::current().id(), event)))).unwrap();
    (engine, events)
}

fn setup(src: &Path, dst: &Path, mode: SyncMode, conflict: ConflictRule) -> JobSetup {
    JobSetup {
        name: String::new(),
        source: src.to_path_buf(),
        destination: dst.to_path_buf(),
        mode,
        conflict,
        trigger: Trigger::Manual,
        excludes: JobSetup::default_excludes(),
        safe_delete: true,
        full_automation: false,
    }
}

/// Waits until no sync of the job runs.
fn wait(engine: &Engine, id: &String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while engine.is_running(id) {
        assert!(Instant::now() < deadline, "sync took too long");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Looks at the job, confirms the list as it is, and waits for the sync to finish.
fn confirm(engine: &Engine, id: &String) -> charon_core::model::Review {
    let review = engine.prepare_review(id).unwrap();
    engine.approve(id, &review.review_id, true).unwrap();
    wait(engine, id);
    review
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn set_modified(path: &Path, secs: u64) {
    let time = std::time::UNIX_EPOCH + Duration::from_secs(secs);
    fs::File::options().write(true).open(path).unwrap().set_times(fs::FileTimes::new().set_modified(time)).unwrap();
}

#[test]
fn full_automation_holds_a_mirror_whose_source_folder_suddenly_looks_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    for i in 0..40 {
        write(&src.join(format!("docs/d{i}.txt")), "x");
    }
    for i in 0..6 {
        write(&src.join(format!("photos/2019/p{i}.jpg")), "x");
    }
    fs::create_dir_all(&dst).unwrap();
    let engine = engine(&data);
    let mut job = setup(&src, &dst, SyncMode::Mirror, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    confirm(&engine, &id);

    // A share that lists the folder before it has loaded: there, but showing nothing inside.
    for i in 0..6 {
        fs::remove_file(src.join(format!("photos/2019/p{i}.jpg"))).unwrap();
    }
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Started);
    wait(&engine, &id);
    for i in 0..6 {
        assert!(dst.join(format!("photos/2019/p{i}.jpg")).exists(), "nothing was removed");
    }
    let view = engine.job(&id).unwrap();
    assert!(view.job.needs_review);
    assert!(view.job.held.as_deref().is_some_and(|held| held.contains("photos/2019") && held.contains("looks empty")), "{:?}", view.job.held);
    // The list says why, in red, and asks for the tick.
    let review = engine.prepare_review(&id).unwrap();
    assert!(review.needs_tick);
    assert!(review.warnings.iter().any(|w| w.level == Level::Danger && w.text.contains("photos/2019")), "{:?}", review.warnings);
}

#[test]
fn looks_never_read_the_folders_on_the_thread_that_asks() {
    // Charon's own threads never download files that are only in the cloud. A list is
    // asked for on the window's thread, which must keep its own setting.
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("same.txt"), "same contents");
    write(&b.join("same.txt"), "same contents");
    set_modified(&a.join("same.txt"), 1_600_000_000);
    set_modified(&b.join("same.txt"), 1_700_000_000);
    let (engine, events) = recording(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;

    let review = engine.prepare_review(&id).unwrap();
    assert_eq!(review.total_steps, 0, "found identical by their contents");
    let asking = thread::current().id();
    let events = events.lock().unwrap();
    let looking: Vec<&(ThreadId, EngineEvent)> =
        events.iter().filter(|(_, event)| matches!(event, EngineEvent::ScanProgress { .. } | EngineEvent::CompareProgress { .. })).collect();
    assert!(looking.iter().any(|(_, event)| matches!(event, EngineEvent::CompareProgress { .. })), "the contents were compared");
    assert!(looking.iter().filter(|(_, event)| matches!(event, EngineEvent::ScanProgress { .. })).count() >= 2, "both sides were scanned");
    assert!(looking.iter().all(|(thread, _)| *thread != asking), "nothing was read on the thread that asked");
}

#[cfg(target_os = "linux")]
#[test]
fn a_change_left_in_only_by_its_case_makes_the_list_stale() {
    // Both folders tell upper and lower case apart, so "*.tmp" leaves "notes.TMP" in the
    // sync: a change to it matters.
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("a.txt"), "a");
    fs::create_dir_all(&dst).unwrap();
    let engine = engine(&data);
    let mut job = setup(&src, &dst, SyncMode::Backup, ConflictRule::Ask);
    job.excludes.push("*.tmp".into());
    let id = engine.add_job(job).unwrap().job.id;

    let review = engine.prepare_review(&id).unwrap();
    assert!(engine.note_change(&id, &src.join("notes.TMP")));
    assert!(matches!(engine.approve(&id, &review.review_id, true), Err(Error::ReviewOutdated(_))));
    // A change to an item the sync leaves out still doesn't matter.
    let review = engine.prepare_review(&id).unwrap();
    assert!(!engine.note_change(&id, &src.join("notes.tmp")));
    engine.approve(&id, &review.review_id, true).unwrap();
    wait(&engine, &id);
}

#[cfg(target_os = "linux")]
#[test]
fn a_first_sync_into_an_empty_folder_is_trusted_the_next_time() {
    // tmpfs is a kind of drive whose case setting Charon can't guess; an empty folder there
    // used to get another answer than the same folder once it held files.
    let shared_memory = Path::new("/dev/shm");
    if !shared_memory.is_dir() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let far = tempfile::tempdir_in(shared_memory).unwrap();
    let (a, b, data) = (tmp.path().join("a"), far.path().join("b"), tmp.path().join("data"));
    write(&a.join("Report.txt"), "1");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    confirm(&engine, &id);
    assert!(b.join("Report.txt").exists());

    write(&a.join("Notes.txt"), "2");
    let review = engine.prepare_review(&id).unwrap();
    assert!(review.warnings.iter().all(|w| !w.text.contains("upper and lower case")), "{:?}", review.warnings);
    assert!(review.warnings.iter().all(|w| w.level == Level::Info), "the memory is trusted: {:?}", review.warnings);
    assert_eq!(review.tally.destination.new_files, 1);
}

#[test]
fn a_folder_named_like_a_drives_own_is_left_alone_and_listed() {
    // The top of a drive keeps folders of its own; anywhere else it's the person's folder.
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("lost+found/notes.txt"), "mine");
    write(&src.join("a.txt"), "a");
    fs::create_dir_all(&dst).unwrap();
    let engine = engine(&data);
    let id = engine.add_job(setup(&src, &dst, SyncMode::Backup, ConflictRule::Ask)).unwrap().job.id;

    let review = confirm(&engine, &id);
    assert!(review.notes.iter().any(|note| note.kind == NoteKind::LeftOut && note.path == "lost+found"), "{:?}", review.notes);
    assert!(dst.join("a.txt").exists());
    assert!(!dst.join("lost+found").exists(), "left alone");
}
