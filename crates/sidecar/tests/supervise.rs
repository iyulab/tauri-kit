//! The supervisor against real processes, through the platform shell.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tauri_kit_sidecar::supervise::{
    Ended, Event, Health, RestartPolicy, Started, State, SuperviseOptions, Supervisor,
};
use tauri_kit_sidecar::{Output, Sidecar};

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

/// Exits with 3 at once.
fn exits() -> Command {
    shell("exit 3")
}

/// Writes `marker` about two seconds after it starts, then keeps running — unless it is stopped.
fn marks_later(marker: &Path) -> Command {
    let marker = marker.display();
    if cfg!(windows) {
        // The temp path has no spaces, so it is not quoted: cmd's nested quoting is fragile.
        shell(&format!(
            "ping -n 3 127.0.0.1 >NUL & echo alive>{marker} & ping -n 30 127.0.0.1 >NUL"
        ))
    } else {
        shell(&format!("sleep 2; touch '{marker}'; sleep 30"))
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("tauri-kit-supervise-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What an event said, owned, to be checked on the test's thread.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Restarted {
        ended: Ended,
        attempt: u32,
        before: u32,
        after: u32,
    },
    RestartFailed {
        attempt: u32,
    },
    Down {
        attempts: u32,
    },
}

fn recorder() -> (
    impl for<'a> FnMut(Event<'a, u32>) + Send + 'static,
    Receiver<Seen>,
) {
    let (sender, receiver) = mpsc::channel();
    let handler = move |event: Event<'_, u32>| {
        let seen = match event {
            Event::Restarted {
                ended,
                attempt,
                before,
                after,
            } => Seen::Restarted {
                ended,
                attempt,
                before: *before,
                after: *after,
            },
            Event::RestartFailed { attempt, .. } => Seen::RestartFailed { attempt },
            Event::Down { attempts, .. } => Seen::Down { attempts },
        };
        let _ = sender.send(seen);
    };
    (handler, receiver)
}

fn next(events: &Receiver<Seen>) -> Seen {
    events
        .recv_timeout(Duration::from_secs(20))
        .expect("an event in time")
}

fn quick(restart: RestartPolicy) -> SuperviseOptions<u32> {
    SuperviseOptions {
        check_every: Duration::from_millis(50),
        restart,
        health: None,
    }
}

/// A launcher that numbers its starts (0, 1, …) and starts `command(n)` for start `n`.
fn numbered(
    command: impl Fn(u32) -> Command + Send + Sync + 'static,
) -> (
    impl Fn() -> Result<Started<u32>, std::io::Error> + Send + Sync + 'static,
    Arc<AtomicU32>,
) {
    let count = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&count);
    let launch = move || {
        let n = counted.fetch_add(1, Ordering::SeqCst);
        Ok(Started {
            sidecar: Sidecar::spawn(command(n), Output::Discard)?,
            value: n,
        })
    };
    (launch, count)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < until, "{what} never happened");
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_sidecar_that_exits_is_started_again_and_the_new_value_replaces_the_old() {
    let (launch, _) = numbered(|n| if n == 0 { exits() } else { long_running() });
    let (handler, events) = recorder();
    let supervisor = Supervisor::start(launch, quick(RestartPolicy::default()), handler).unwrap();
    match next(&events) {
        Seen::Restarted {
            ended: Ended::Exited(status),
            attempt: 1,
            before: 0,
            after: 1,
        } => assert_eq!(status.code(), Some(3)),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(supervisor.current(), 1);
    assert_eq!(supervisor.state(), State::Running);
}

#[test]
fn once_the_policy_is_used_up_the_sidecar_stays_down() {
    let (launch, count) = numbered(|_| exits());
    let (handler, events) = recorder();
    let policy = RestartPolicy {
        limit: 2,
        window: Duration::from_secs(60),
    };
    let supervisor = Supervisor::start(launch, quick(policy), handler).unwrap();
    assert!(matches!(next(&events), Seen::Restarted { after: 1, .. }));
    assert!(matches!(next(&events), Seen::Restarted { after: 2, .. }));
    assert_eq!(next(&events), Seen::Down { attempts: 0 });
    assert_eq!(supervisor.state(), State::Down);
    sleep(Duration::from_millis(300));
    assert_eq!(
        count.load(Ordering::SeqCst),
        3,
        "started once more than allowed"
    );
}

#[test]
fn a_policy_of_never_leaves_it_down_until_the_app_restarts_it() {
    let (launch, count) = numbered(|n| if n == 0 { exits() } else { long_running() });
    let (handler, events) = recorder();
    let supervisor = Supervisor::start(launch, quick(RestartPolicy::NEVER), handler).unwrap();
    assert_eq!(next(&events), Seen::Down { attempts: 0 });
    assert_eq!(count.load(Ordering::SeqCst), 1);

    supervisor.restart(Duration::ZERO).unwrap();
    assert_eq!(supervisor.state(), State::Running);
    assert_eq!(supervisor.current(), 1);
}

#[test]
fn starts_that_fail_count_against_the_policy_and_are_reported() {
    let count = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&count);
    let launch = move || {
        let n = counted.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(Started {
                sidecar: Sidecar::spawn(exits(), Output::Discard)?,
                value: n,
            })
        } else {
            Err(std::io::Error::other("cannot start"))
        }
    };
    let (handler, events) = recorder();
    let policy = RestartPolicy {
        limit: 2,
        window: Duration::from_secs(60),
    };
    let supervisor = Supervisor::start(launch, quick(policy), handler).unwrap();
    assert_eq!(next(&events), Seen::RestartFailed { attempt: 1 });
    assert_eq!(next(&events), Seen::RestartFailed { attempt: 2 });
    assert_eq!(next(&events), Seen::Down { attempts: 2 });
    assert_eq!(supervisor.state(), State::Down);
    assert_eq!(supervisor.current(), 0, "the last value stays");
}

#[test]
fn a_sidecar_that_stops_answering_is_stopped_and_started_again() {
    let dir = temp_dir("unresponsive");
    let first_marker = dir.join("first");
    let marker = first_marker.clone();
    let (launch, _) = numbered(move |n| {
        if n == 0 {
            marks_later(&marker)
        } else {
            long_running()
        }
    });
    let (handler, events) = recorder();
    let options = SuperviseOptions {
        health: Some(Health::new(Duration::from_millis(50), 2, |value: &u32| {
            *value != 0
        })),
        ..quick(RestartPolicy::default())
    };
    let supervisor = Supervisor::start(launch, options, handler).unwrap();
    assert_eq!(
        next(&events),
        Seen::Restarted {
            ended: Ended::Unresponsive,
            attempt: 1,
            before: 0,
            after: 1
        }
    );
    sleep(Duration::from_secs(3));
    assert!(
        !first_marker.exists(),
        "the unresponsive sidecar was left running"
    );
    assert_eq!(supervisor.state(), State::Running);
}

#[test]
fn a_stop_during_a_slow_start_undoes_that_start() {
    let dir = temp_dir("slow-start");
    let late_marker = dir.join("late");
    let marker = late_marker.clone();
    let starting = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&starting);
    let count = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&count);
    let launch = move || {
        let n = counted.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            return Ok::<_, std::io::Error>(Started {
                sidecar: Sidecar::spawn(exits(), Output::Discard)?,
                value: n,
            });
        }
        let sidecar = Sidecar::spawn(marks_later(&marker), Output::Discard)?;
        *flag.lock().unwrap() = true;
        sleep(Duration::from_millis(800)); // a start that takes its time to be ready
        Ok(Started { sidecar, value: n })
    };
    let (handler, _events) = recorder();
    let supervisor = Supervisor::start(launch, quick(RestartPolicy::default()), handler).unwrap();
    wait_until("the second start", || *starting.lock().unwrap());

    let asked = Instant::now();
    supervisor.stop(Duration::ZERO, |_| {});
    assert!(
        asked.elapsed() < Duration::from_millis(500),
        "stop waited for the start"
    );
    assert_eq!(supervisor.state(), State::Stopped);

    sleep(Duration::from_secs(3));
    assert!(
        !late_marker.exists(),
        "the start under way was left running after stop"
    );
    assert_eq!(supervisor.state(), State::Stopped);
    assert_eq!(count.load(Ordering::SeqCst), 2, "started again after stop");
}

#[test]
fn stop_asks_with_the_running_value_and_happens_once() {
    let (launch, count) = numbered(|_| long_running());
    let (handler, _events) = recorder();
    let supervisor = Supervisor::start(launch, quick(RestartPolicy::default()), handler).unwrap();
    let asked = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&asked);
    let ended = supervisor.stop(Duration::from_millis(100), move |value| {
        *seen.lock().unwrap() = Some(*value)
    });
    assert!(matches!(ended, Some(Ok(_))));
    assert_eq!(*asked.lock().unwrap(), Some(0));
    assert!(supervisor.stop(Duration::ZERO, |_| {}).is_none());
    assert!(supervisor.restart(Duration::ZERO).is_err());
    sleep(Duration::from_millis(300));
    assert_eq!(count.load(Ordering::SeqCst), 1, "started again after stop");
}

#[test]
fn a_first_start_that_fails_is_returned_as_it_is() {
    let failed = Supervisor::<u32>::start(
        || Err::<Started<u32>, _>(std::io::Error::other("no such helper")),
        SuperviseOptions::default(),
        |_| {},
    );
    assert_eq!(failed.err().unwrap().to_string(), "no such helper");
}
