//! Against real processes, through the platform shell.

use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tauri_kit_sidecar::{LineReadiness, Output, Readiness, Sidecar};

/// A shell command line, passed verbatim.
fn shell(line: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("cmd");
        cmd.raw_arg("/C").raw_arg(line);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(line);
        cmd
    }
}

/// Runs for about 30 seconds.
fn long_running() -> Command {
    if cfg!(windows) {
        shell("ping -n 30 127.0.0.1 >NUL")
    } else {
        shell("sleep 30")
    }
}

/// Starts a grandchild that writes `started` at once and `marker` about two seconds later, then
/// keeps running itself.
fn with_grandchild(started: &Path, marker: &Path) -> Command {
    let (started, marker) = (started.display(), marker.display());
    if cfg!(windows) {
        shell(&format!(
            // The temp path has no spaces, so it is not quoted: cmd's nested quoting is fragile.
            "\"start \"\" /B cmd /C \"echo up>{started} & ping -n 3 127.0.0.1 >NUL & echo alive>{marker}\" & ping -n 30 127.0.0.1 >NUL\""
        ))
    } else {
        shell(&format!(
            "(touch '{started}'; sleep 2; touch '{marker}') & sleep 30"
        ))
    }
}

/// Waits until `path` exists. A fixed sleep is not enough: under parallel test load the shell can
/// take longer to start the grandchild, and stopping the child before then proves nothing.
fn wait_for(path: &Path) {
    let until = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < until, "{} never appeared", path.display());
        sleep(Duration::from_millis(20));
    }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tauri-kit-sidecar-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_crash_during_startup_is_reported_at_once() {
    let started = Instant::now();
    let mut sidecar = Sidecar::spawn(shell("exit 3"), Output::Discard).unwrap();
    let readiness = sidecar
        .wait_ready(Duration::from_secs(30), Duration::from_millis(50), || false)
        .unwrap();
    assert!(matches!(readiness, Readiness::Exited(s) if s.code() == Some(3)));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited out the deadline instead"
    );
}

#[test]
fn readiness_is_reported_when_the_probe_succeeds() {
    let mut sidecar = Sidecar::spawn(long_running(), Output::Discard).unwrap();
    let mut calls = 0;
    let readiness = sidecar
        .wait_ready(Duration::from_secs(10), Duration::from_millis(10), || {
            calls += 1;
            calls == 3
        })
        .unwrap();
    assert_eq!(readiness, Readiness::Ready);
    assert!(sidecar.is_running());
    sidecar.shutdown(Duration::ZERO).unwrap();
}

#[test]
fn waiting_stops_at_the_deadline_and_leaves_the_sidecar_running() {
    let mut sidecar = Sidecar::spawn(long_running(), Output::Discard).unwrap();
    let readiness = sidecar
        .wait_ready(
            Duration::from_millis(200),
            Duration::from_millis(20),
            || false,
        )
        .unwrap();
    assert_eq!(readiness, Readiness::TimedOut);
    assert!(sidecar.is_running());
}

#[test]
fn shutdown_stops_what_the_sidecar_started() {
    let dir = temp_dir("shutdown");
    let (started, marker) = (dir.join("started"), dir.join("marker"));
    let sidecar = Sidecar::spawn(with_grandchild(&started, &marker), Output::Discard).unwrap();
    wait_for(&started);
    sidecar.shutdown(Duration::ZERO).unwrap();
    sleep(Duration::from_secs(4));
    assert!(
        !marker.exists(),
        "a process the sidecar started outlived shutdown"
    );
}

#[test]
fn dropping_the_sidecar_stops_what_it_started() {
    let dir = temp_dir("drop");
    let (started, marker) = (dir.join("started"), dir.join("marker"));
    let sidecar = Sidecar::spawn(with_grandchild(&started, &marker), Output::Discard).unwrap();
    wait_for(&started);
    drop(sidecar);
    sleep(Duration::from_secs(4));
    assert!(
        !marker.exists(),
        "a process the sidecar started outlived the drop"
    );
}

/// The premise of the two tests above: killing only the child leaves the grandchild running. If
/// this ever stops holding, those tests would pass without proving anything.
#[test]
fn killing_only_the_child_leaves_its_grandchild_running() {
    let dir = temp_dir("premise");
    let (started, marker) = (dir.join("started"), dir.join("marker"));
    let mut cmd = with_grandchild(&started, &marker);
    tauri_kit_sidecar::hide_console(&mut cmd);
    let mut child = cmd.spawn().unwrap();
    wait_for(&started);
    child.kill().unwrap();
    child.wait().unwrap();
    let until = Instant::now() + Duration::from_secs(6);
    while !marker.exists() && Instant::now() < until {
        sleep(Duration::from_millis(100));
    }
    assert!(marker.exists());
}

#[test]
fn output_is_written_to_files() {
    let dir = temp_dir("output");
    let (stdout, stderr) = (dir.join("out.log"), dir.join("err.log"));
    let mut sidecar = Sidecar::spawn(
        shell("echo hello& echo oops 1>&2"),
        Output::Files {
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        },
    )
    .unwrap();
    sidecar
        .wait_ready(Duration::from_secs(10), Duration::from_millis(20), || false)
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    while !(read(&stdout).contains("hello") && read(&stderr).contains("oops"))
        && Instant::now() < until
    {
        sleep(Duration::from_millis(50));
    }
    assert!(read(&stdout).contains("hello"));
    assert!(read(&stderr).contains("oops"));
}

#[test]
fn a_sidecar_started_from_a_thread_outlives_that_thread() {
    // Apps start helpers from worker threads. Tying the helper's life to the spawning thread
    // (Linux PR_SET_PDEATHSIG does exactly that) would kill it as soon as the thread returns.
    let mut sidecar =
        std::thread::spawn(|| Sidecar::spawn(long_running(), Output::Discard).unwrap())
            .join()
            .unwrap();
    sleep(Duration::from_millis(500));
    assert!(
        sidecar.is_running(),
        "the sidecar died with the thread that started it"
    );
}

#[test]
fn the_exit_status_is_kept_when_it_is_read_early() {
    // Reading whether the sidecar exited must not use its status up: stopping it afterwards
    // still reports how it exited.
    let mut sidecar = Sidecar::spawn(shell("exit 3"), Output::Discard).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let early = loop {
        if let Some(status) = sidecar.try_status().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the sidecar did not exit");
        sleep(Duration::from_millis(20));
    };
    assert_eq!(early.code(), Some(3));
    assert_eq!(
        sidecar.try_status().unwrap().and_then(|s| s.code()),
        Some(3)
    );
    assert!(!sidecar.is_running());
    assert_eq!(sidecar.shutdown(Duration::ZERO).unwrap().code(), Some(3));
}

#[cfg(target_os = "linux")]
#[test]
fn an_exited_sidecar_keeps_its_pid_until_it_is_stopped() {
    // Its process-group id is how the tree is stopped; if the pid were freed as soon as the exit
    // was noticed, the system could give it to an unrelated process before the group is killed.
    let mut sidecar = Sidecar::spawn(shell("exit 0"), Output::Discard).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while sidecar.try_status().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the sidecar did not exit");
        sleep(Duration::from_millis(20));
    }
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", sidecar.id()))
        .expect("the pid was released as soon as the exit was noticed");
    let state = stat.rsplit(") ").next().unwrap().chars().next().unwrap();
    assert_eq!(
        state, 'Z',
        "expected the exited sidecar to be held unreaped"
    );
}

#[test]
fn a_readiness_line_is_returned_as_soon_as_it_is_printed() {
    let started = Instant::now();
    let line = if cfg!(windows) {
        "echo noise& echo {\"port\":51233}& ping -n 30 127.0.0.1 >NUL"
    } else {
        "echo noise; echo '{\"port\":51233}'; sleep 30"
    };
    let mut sidecar = Sidecar::spawn(shell(line), Output::Lines { stderr: None }).unwrap();
    let readiness = sidecar
        .wait_line(Duration::from_secs(20), |l| l.starts_with('{'))
        .unwrap();
    assert_eq!(
        readiness,
        LineReadiness::Line("{\"port\":51233}".to_owned())
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(sidecar.is_running());
}

#[test]
fn a_crash_before_the_readiness_line_is_reported_at_once() {
    let started = Instant::now();
    let mut sidecar = Sidecar::spawn(
        shell("echo starting& exit 4"),
        Output::Lines { stderr: None },
    )
    .unwrap();
    let readiness = sidecar
        .wait_line(Duration::from_secs(30), |l| l.starts_with('{'))
        .unwrap();
    assert!(matches!(readiness, LineReadiness::Exited(s) if s.code() == Some(4)));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited out the deadline instead"
    );
}

#[test]
fn waiting_for_a_line_stops_at_the_deadline() {
    let mut sidecar = Sidecar::spawn(long_running(), Output::Lines { stderr: None }).unwrap();
    let readiness = sidecar
        .wait_line(Duration::from_millis(300), |_| true)
        .unwrap();
    assert_eq!(readiness, LineReadiness::TimedOut);
    assert!(sidecar.is_running());
}

#[test]
fn output_after_the_readiness_line_keeps_being_drained() {
    // Far more than a pipe buffer after the line: if nobody read it, the child would block and never exit.
    let line = if cfg!(windows) {
        "echo ready& for /L %i in (1,1,20000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
    } else {
        "echo ready; i=0; while [ $i -lt 20000 ]; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i+1)); done"
    };
    let mut sidecar = Sidecar::spawn(shell(line), Output::Lines { stderr: None }).unwrap();
    let readiness = sidecar
        .wait_line(Duration::from_secs(20), |l| l == "ready")
        .unwrap();
    assert_eq!(readiness, LineReadiness::Line("ready".to_owned()));
    let until = Instant::now() + Duration::from_secs(30);
    while sidecar.is_running() && Instant::now() < until {
        sleep(Duration::from_millis(50));
    }
    assert!(!sidecar.is_running(), "the child blocked on a full pipe");
    // Closing stdout instead of draining it would unblock the child by failing its writes; on Unix
    // that kills the shell (SIGPIPE) and shows here. cmd on Windows exits 0 regardless.
    assert!(
        sidecar.try_status().unwrap().unwrap().success(),
        "the child's writes failed"
    );
}

#[test]
fn waiting_for_a_line_needs_lines_output() {
    let mut sidecar = Sidecar::spawn(long_running(), Output::Discard).unwrap();
    let error = sidecar
        .wait_line(Duration::from_millis(10), |_| true)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}
