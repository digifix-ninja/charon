//! End-to-end: real folders on disk, real runs through the engine.

use charon_core::{ConflictChoice, ConflictRule, Engine, EngineEvent, JobSetup, JobStatus, SyncMode, Trigger};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn engine(data: &Path) -> Engine {
    Engine::open(data, Arc::new(|_: EngineEvent| {})).unwrap()
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
    }
}

/// Runs a sync and waits for it to finish.
fn sync(engine: &Engine, id: &String) {
    // A check started by add_job may still be running; that's fine, syncs don't wait on it.
    engine.sync_now(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while engine.is_running(id) {
        assert!(Instant::now() < deadline, "sync took too long");
        thread::sleep(Duration::from_millis(20));
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn mirror_copies_and_removes_into_the_trash() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    write(&src.join("photos/cat.jpg"), "meow");
    write(&src.join(".DS_Store"), "junk");
    write(&dst.join("old.txt"), "old");

    let engine = engine(&data);
    let job = engine.add_job(setup(&src, &dst, SyncMode::Mirror, ConflictRule::Ask)).unwrap();
    assert_eq!(job.job.setup.name, "src → dst");

    let preview = engine.preview(&job.job.id).unwrap();
    assert_eq!(preview.summary.copies, 1);
    assert_eq!(preview.summary.deletes, 1);

    sync(&engine, &job.job.id);
    assert_eq!(fs::read_to_string(dst.join("photos/cat.jpg")).unwrap(), "meow");
    assert!(!dst.join(".DS_Store").exists(), "excluded files stay behind");
    assert!(!dst.join("old.txt").exists());
    let trashed: Vec<_> = walk(&dst.join(".charon-trash"));
    assert!(trashed.iter().any(|p| p.ends_with("old.txt")), "removed file is in the trash");
    assert_eq!(engine.job(&job.job.id).unwrap().status, JobStatus::InSync);
    assert!(engine.log(&job.job.id, 10).iter().any(|l| l.text.contains("copied 1 file")));
}

#[test]
fn mirror_refuses_to_wipe_from_an_empty_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, data) = (tmp.path().join("src"), tmp.path().join("dst"), tmp.path().join("data"));
    fs::create_dir_all(&src).unwrap();
    write(&dst.join("precious.txt"), "keep me");

    let engine = engine(&data);
    let job = engine.add_job(setup(&src, &dst, SyncMode::Mirror, ConflictRule::Ask)).unwrap();
    sync(&engine, &job.job.id);
    assert!(dst.join("precious.txt").exists());
    assert_eq!(engine.job(&job.job.id).unwrap().status, JobStatus::Error);
}

#[test]
fn two_way_carries_changes_and_deletions_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("from-a.txt"), "a");
    write(&b.join("from-b.txt"), "b");

    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    sync(&engine, &id);
    assert!(a.join("from-b.txt").exists() && b.join("from-a.txt").exists());

    // Deleting on one side now deletes on the other, instead of copying back.
    fs::remove_file(b.join("from-a.txt")).unwrap();
    sync(&engine, &id);
    assert!(!a.join("from-a.txt").exists());
}

#[test]
fn two_way_conflicts_wait_for_a_decision() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, data) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"));
    write(&a.join("plan.txt"), "first");

    let engine = engine(&data);
    let id = engine.add_job(setup(&a, &b, SyncMode::TwoWay, ConflictRule::Ask)).unwrap().job.id;
    sync(&engine, &id);

    write(&a.join("plan.txt"), "edited on a");
    write(&b.join("plan.txt"), "edited on b, longer");
    sync(&engine, &id);
    let view = engine.job(&id).unwrap();
    assert_eq!(view.status, JobStatus::Conflict);
    assert_eq!(view.conflicts[0].path, "plan.txt");

    engine.resolve_conflict(&id, "plan.txt", ConflictChoice::KeepBoth).unwrap();
    let names: Vec<String> = fs::read_dir(&a).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().any(|n| n.starts_with("plan (conflict ")), "{names:?}");
    assert_eq!(fs::read_to_string(b.join("plan.txt")).unwrap(), "edited on a");
    assert!(engine.job(&id).unwrap().conflicts.is_empty());
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
