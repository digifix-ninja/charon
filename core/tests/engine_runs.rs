//! End-to-end: real folders on disk, real looks, reviews and runs through the engine.

use charon_core::model::{ConflictChoice, Level, ReviewKind, StepFilter, SyncMode};
use charon_core::{ConflictRule, Engine, EngineEvent, Error, JobSetup, JobStatus, SyncStart, Trigger};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn engine(data: &Path) -> Engine {
    Engine::open(data, Arc::new(|_: EngineEvent| {})).unwrap()
}

/// An engine that keeps every event it sends.
fn recording(data: &Path) -> (Engine, Arc<Mutex<Vec<EngineEvent>>>) {
    let events: Arc<Mutex<Vec<EngineEvent>>> = Arc::default();
    let sink = events.clone();
    let engine = Engine::open(data, Arc::new(move |event: EngineEvent| sink.lock().unwrap().push(event))).unwrap();
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

/// Every file under `root`.
fn walk(root: &Path) -> Vec<std::path::PathBuf> {
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

#[test]
fn mirror_copies_and_removes_into_the_trash_once_confirmed() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("photos/cat.jpg"), "meow");
    write(&src.join(".DS_Store"), "junk");
    write(&dst.join("old.txt"), "old");

    let engine = engine(&data);
    let job = engine.add_job(setup(&src, &dst, SyncMode::Mirror, ConflictRule::Ask)).unwrap();
    let id = job.job.id.clone();
    assert_eq!(job.job.setup.name, "src → dst");

    let review = engine.prepare_review(&id).unwrap();
    assert_eq!(review.kind, ReviewKind::Sync);
    assert_eq!(review.tally.destination.new_files, 1);
    assert_eq!(review.tally.destination.removed, 1);
    assert!(review.blocked.is_none());
    let removals = engine.review_steps(&id, &review.review_id, StepFilter::Removals, "", 0, 50).unwrap();
    assert_eq!(removals.total, 1);
    assert!(removals.steps[0].danger);
    // Nothing happened yet.
    assert!(dst.join("old.txt").exists());
    assert!(!dst.join("photos").exists());

    engine.approve(&id, &review.review_id, true).unwrap();
    wait(&engine, &id);
    assert_eq!(fs::read_to_string(dst.join("photos/cat.jpg")).unwrap(), "meow");
    assert!(!dst.join(".DS_Store").exists(), "excluded files stay behind");
    assert!(!dst.join("old.txt").exists());
    let trashed: Vec<_> = walk(&dst.join(".charon-trash"));
    assert!(trashed.iter().any(|p| p.ends_with("old.txt")), "removed file is in the trash");
    let view = engine.job(&id).unwrap();
    assert!(view.job.last_run.as_ref().unwrap().approved);
    assert!(!view.job.needs_review);
    assert!(engine.log(&id, 10).iter().any(|l| l.text.contains("copied 1 file")));
}

#[test]
fn a_list_is_confirmed_once_and_only_as_it_was_made() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("a.txt"), "a");
    fs::create_dir_all(&dst).unwrap();
    let engine = engine(&data);
    let id = engine.add_job(setup(&src, &dst, SyncMode::Backup, ConflictRule::Ask)).unwrap().job.id;

    // A list made before a change in the folders can't be confirmed.
    let review = engine.prepare_review(&id).unwrap();
    assert!(engine.note_change(&id, &src.join("a.txt")));
    assert!(matches!(engine.approve(&id, &review.review_id, true), Err(Error::ReviewOutdated(_))));

    // Nor one made before the job's settings changed.
    let review = engine.prepare_review(&id).unwrap();
    let mut changed = engine.job(&id).unwrap().job.setup;
    changed.excludes.push("*.bak".into());
    engine.update_job(&id, changed).unwrap();
    assert!(engine.approve(&id, &review.review_id, true).is_err());

    // Changes to excluded items don't matter.
    let review = engine.prepare_review(&id).unwrap();
    assert!(!engine.note_change(&id, &src.join("x.bak")));
    assert!(!engine.note_change(&id, &src.join(".DS_Store")));
    engine.approve(&id, &review.review_id, true).unwrap();
    wait(&engine, &id);
    assert!(dst.join("a.txt").exists());
    // Confirmed lists are used up.
    assert!(engine.approve(&id, &review.review_id, true).is_err());
}

#[test]
fn mirror_refuses_to_wipe_from_an_empty_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    fs::create_dir_all(&src).unwrap();
    write(&dst.join("precious.txt"), "keep me");

    let engine = engine(&data);
    let id = engine.add_job(setup(&src, &dst, SyncMode::Mirror, ConflictRule::Ask)).unwrap().job.id;
    let review = engine.prepare_review(&id).unwrap();
    assert!(review.blocked.is_some());
    assert!(engine.approve(&id, &review.review_id, true).is_err());
    assert!(dst.join("precious.txt").exists());
}

#[test]
fn removals_need_the_tick_when_safe_deletes_are_off() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("keep.txt"), "k");
    write(&dst.join("keep.txt"), "k");
    write(&dst.join("extra.txt"), "x");
    let engine = engine(&data);
    let mut job = setup(&src, &dst, SyncMode::Mirror, ConflictRule::Ask);
    job.safe_delete = false;
    let id = engine.add_job(job).unwrap().job.id;
    let review = engine.prepare_review(&id).unwrap();
    assert!(review.needs_tick);
    assert!(engine.approve(&id, &review.review_id, false).is_err());
    assert!(dst.join("extra.txt").exists());
    engine.approve(&id, &review.review_id, true).unwrap();
    wait(&engine, &id);
    assert!(!dst.join("extra.txt").exists());
}

#[test]
fn two_way_carries_changes_and_deletions_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("from-a.txt"), "a");
    write(&b.join("from-b.txt"), "b");

    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    let first = confirm(&engine, &id);
    assert!(first.warnings.iter().any(|w| w.level == Level::Info && w.text.contains("first two-way sync")));
    assert!(a.join("from-b.txt").exists() && b.join("from-a.txt").exists());
    assert!(a.join(".charon").is_dir() && b.join(".charon").is_dir(), "both sides carry the job's marker");

    // Deleting on one side now deletes on the other, instead of copying back.
    fs::remove_file(b.join("from-a.txt")).unwrap();
    let review = confirm(&engine, &id);
    assert_eq!(review.tally.source.removed, 1);
    assert!(!a.join("from-a.txt").exists());
    assert!(walk(&a.join(".charon-trash")).iter().any(|p| p.ends_with("from-a.txt")));
}

#[test]
fn two_way_conflicts_are_settled_through_a_list() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("plan.txt"), "first");
    fs::create_dir_all(&b).unwrap();

    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    confirm(&engine, &id);

    write(&a.join("plan.txt"), "edited on a");
    write(&b.join("plan.txt"), "edited on b, longer");
    confirm(&engine, &id);
    let view = engine.job(&id).unwrap();
    assert_eq!(view.status, JobStatus::Conflict);
    let conflict = view.conflicts[0].clone();
    assert_eq!(conflict.path, "plan.txt");

    let review = engine.prepare_resolution(&id, &conflict, ConflictChoice::KeepBoth).unwrap();
    assert_eq!(review.kind, ReviewKind::Resolve);
    assert_eq!(fs::read_to_string(b.join("plan.txt")).unwrap(), "edited on b, longer", "nothing changes before the OK");
    engine.approve(&id, &review.review_id, false).unwrap();
    wait(&engine, &id);
    let names: Vec<String> = fs::read_dir(&a).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().any(|n| n.starts_with("plan (conflict ")), "{names:?}");
    assert_eq!(fs::read_to_string(b.join("plan.txt")).unwrap(), "edited on a");
    assert!(engine.job(&id).unwrap().conflicts.is_empty());

    // The next look sees both sides in agreement.
    let next = engine.prepare_review(&id).unwrap();
    assert_eq!(next.total_steps, 0, "{:?}", next.steps);
}

#[test]
fn a_conflict_that_changed_since_it_was_shown_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("plan.txt"), "first");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    confirm(&engine, &id);
    write(&a.join("plan.txt"), "edited on a");
    write(&b.join("plan.txt"), "edited on b, longer");
    let review = engine.prepare_review(&id).unwrap();
    engine.discard_review(&id, &review.review_id);
    let conflict = engine.job(&id).unwrap().conflicts[0].clone();
    write(&b.join("plan.txt"), "edited on b again, much longer");
    assert!(matches!(engine.prepare_resolution(&id, &conflict, ConflictChoice::KeepSource), Err(Error::ReviewOutdated(_))));
}

#[test]
fn nothing_changes_by_itself_without_full_automation() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("doc.txt"), "a");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let mut job = setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask);
    job.trigger = Trigger::OnChange { settle_seconds: 1 };
    let id = engine.add_job(job).unwrap().job.id;
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Review);
    // Only a look: the row says changes wait for the OK.
    engine.check(&id);
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.job(&id).unwrap().waiting.is_none() {
        assert!(Instant::now() < deadline, "the look never finished");
        thread::sleep(Duration::from_millis(20));
    }
    let view = engine.job(&id).unwrap();
    assert_eq!(view.status, JobStatus::Review);
    assert_eq!(view.waiting.as_ref().unwrap().items, 1);
    assert!(!b.join("doc.txt").exists());
}

#[test]
fn full_automation_starts_only_after_the_first_confirmed_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("one.txt"), "1");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let mut job = setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Review, "the first sync is always looked at");
    confirm(&engine, &id);
    assert!(engine.job(&id).unwrap().runs_by_itself);

    write(&a.join("two.txt"), "2");
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Started);
    wait(&engine, &id);
    assert!(b.join("two.txt").exists());
    assert!(!engine.job(&id).unwrap().job.last_run.unwrap().approved);
}

#[test]
fn full_automation_holds_back_a_mass_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    for i in 0..40 {
        write(&a.join(format!("f{i}.txt")), "x");
    }
    fs::create_dir_all(&b).unwrap();
    let (engine, events) = recording(&data);
    let mut job = setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    confirm(&engine, &id);
    for i in 0..30 {
        fs::remove_file(a.join(format!("f{i}.txt"))).unwrap();
    }
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Started);
    wait(&engine, &id);
    assert_eq!(walk(&b).iter().filter(|p| !p.to_string_lossy().contains(".charon")).count(), 40, "nothing was removed");
    let view = engine.job(&id).unwrap();
    assert!(view.job.needs_review && view.job.held.is_some());
    assert_eq!(view.status, JobStatus::Review);
    assert!(events.lock().unwrap().iter().any(|e| matches!(e, EngineEvent::Held { .. })));
    // The person can still look and confirm, with the tick.
    let review = engine.prepare_review(&id).unwrap();
    assert!(review.needs_tick);
    assert!(review.warnings.iter().any(|w| w.level == Level::Danger));
}

#[test]
fn stopping_a_run_makes_the_next_one_wait_for_an_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("one.txt"), "1");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let mut job = setup(&a, &b, SyncMode::Backup, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    confirm(&engine, &id);
    write(&a.join("big.bin"), &"x".repeat(1 << 20));
    engine.sync_now(&id).unwrap();
    engine.cancel(&id);
    wait(&engine, &id);
    let view = engine.job(&id).unwrap();
    assert!(view.job.last_run.as_ref().unwrap().cancelled);
    assert!(view.job.needs_review);
    assert!(view.job.held.is_some(), "the row says why it waits");
    assert_eq!(view.status, JobStatus::Review);
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Review);

    // Only a confirmed list lets it run by itself again.
    confirm(&engine, &id);
    let view = engine.job(&id).unwrap();
    assert!(!view.job.needs_review && view.job.held.is_none());
    assert!(a.join("big.bin").exists() && b.join("big.bin").exists());
}

#[test]
fn settling_a_conflict_does_not_confirm_the_rest_of_the_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("plan.txt"), "first");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let mut job = setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    confirm(&engine, &id);
    assert!(!engine.job(&id).unwrap().job.needs_review);

    // The person changes the job: its next sync waits for an OK.
    let mut changed = engine.job(&id).unwrap().job.setup.clone();
    changed.safe_delete = false;
    engine.update_job(&id, changed).unwrap();
    assert!(engine.job(&id).unwrap().job.needs_review);

    write(&a.join("plan.txt"), "edited on a");
    write(&b.join("plan.txt"), "edited on b, longer");
    write(&a.join("other.txt"), "waiting");
    engine.prepare_review(&id).map(|r| engine.discard_review(&id, &r.review_id)).unwrap();
    let conflict = engine.job(&id).unwrap().conflicts[0].clone();
    let review = engine.prepare_resolution(&id, &conflict, ConflictChoice::KeepSource).unwrap();
    engine.approve(&id, &review.review_id, false).unwrap();
    wait(&engine, &id);
    assert_eq!(fs::read_to_string(b.join("plan.txt")).unwrap(), "edited on a");
    assert!(!b.join("other.txt").exists(), "nothing but the conflict changed");
    let view = engine.job(&id).unwrap();
    assert!(view.job.needs_review, "the rest of the job's changes still wait for an OK");
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Review);
}

#[test]
fn quitting_stops_runs_without_asking_for_an_ok_and_starts_nothing_new() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("one.txt"), "1");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let mut job = setup(&a, &b, SyncMode::Backup, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    confirm(&engine, &id);
    assert!(!engine.job(&id).unwrap().job.needs_review);

    write(&a.join("big.bin"), &"x".repeat(1 << 20));
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Started);
    assert!(engine.shut_down(Duration::from_secs(30)), "the run stops in time");
    let view = engine.job(&id).unwrap();
    assert!(view.job.last_run.as_ref().unwrap().cancelled);
    assert!(!view.job.needs_review, "a quit is not the person pressing Stop");
    assert!(engine.sync_now(&id).is_err(), "nothing starts while quitting");
    assert!(engine.prepare_review(&id).is_err());
}

#[test]
fn a_second_charon_on_the_same_data_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let first = engine(tmp.path());
    assert!(matches!(Engine::open(tmp.path(), Arc::new(|_: EngineEvent| {})), Err(Error::AlreadyRunning)));
    drop(first);
}

#[test]
fn a_two_way_folder_without_its_marker_is_compared_afresh() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("one.txt"), "1");
    write(&a.join("two.txt"), "2");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    confirm(&engine, &id);

    // Another drive under the same name: empty apart from one file.
    fs::rename(&b, tmp.path().join("old-b")).unwrap();
    write(&b.join("one.txt"), "1");
    let review = engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.source.removed, 0, "nothing is removed from the source");
    assert!(review.warnings.iter().any(|w| w.text.contains("marker")));
    engine.approve(&id, &review.review_id, true).unwrap();
    wait(&engine, &id);
    assert!(a.join("two.txt").exists());
    assert!(b.join("two.txt").exists());
}

#[test]
fn an_unfinished_sync_is_not_trusted_next_time() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("one.txt"), "1");
    fs::create_dir_all(&b).unwrap();
    {
        let engine = engine(&data);
        let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
        confirm(&engine, &id);
    }
    // Charon stopped in the middle of the next sync: the mark is still set.
    let state_path = data.join("charon-state.json");
    let text = fs::read_to_string(&state_path).unwrap();
    fs::write(&state_path, text.replace("\"runInProgress\": false", "\"runInProgress\": true")).unwrap();
    fs::remove_file(b.join("one.txt")).unwrap();

    let engine = engine(&data);
    let id = engine.jobs()[0].job.id.clone();
    let review = engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.source.removed, 0);
    assert!(review.warnings.iter().any(|w| w.text.contains("didn't finish")));
}

/// Rewrites the counter of the job's marker in `root`, as an older copy of the drive has it.
fn set_marker_generation(root: &Path, id: &str, generation: u64) {
    let path = root.join(".charon").join(format!("{id}.json"));
    let mut marker: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    marker["generation"] = serde_json::json!(generation);
    fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
}

fn marker_generation(root: &Path, id: &str) -> u64 {
    let path = root.join(".charon").join(format!("{id}.json"));
    let marker: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    marker["generation"].as_u64().unwrap()
}

#[test]
fn a_drive_swapped_while_the_list_is_open_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("one.txt"), "1");
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    confirm(&engine, &id);
    write(&a.join("new.txt"), "new");
    let review = engine.prepare_review(&id).unwrap();

    // The drive is swapped for an older copy of itself: same folder, older marker.
    set_marker_generation(&b, &id, 0);
    match engine.approve(&id, &review.review_id, true) {
        Err(Error::ReviewOutdated(text)) => assert!(text.contains("marker"), "{text}"),
        other => panic!("the list must be refused: {other:?}"),
    }
    assert!(!b.join("new.txt").exists());
}

/// Something a test runs once, from inside the engine.
type Hook = Box<dyn FnOnce() + Send>;

#[test]
fn a_drive_swapped_during_the_sync_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("one.txt"), "1");
    write(&a.join("two.txt"), "2");
    fs::create_dir_all(&b).unwrap();
    // An engine that swaps the drive (an older copy of it) right before the first step.
    let swap: Arc<Mutex<Option<Hook>>> = Arc::default();
    let armed = swap.clone();
    let engine = Engine::open(
        &data,
        Arc::new(move |event: EngineEvent| {
            if let EngineEvent::Progress { progress, .. } = &event {
                if progress.current.is_some() {
                    if let Some(hook) = armed.lock().unwrap().take() {
                        hook();
                    }
                }
            }
        }),
    )
    .unwrap();
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    confirm(&engine, &id);
    assert_eq!(marker_generation(&b, &id), 1);

    write(&a.join("new.txt"), "new");
    let review = engine.prepare_review(&id).unwrap();
    let (older, marker_id) = (b.clone(), id.clone());
    thread::sleep(Duration::from_millis(150)); // let the progress throttle pass
    *swap.lock().unwrap() = Some(Box::new(move || set_marker_generation(&older, &marker_id, 0)));
    engine.approve(&id, &review.review_id, true).unwrap();
    wait(&engine, &id);
    assert!(swap.lock().unwrap().is_none(), "the swap happened during the sync");

    let view = engine.job(&id).unwrap();
    let run = view.job.last_run.clone().unwrap();
    assert!(run.error.as_deref().is_some_and(|e| e.contains("marker")), "{run:?}");
    assert_eq!(marker_generation(&b, &id), 0, "the other drive isn't stamped as the synced one");
    assert!(view.job.needs_review && view.job.held.is_some(), "the next sync waits for an OK");

    // The next look trusts nothing: it removes nothing anywhere.
    fs::remove_file(b.join("one.txt")).unwrap();
    let next = engine.prepare_review(&id).unwrap();
    assert_eq!((next.tally.source.removed, next.tally.destination.removed), (0, 0));
}

#[test]
fn mirror_from_an_older_copy_of_its_source_is_held() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    for n in 0..20 {
        write(&a.join(format!("p{n}.jpg")), "photo");
    }
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let mut job = setup(&a, &b, SyncMode::Mirror, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    confirm(&engine, &id);
    write(&a.join("p20.jpg"), "newest");
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Started);
    wait(&engine, &id);
    assert!(b.join("p20.jpg").exists());
    assert_eq!(marker_generation(&a, &id), 2);

    // The source drive is swapped for an older copy of itself (made before p20).
    fs::remove_file(a.join("p20.jpg")).unwrap();
    set_marker_generation(&a, &id, 1);
    engine.sync_now(&id).unwrap();
    wait(&engine, &id);
    let view = engine.job(&id).unwrap();
    assert!(view.job.held.as_deref().is_some_and(|held| held.contains("back in time")), "held for a look: {:?}", view.job.held);
    assert!(b.join("p20.jpg").exists(), "the newest backup is still there");
    assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Review);
    let review = engine.prepare_review(&id).unwrap();
    assert!(review.needs_tick);
    assert!(review.warnings.iter().any(|w| w.level == Level::Danger && w.text.contains("back in time")), "{:?}", review.warnings);
}

/// What a scan sees of a file (size and date; the change time left out matches anything).
fn seen(path: &Path) -> charon_core::scan::Entry {
    let meta = fs::metadata(path).unwrap();
    let modified = meta.modified().unwrap();
    let ms = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(after) => after.as_millis() as i64,
        Err(before) => -(before.duration().as_millis() as i64),
    };
    charon_core::scan::Entry::file(meta.len(), ms)
}

fn set_modified(path: &Path, secs: u64) {
    let time = std::time::UNIX_EPOCH + Duration::from_secs(secs);
    fs::File::options().write(true).open(path).unwrap().set_times(fs::FileTimes::new().set_modified(time)).unwrap();
}

fn marker_token(root: &Path, id: &str) -> String {
    let text = fs::read_to_string(root.join(".charon").join(format!("{id}.json"))).unwrap();
    serde_json::from_str::<serde_json::Value>(&text).unwrap()["token"].as_str().unwrap().to_string()
}

#[test]
fn a_backup_leaves_alone_copies_whose_dates_didnt_stick() {
    use charon_core::plan::{Baseline, Pair};
    use charon_core::store::Landings;
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("old.txt"), "1975");
    set_modified(&src.join("old.txt"), 157_766_400); // 1 January 1975
    fs::create_dir_all(&dst).unwrap();
    let engine = engine(&data);
    let job = engine.add_job(setup(&src, &dst, SyncMode::Backup, ConflictRule::Ask)).unwrap().job;
    let id = job.id.clone();
    confirm(&engine, &id);
    // The destination's drive couldn't keep the date (FAT starts in 1980).
    set_modified(&dst.join("old.txt"), 315_532_800);
    let review = engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.destination.replaced, 1, "without a record it's copied again");
    engine.discard_review(&id, &review.review_id);

    // The record a run keeps of such a copy.
    let mut landed = Baseline::new();
    landed.insert("old.txt", Pair::new(seen(&src.join("old.txt")), seen(&dst.join("old.txt"))));
    let landings = Landings::new(data.join("landings"));
    landings.save(&id, &job.setup, false, &marker_token(&dst, &id), &landed).unwrap();
    let review = engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.destination.replaced, 0);
    assert!(review.warnings.iter().any(|w| w.level == Level::Info && w.text.contains("other dates")), "{:?}", review.warnings);
    engine.discard_review(&id, &review.review_id);

    // Edited in the source: copied again, and the record of it goes once the date sticks.
    write(&src.join("old.txt"), "1976");
    let review = confirm(&engine, &id);
    assert_eq!(review.tally.destination.replaced, 1);
    assert_eq!(fs::read_to_string(dst.join("old.txt")).unwrap(), "1976");
    assert!(!data.join("landings").join(format!("{id}.json")).exists());
}

#[test]
fn copies_whose_dates_didnt_stick_on_another_drive_are_copied_again() {
    use charon_core::plan::{Baseline, Pair};
    use charon_core::store::Landings;
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("old.txt"), "1975");
    fs::create_dir_all(&dst).unwrap();
    let engine = engine(&data);
    let job = engine.add_job(setup(&src, &dst, SyncMode::Backup, ConflictRule::Ask)).unwrap().job;
    let id = job.id.clone();
    confirm(&engine, &id);
    set_modified(&dst.join("old.txt"), 315_532_800);
    let mut landed = Baseline::new();
    landed.insert("old.txt", Pair::new(seen(&src.join("old.txt")), seen(&dst.join("old.txt"))));
    // Made for a destination whose marker had another token: another drive.
    Landings::new(data.join("landings")).save(&id, &job.setup, false, "another-drive", &landed).unwrap();
    let review = engine.prepare_review(&id).unwrap();
    assert_eq!(review.tally.destination.replaced, 1);
}

#[test]
fn full_automation_counts_each_file_once_a_day_and_the_list_shows_the_day() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    for i in 0..40 {
        write(&a.join(format!("f{i}.txt")), "x");
    }
    fs::create_dir_all(&b).unwrap();
    let engine = engine(&data);
    let mut job = setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask);
    job.full_automation = true;
    let id = engine.add_job(job).unwrap().job.id;
    confirm(&engine, &id);
    let run = |engine: &Engine| {
        assert_eq!(engine.sync_now(&id).unwrap(), SyncStart::Started);
        wait(engine, &id);
        engine.job(&id).unwrap().job.held
    };

    // A log saved all day long counts once.
    for n in 0..25 {
        write(&a.join("f0.txt"), &"log ".repeat(n + 2));
        assert_eq!(run(&engine), None, "run {n} was held");
    }
    // Four other files at a time: by the fifth run, half of the folder was replaced today.
    for round in 0..4 {
        for i in 1 + round * 4..5 + round * 4 {
            write(&a.join(format!("f{i}.txt")), &format!("edit {round}"));
        }
        assert_eq!(run(&engine), None, "round {round} was held");
    }
    for i in 17..21 {
        write(&a.join(format!("f{i}.txt")), "edit 4");
    }
    let held = run(&engine).expect("held back");
    assert!(held.contains("within a day"), "{held}");
    assert_eq!(fs::read_to_string(b.join("f17.txt")).unwrap(), "x", "nothing was replaced");

    // The list shows why, and once confirmed the day's count starts again.
    let review = engine.prepare_review(&id).unwrap();
    assert!(review.needs_tick);
    assert!(review.warnings.iter().any(|w| w.level == Level::Danger && w.text.contains("within a day")), "{:?}", review.warnings);
    engine.approve(&id, &review.review_id, true).unwrap();
    wait(&engine, &id);
    assert_eq!(fs::read_to_string(b.join("f17.txt")).unwrap(), "edit 4");
    for i in 21..25 {
        write(&a.join(format!("f{i}.txt")), "edit 5");
    }
    assert_eq!(run(&engine), None);
}
