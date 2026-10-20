//! End-to-end: which folders a Nextcloud or ownCloud client syncs is told by the client's
//! own settings on this computer, not by a copy of its journal.
//!
//! The test sets the variables that lead Charon to the clients' settings before anything
//! reads them, so it's the only test in this file.

use charon_core::model::SyncMode;
use charon_core::plan::NoteKind;
use charon_core::{ConflictRule, Engine, EngineEvent, JobSetup, Trigger};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn engine(data: &Path) -> Engine {
    Engine::open(data, Arc::new(|_: EngineEvent| {})).unwrap()
}

fn setup(src: &Path, dst: &Path, mode: SyncMode) -> JobSetup {
    JobSetup {
        name: String::new(),
        source: src.to_path_buf(),
        destination: dst.to_path_buf(),
        mode,
        conflict: ConflictRule::Ask,
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

/// Makes `home` the home folder of this run, with a Nextcloud client's settings that list
/// `folders` as the folders it syncs, where the client keeps them on Linux, macOS and
/// Windows.
fn home_with_nextcloud_syncing(home: &Path, folders: &[&Path]) {
    let mut settings = String::from("[General]\nclientVersion=3.13.0\n\n[Accounts]\n");
    for (number, folder) in folders.iter().enumerate() {
        // As the client writes a folder: with `/` between its parts on Windows too.
        settings += &format!("0\\Folders\\{}\\localPath={}/\n", number + 1, folder.display().to_string().replace('\\', "/"));
    }
    for settings_folder in [".config", "Library/Preferences", "AppData/Roaming"] {
        write(&home.join(settings_folder).join("Nextcloud/nextcloud.cfg"), &settings);
    }
    std::env::set_var("HOME", home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    std::env::set_var("APPDATA", home.join("AppData/Roaming"));
    std::env::set_var("LOCALAPPDATA", home.join("AppData/Local"));
}

/// A backup made by a tool that copied a Nextcloud folder as it was, from its top (Charon
/// 0.2, rsync, cp), holds a copy of the client's journal, and copies of the placeholders of
/// the files that were only in the cloud then. The client's settings don't list the backup,
/// so it's no folder the client syncs: the copies are ordinary files, and the file each one
/// names is backed up all the same.
#[test]
fn a_copy_of_a_journal_never_makes_a_backup_look_synced_by_the_client() {
    let tmp = tempfile::tempdir().unwrap();
    let modes = [SyncMode::Mirror, SyncMode::Backup, SyncMode::TwoWay];
    let synced = |mode: SyncMode| tmp.path().join(format!("home/{mode:?}/Nextcloud"));
    home_with_nextcloud_syncing(&tmp.path().join("home"), &modes.map(synced).iter().map(|folder| folder.as_path()).collect::<Vec<_>>());

    for mode in modes {
        let (src, dst, data) = (synced(mode), tmp.path().join(format!("usb/{mode:?}/Nextcloud backup")), tmp.path().join(format!("data/{mode:?}")));
        // The Nextcloud folder, at its top: the client's journal, and the report, downloaded
        // again since the backup was made.
        write(&src.join(".sync_0123456789ab.db"), "the client's journal");
        write(&src.join("Docs/Report.pdf"), "the report");
        write(&src.join("Docs/Notes.txt"), "notes");
        write(&dst.join(".sync_0123456789ab.db"), "an old copy of the client's journal");
        write(&dst.join("Docs/Report.pdf.nextcloud"), " ");
        write(&dst.join("Docs/Notes.txt"), "notes");
        let engine = engine(&data);
        let id = engine.add_job(setup(&src, &dst, mode)).unwrap().job.id;
        let review = confirm(&engine, &id);
        assert!(review.notes.iter().all(|note| note.kind != NoteKind::Unreadable), "{mode:?}: {:?}", review.notes);
        assert_eq!(fs::read_to_string(dst.join("Docs/Report.pdf")).ok().as_deref(), Some("the report"), "{mode:?}: backed up");
        assert_eq!(dst.join("Docs/Report.pdf.nextcloud").exists(), mode != SyncMode::Mirror, "{mode:?}: a Mirror replaces the copy of the placeholder");
        assert!(!src.join("Docs/Report.pdf.nextcloud").exists(), "{mode:?}: never made where the client would take it for its own");
        assert_eq!(fs::read_to_string(dst.join(".sync_0123456789ab.db")).unwrap(), "an old copy of the client's journal", "{mode:?}");
        let again = engine.prepare_review(&id).unwrap();
        assert_eq!(again.total_steps, 0, "{mode:?}: {:?}", again.steps);

        // In the folder the client syncs, a placeholder is the client's own: the file it
        // stands for is only in the cloud, and left alone on both sides.
        fs::remove_file(src.join("Docs/Notes.txt")).unwrap();
        write(&src.join("Docs/Notes.txt.nextcloud"), " ");
        let review = engine.prepare_review(&id).unwrap();
        assert_eq!(review.total_steps, 0, "{mode:?}: {:?}", review.steps);
        assert!(review.notes.iter().any(|note| note.kind == NoteKind::Unreadable && note.path == "Docs/Notes.txt" && note.reason.contains("only in the cloud")), "{mode:?}: {:?}", review.notes);
    }
}
