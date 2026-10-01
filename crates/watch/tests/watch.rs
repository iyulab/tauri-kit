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

    /// Whether a [`Notice::Rescan`] arrives before patience runs out. Changes on the way are let by.
    fn rescan_arrives(&self) -> bool {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.notices.recv_timeout(left) {
                Ok(Notice::Rescan) => return true,
                Ok(Notice::Changed(_)) => {}
                Err(_) => return false,
            }
        }
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

fn is_head(path: &Path) -> bool {
    path == Path::new(".git").join("HEAD")
}

#[test]
fn a_path_the_app_names_asks_for_a_rescan_even_inside_an_ignored_folder() {
    let h = watch(|w| w.ignore(|p| p.starts_with(".git")).rescan_on(is_head));
    fs::create_dir_all(h.path(".git/objects")).unwrap();
    fs::write(h.path(".git/objects/ab"), "object").unwrap();
    h.nothing_for("writes inside the ignored folder");
    fs::write(h.path(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
    assert!(h.rescan_arrives(), "the branch record changed");
}

#[test]
fn the_rescan_takes_the_place_of_the_batch_it_came_in() {
    let h = watch(|w| w.rescan_on(|p| p == Path::new("RELOAD")));
    fs::write(h.path("note.md"), "hello").unwrap();
    fs::write(h.path("RELOAD"), "now").unwrap();
    assert!(h.rescan_arrives());
    // Read again from here: what the rescan covered is not reported on top of it.
    h.nothing_for("changes the rescan already covers");
}

#[test]
fn without_the_rule_an_ignored_path_stays_quiet() {
    let h = watch(|w| w.ignore(|p| p.starts_with(".git")));
    fs::create_dir_all(h.path(".git")).unwrap();
    fs::write(h.path(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    h.nothing_for("an ignored path no rule names");
}

/// A file another program makes and removes again quickly can be seen by the folder listing that
/// a change beside it causes, while the platform's own notifications for it cancel out. Its
/// removal must still be reported: the app must not keep a file that is gone.
#[test]
fn a_file_seen_beside_another_change_and_removed_at_once_is_reported_gone() {
    // Like an app that hears only its own kind of file: folders do not end in it, so their own
    // notifications are left out too.
    let h = watch(|w| w.ignore(|p| p.extension().is_none_or(|e| e != "md")));
    fs::create_dir(h.path("sub")).unwrap();
    fs::write(h.path("sub/a.md"), "a").unwrap();
    h.changes_until(|c| has(c, "sub/a.md", ChangeKind::Written));
    let mut seen_written = 0;
    for (round, offset) in [30u64, 50, 70, 90, 110, 130].into_iter().enumerate() {
        let stray = h.path("sub/stray.md");
        fs::write(h.path(&format!("sub/b{round}.md")), "b").unwrap();
        std::thread::sleep(Duration::from_millis(offset));
        fs::write(&stray, "s").unwrap();
        // The batch for b: once it is here, take the stray file away again at once.
        let first = h.notices.recv_timeout(PATIENCE).expect("a batch");
        fs::remove_file(&stray).unwrap();
        let mut latest: Vec<Change> = match first {
            Notice::Changed(c) => c,
            Notice::Rescan => panic!("unexpected rescan"),
        };
        while let Ok(notice) = h.notices.recv_timeout(QUIET) {
            if let Notice::Changed(changes) = notice {
                for change in changes {
                    latest.retain(|c| c.path != change.path);
                    latest.push(change);
                }
            }
        }
        if has(&latest, "sub/stray.md", ChangeKind::Written) {
            panic!("round {round} (offset {offset} ms): the stray file was reported written and never removed: {latest:?}");
        }
        if latest.iter().any(|c| c.path == Path::new("sub/stray.md")) {
            seen_written += 1;
        }
    }
    eprintln!("rounds in which the stray file was reported at all: {seen_written}");
}

/// An app that hears only its own kind of file, and says so of files alone.
fn md_only(w: Watch) -> Watch {
    w.ignore_files(|p| p.extension().is_none_or(|e| e != "md"))
}

#[test]
fn a_folder_moved_in_whole_reports_its_files_when_only_files_are_ignored() {
    let h = watch(md_only);
    // Made beside the watched folder, on the same volume, then moved in as a sync client does.
    let outside = h.path("../outside-moved-in");
    fs::create_dir_all(outside.join("deep")).unwrap();
    fs::write(outside.join("a.md"), "a").unwrap();
    fs::write(outside.join("deep").join("b.md"), "b").unwrap();
    fs::write(outside.join("x.txt"), "x").unwrap();
    fs::rename(&outside, h.path("moved")).unwrap();
    let changes = h.changes_until(|c| {
        has(c, "moved/a.md", ChangeKind::Written) && has(c, "moved/deep/b.md", ChangeKind::Written)
    });
    assert!(
        has(&changes, "moved/a.md", ChangeKind::Written),
        "{changes:?}"
    );
    assert!(
        has(&changes, "moved/deep/b.md", ChangeKind::Written),
        "{changes:?}"
    );
    assert!(
        !changes
            .iter()
            .any(|c| c.path.extension().is_none_or(|e| e != "md")),
        "only md files: {changes:?}"
    );
}

#[test]
fn a_folder_removed_whole_reports_its_files_gone_when_only_files_are_ignored() {
    let h = watch(md_only);
    fs::create_dir(h.path("sub")).unwrap();
    fs::write(h.path("sub/a.md"), "a").unwrap();
    fs::write(h.path("sub/x.txt"), "x").unwrap();
    h.changes_until(|c| has(c, "sub/a.md", ChangeKind::Written));
    // A folder removed whole is one that has been there a while. Removed at once, macOS can hand
    // its files over as made and removed in one go — no change at all, which is right for that.
    std::thread::sleep(QUIET);
    while h.notices.try_recv().is_ok() {}
    fs::remove_dir_all(h.path("sub")).unwrap();
    let changes = h.changes_until(|c| has(c, "sub/a.md", ChangeKind::Removed));
    assert!(
        has(&changes, "sub/a.md", ChangeKind::Removed),
        "{changes:?}"
    );
    assert!(
        !changes.iter().any(|c| c.path == Path::new("sub/x.txt")),
        "{changes:?}"
    );
}

#[test]
fn ignoring_files_leaves_the_folders_they_are_in_watched() {
    let h = watch(md_only);
    fs::create_dir(h.path("notes")).unwrap();
    fs::write(h.path("notes/x.txt"), "x").unwrap();
    fs::write(h.path("notes/a.md"), "a").unwrap();
    let changes = h.changes_until(|c| has(c, "notes/a.md", ChangeKind::Written));
    assert_eq!(
        changes,
        vec![Change {
            path: PathBuf::from("notes/a.md"),
            kind: ChangeKind::Written
        }]
    );
}
