use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use tauri_kit_watch::{Change, ChangeKind, Notice, OwnWrites, Watch, Watcher};

const DEBOUNCE: Duration = Duration::from_millis(100);
/// Long enough for a slow CI machine to deliver a batch; a test waits this long only when it
/// expects nothing.
const QUIET: Duration = Duration::from_millis(1500);
const PATIENCE: Duration = Duration::from_secs(10);

struct Harness {
    dir: tempfile::TempDir,
    own: OwnWrites,
    notices: Receiver<Notice>,
    _watcher: Watcher,
}

fn watch(configure: impl FnOnce(Watch) -> Watch) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let own = OwnWrites::new();
    let (tx, notices) = mpsc::channel();
    let watcher = configure(Watch::new(dir.path()).debounce(DEBOUNCE).own_writes(&own))
        .start(move |notice| {
            let _ = tx.send(notice);
        })
        .unwrap();
    // Some platforms start delivering only a moment after the watch is set up.
    std::thread::sleep(Duration::from_millis(200));
    Harness {
        dir,
        own,
        notices,
        _watcher: watcher,
    }
}

impl Harness {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Changes delivered until `done` holds for the latest state of every path seen, or patience
    /// runs out. Later batches override earlier ones for the same path.
    fn changes_until(&self, done: impl Fn(&[Change]) -> bool) -> Vec<Change> {
        let deadline = Instant::now() + PATIENCE;
        let mut latest: Vec<Change> = Vec::new();
        while !done(&latest) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.notices.recv_timeout(left) {
                Ok(Notice::Changed(changes)) => {
                    for change in changes {
                        latest.retain(|c| c.path != change.path);
                        latest.push(change);
                    }
                }
                Ok(Notice::Rescan) => panic!("unexpected rescan"),
                Err(_) => break,
            }
        }
        latest
    }

    fn nothing_for(&self, what: &str) {
        match self.notices.recv_timeout(QUIET) {
            Err(_) => {}
            Ok(notice) => panic!("expected no notice for {what}, got {notice:?}"),
        }
    }
}

fn has(changes: &[Change], path: &str, kind: ChangeKind) -> bool {
    changes
        .iter()
        .any(|c| c.path == Path::new(path) && c.kind == kind)
}

#[test]
fn reports_a_file_written_by_someone_else() {
    let h = watch(|w| w);
    fs::write(h.path("note.md"), "hello").unwrap();
    let changes = h.changes_until(|c| has(c, "note.md", ChangeKind::Written));
    assert!(has(&changes, "note.md", ChangeKind::Written), "{changes:?}");
}

#[test]
fn reports_a_removed_file() {
    let h = watch(|w| w);
    fs::write(h.path("note.md"), "hello").unwrap();
    h.changes_until(|c| has(c, "note.md", ChangeKind::Written));
    fs::remove_file(h.path("note.md")).unwrap();
    let changes = h.changes_until(|c| has(c, "note.md", ChangeKind::Removed));
    assert!(has(&changes, "note.md", ChangeKind::Removed), "{changes:?}");
}

#[test]
fn reports_a_rename_as_the_old_path_removed_and_the_new_one_written() {
    let h = watch(|w| w);
    fs::write(h.path("a.md"), "hello").unwrap();
    h.changes_until(|c| has(c, "a.md", ChangeKind::Written));
    fs::rename(h.path("a.md"), h.path("b.md")).unwrap();
    let changes = h.changes_until(|c| {
        has(c, "a.md", ChangeKind::Removed) && has(c, "b.md", ChangeKind::Written)
    });
    assert!(has(&changes, "a.md", ChangeKind::Removed), "{changes:?}");
    assert!(has(&changes, "b.md", ChangeKind::Written), "{changes:?}");
}

#[test]
fn reports_files_in_subfolders_relative_to_the_root() {
    let h = watch(|w| w);
    fs::create_dir_all(h.path("docs/2026")).unwrap();
    fs::write(h.path("docs/2026/note.md"), "hello").unwrap();
    let expected = Path::new("docs").join("2026").join("note.md");
    let changes = h.changes_until(|c| c.iter().any(|c| c.path == expected));
    assert!(
        changes
            .iter()
            .any(|c| c.path == expected && c.kind == ChangeKind::Written),
        "{changes:?}"
    );
    assert!(
        !changes.iter().any(|c| c.path == Path::new("docs")),
        "folders are not reported: {changes:?}"
    );
}

#[test]
fn leaves_out_the_apps_own_atomic_write() {
    let h = watch(|w| w);
    let path = h.path("note.md");
    h.own.record(&path, b"mine");
    tauri_kit_fs::write_atomic(&path, b"mine").unwrap();
    h.nothing_for("the app's own write");
}

#[test]
fn leaves_out_the_apps_own_write_recorded_with_forward_slashes() {
    let h = watch(|w| w);
    fs::create_dir(h.path("docs")).unwrap();
    h.nothing_for("creating the folder");
    let written_as = format!("{}/docs/note.md", h.dir.path().display());
    h.own.record(Path::new(&written_as), b"mine");
    tauri_kit_fs::write_atomic(&h.path("docs").join("note.md"), b"mine").unwrap();
    h.nothing_for("the app's own write");
}

#[test]
fn reports_a_change_made_after_the_apps_own_write() {
    let h = watch(|w| w);
    let path = h.path("note.md");
    h.own.record(&path, b"mine");
    tauri_kit_fs::write_atomic(&path, b"mine").unwrap();
    h.nothing_for("the app's own write");
    fs::write(&path, "theirs").unwrap();
    let changes = h.changes_until(|c| has(c, "note.md", ChangeKind::Written));
    assert!(has(&changes, "note.md", ChangeKind::Written), "{changes:?}");
}

#[test]
fn a_forgotten_write_is_reported() {
    let h = watch(|w| w);
    let path = h.path("note.md");
    h.own.record(&path, b"mine");
    h.own.forget(&path);
    fs::write(&path, "mine").unwrap();
    let changes = h.changes_until(|c| has(c, "note.md", ChangeKind::Written));
    assert!(has(&changes, "note.md", ChangeKind::Written), "{changes:?}");
}

#[test]
fn leaves_out_temporary_files_and_ignored_paths() {
    let h = watch(|w| w.ignore(|p| p.starts_with("cache")));
    fs::write(h.path(&format!("{}x", tauri_kit_fs::TEMP_PREFIX)), "tmp").unwrap();
    fs::create_dir(h.path("cache")).unwrap();
    fs::write(h.path("cache").join("index"), "cached").unwrap();
    fs::write(h.path("note.md"), "hello").unwrap();
    let changes = h.changes_until(|c| has(c, "note.md", ChangeKind::Written));
    assert_eq!(
        changes,
        vec![Change {
            path: PathBuf::from("note.md"),
            kind: ChangeKind::Written
        }]
    );
}

#[test]
fn refuses_a_root_that_is_not_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, "x").unwrap();
    let err = Watch::new(&file).start(|_| {}).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);
    let err = Watch::new(dir.path().join("missing"))
        .start(|_| {})
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);
}

#[test]
fn a_live_watch_hears_its_probes_and_they_leave_nothing_behind() {
    let h = watch(|w| w.probe_liveness(".state", Duration::from_millis(500)));
    // Several probes, each heard: no rescan, and the probe files themselves are never reported.
    h.nothing_for("probing a live watch");
    let state = h.path(".state");
    assert!(
        fs::read_dir(&state).unwrap().count() <= 1,
        "one probe file at a time"
    );
    drop(h._watcher);
    assert_eq!(
        fs::read_dir(&state).unwrap().count(),
        0,
        "the last probe is removed on stop"
    );
}

#[test]
fn refuses_a_probe_faster_than_the_debounce_window() {
    let dir = tempfile::tempdir().unwrap();
    let err = Watch::new(dir.path())
        .debounce(Duration::from_millis(300))
        .probe_liveness(".state", Duration::from_millis(600))
        .start(|_| {})
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
