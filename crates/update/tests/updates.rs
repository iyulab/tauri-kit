//! The orchestration against a scripted source.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
use tauri_kit_update::{Event, Found, Looked, Source, Stage, UpdateOptions, Updates};

/// What the source was asked, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Check,
    Download(String),
    Install { version: String, restart: bool },
    Restore(String),
    Forget(String),
}

#[derive(Default)]
struct Script {
    /// What each check answers, in turn; once empty, the last answer again.
    checks: VecDeque<Result<Option<String>, String>>,
    last: Option<Result<Option<String>, String>>,
    /// Versions whose download fails.
    failing: Vec<String>,
    /// Versions kept from an earlier run.
    kept: Vec<String>,
    calls: Vec<Call>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Script>>);

impl Fake {
    fn answering(answers: &[Result<Option<&str>, &str>]) -> Fake {
        let fake = Fake::default();
        fake.0.lock().unwrap().checks = answers
            .iter()
            .map(|a| a.map(|v| v.map(str::to_string)).map_err(str::to_string))
            .collect();
        fake
    }

    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().calls.clone()
    }

    fn count(&self, call: &Call) -> usize {
        self.calls().iter().filter(|c| *c == call).count()
    }

    fn record(&self, call: Call) {
        self.0.lock().unwrap().calls.push(call);
    }
}

impl Source for Fake {
    type Update = ();
    type Installer = String;

    fn check(&self) -> Result<Option<Found<()>>, String> {
        self.record(Call::Check);
        let mut script = self.0.lock().unwrap();
        let answer = match script.checks.pop_front() {
            Some(answer) => {
                script.last = Some(answer.clone());
                answer
            }
            None => script.last.clone().unwrap_or(Ok(None)),
        };
        answer.map(|v| {
            v.map(|version| Found {
                version,
                update: (),
            })
        })
    }

    fn download(&self, found: &Found<()>) -> Result<String, String> {
        self.record(Call::Download(found.version.clone()));
        if self.0.lock().unwrap().failing.contains(&found.version) {
            return Err("does not verify".into());
        }
        Ok(format!("installer {}", found.version))
    }

    fn install(&self, found: Found<()>, installer: String, restart: bool) -> Result<(), String> {
        assert_eq!(installer, format!("installer {}", found.version));
        self.record(Call::Install {
            version: found.version,
            restart,
        });
        Ok(())
    }

    fn restore(&self, version: &str) -> Option<(Found<()>, String)> {
        self.record(Call::Restore(version.into()));
        let kept = self.0.lock().unwrap().kept.contains(&version.to_string());
        kept.then(|| {
            (
                Found {
                    version: version.into(),
                    update: (),
                },
                format!("installer {version}"),
            )
        })
    }

    fn forget(&self, version: &str) {
        self.record(Call::Forget(version.into()));
    }
}

/// What an event said, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Found(String, bool),
    Ready(String),
    Failed(Stage, Option<String>),
}

fn recorder() -> (
    impl for<'a> FnMut(Event<'a>) + Send + 'static,
    Receiver<Seen>,
) {
    let (sender, receiver) = mpsc::channel();
    let handler = move |event: Event<'_>| {
        let _ = sender.send(match event {
            Event::Found {
                version,
                first_time,
            } => Seen::Found(version.into(), first_time),
            Event::Ready { version } => Seen::Ready(version.into()),
            Event::Failed { stage, version, .. } => Seen::Failed(stage, version.map(Into::into)),
        });
    };
    (handler, receiver)
}

fn next(events: &Receiver<Seen>) -> Seen {
    events
        .recv_timeout(Duration::from_secs(10))
        .expect("an event in time")
}

fn options(state: Option<&Path>) -> UpdateOptions {
    UpdateOptions {
        state_file: state.map(Path::to_path_buf),
        ..UpdateOptions::new("1.1.0")
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        sleep(Duration::from_millis(20));
    }
    panic!("{what} never happened");
}

#[test]
fn a_newer_release_is_announced_downloaded_and_ready_to_install_on_close() {
    let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
    let (handler, events) = recorder();
    let updates = Updates::start(fake.clone(), options(None), handler);

    assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
    assert_eq!(next(&events), Seen::Ready("1.2.0".into()));
    let status = updates.status();
    assert_eq!(status.available.as_deref(), Some("1.2.0"));
    assert_eq!(status.ready.as_deref(), Some("1.2.0"));

    let order = Arc::new(Mutex::new(Vec::new()));
    let first = Arc::clone(&order);
    assert!(updates.install_on_close(move || first.lock().unwrap().push("before")));
    order.lock().unwrap().push("after");
    assert_eq!(*order.lock().unwrap(), ["before", "after"]);
    assert_eq!(
        fake.calls().last(),
        Some(&Call::Install {
            version: "1.2.0".into(),
            restart: false
        })
    );
    assert!(
        !updates.install_on_close(|| panic!("nothing is left to install")),
        "installed once"
    );
}

#[test]
fn a_release_that_is_not_newer_is_not_offered() {
    let fake = Fake::answering(&[Ok(Some("1.1.0"))]);
    let (handler, events) = recorder();
    let updates = Updates::start(fake.clone(), options(None), handler);
    wait_until("the first look", || fake.count(&Call::Check) == 1);
    assert_eq!(updates.check_now(), Looked::Latest);
    assert!(events.recv_timeout(Duration::from_millis(200)).is_err());
    assert_eq!(updates.status().available, None);
    // Up to date, and known to be: the source answered.
    assert!(updates.status().last_answered.is_some());
}

#[test]
fn a_version_is_announced_once_across_launches_and_the_schedule_survives_them() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("update.json");
    {
        let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
        let (handler, events) = recorder();
        let _updates = Updates::start(fake, options(Some(&state)), handler);
        assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
        assert_eq!(next(&events), Seen::Ready("1.2.0".into()));
    }
    // The next launch: the look is not due yet, so it does not ask on its own.
    let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
    let (handler, events) = recorder();
    let updates = Updates::start(fake.clone(), options(Some(&state)), handler);
    sleep(Duration::from_millis(300));
    assert_eq!(
        fake.count(&Call::Check),
        0,
        "asked although a look was not due"
    );
    assert!(updates.status().last_checked.is_some());
    assert!(
        updates.status().last_answered.is_some(),
        "the answer is kept across launches"
    );

    assert_eq!(updates.check_now(), Looked::Found("1.2.0".into()));
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), false));
}

#[test]
fn an_installer_kept_from_an_earlier_launch_is_used_without_downloading_again() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("update.json");
    {
        let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
        let (handler, events) = recorder();
        let _updates = Updates::start(fake, options(Some(&state)), handler);
        assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
        assert_eq!(next(&events), Seen::Ready("1.2.0".into()));
    }
    let fake = Fake::default();
    fake.0.lock().unwrap().kept.push("1.2.0".into());
    let (handler, _events) = recorder();
    let updates = Updates::start(fake.clone(), options(Some(&state)), handler);
    assert_eq!(updates.status().ready.as_deref(), Some("1.2.0"));
    assert_eq!(fake.calls(), [Call::Restore("1.2.0".into())]);

    updates.install_now(|| {}).unwrap();
    assert_eq!(
        fake.calls().last(),
        Some(&Call::Install {
            version: "1.2.0".into(),
            restart: true
        })
    );
    assert_eq!(fake.count(&Call::Download("1.2.0".into())), 0);
}

#[test]
fn an_installer_for_a_version_already_installed_is_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("update.json");
    {
        let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
        let (handler, events) = recorder();
        let _updates = Updates::start(fake, options(Some(&state)), handler);
        assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
        assert_eq!(next(&events), Seen::Ready("1.2.0".into()));
    }
    // The installer ran: this launch is 1.2.0.
    let fake = Fake::default();
    let (handler, _events) = recorder();
    let updates = Updates::start(
        fake.clone(),
        UpdateOptions {
            state_file: Some(state),
            ..UpdateOptions::new("1.2.0")
        },
        handler,
    );
    assert_eq!(updates.status().ready, None);
    assert_eq!(fake.calls(), [Call::Forget("1.2.0".into())]);
}

#[test]
fn a_download_that_fails_is_not_tried_again_for_the_same_version_but_is_for_a_newer_one() {
    let fake = Fake::answering(&[Ok(Some("1.2.0")), Ok(Some("1.2.0")), Ok(Some("1.3.0"))]);
    fake.0.lock().unwrap().failing.push("1.2.0".into());
    let (handler, events) = recorder();
    let updates = Updates::start(fake.clone(), options(None), handler);
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
    assert_eq!(
        next(&events),
        Seen::Failed(Stage::Download, Some("1.2.0".into()))
    );
    assert_eq!(updates.status().ready, None);
    assert_eq!(updates.status().failed.as_deref(), Some("1.2.0"));

    // Looking again — as an app may at every launch — does not download the same version again.
    fake.0.lock().unwrap().failing.clear();
    assert_eq!(updates.check_now(), Looked::Found("1.2.0".into()));
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), false));
    sleep(Duration::from_millis(300));
    assert_eq!(fake.count(&Call::Download("1.2.0".into())), 1);
    assert_eq!(updates.status().failed.as_deref(), Some("1.2.0"));

    // A newer release is downloaded.
    assert_eq!(updates.check_now(), Looked::Found("1.3.0".into()));
    assert_eq!(next(&events), Seen::Found("1.3.0".into(), true));
    assert_eq!(next(&events), Seen::Ready("1.3.0".into()));
    assert_eq!(updates.status().failed, None);
}

#[test]
fn a_download_that_failed_is_tried_again_once_every_has_passed() {
    let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
    fake.0.lock().unwrap().failing.push("1.2.0".into());
    let (handler, events) = recorder();
    let updates = Updates::start(
        fake.clone(),
        UpdateOptions {
            every: Duration::ZERO,
            enabled: false,
            ..UpdateOptions::new("1.1.0")
        },
        handler,
    );
    assert_eq!(updates.check_now(), Looked::Found("1.2.0".into()));
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
    assert!(matches!(next(&events), Seen::Failed(Stage::Download, _)));

    fake.0.lock().unwrap().failing.clear();
    assert_eq!(updates.check_now(), Looked::Found("1.2.0".into()));
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), false));
    assert_eq!(next(&events), Seen::Ready("1.2.0".into()));
    assert_eq!(fake.count(&Call::Download("1.2.0".into())), 2);
}

#[test]
fn a_failed_download_is_still_reported_after_a_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("update.json");
    let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
    fake.0.lock().unwrap().failing.push("1.2.0".into());
    let (handler, events) = recorder();
    let first = Updates::start(fake.clone(), options(Some(&state)), handler);
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
    assert!(matches!(next(&events), Seen::Failed(Stage::Download, _)));
    drop(first);

    // Not due yet: the next launch does not look, and still says which download failed.
    let (handler, _events) = recorder();
    let again = Updates::start(fake, options(Some(&state)), handler);
    assert_eq!(again.status().failed.as_deref(), Some("1.2.0"));
}

#[test]
fn an_unreachable_source_is_reported_and_changes_nothing() {
    let fake = Fake::answering(&[Err("offline")]);
    let (handler, events) = recorder();
    let updates = Updates::start(fake, options(None), handler);
    assert_eq!(next(&events), Seen::Failed(Stage::Check, None));
    assert_eq!(updates.check_now(), Looked::Unreachable("offline".into()));
    assert_eq!(updates.status().available, None);
    // Looked, but never answered: not «up to date».
    let status = updates.status();
    assert!(status.last_checked.is_some() && status.last_answered.is_none());
}

#[test]
fn turned_off_it_does_not_look_on_its_own_and_lets_go_of_what_it_found() {
    let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
    let (handler, events) = recorder();
    let updates = Updates::start(
        fake.clone(),
        UpdateOptions {
            enabled: false,
            ..options(None)
        },
        handler,
    );
    sleep(Duration::from_millis(300));
    assert_eq!(fake.count(&Call::Check), 0, "looked although turned off");

    // A look by hand still works, and its release is downloaded.
    assert_eq!(updates.check_now(), Looked::Found("1.2.0".into()));
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
    assert_eq!(next(&events), Seen::Ready("1.2.0".into()));

    updates.set_enabled(false);
    let status = updates.status();
    assert_eq!((status.available, status.ready), (None, None));
    assert_eq!(fake.count(&Call::Forget("1.2.0".into())), 1);
}

#[test]
fn installing_now_downloads_when_nothing_is_ready_and_asks_for_a_restart() {
    let fake = Fake::answering(&[Ok(Some("1.2.0"))]);
    fake.0.lock().unwrap().failing.push("1.2.0".into());
    let (handler, events) = recorder();
    let updates = Updates::start(fake.clone(), options(None), handler);
    assert_eq!(next(&events), Seen::Found("1.2.0".into(), true));
    assert!(matches!(next(&events), Seen::Failed(Stage::Download, _)));

    assert!(
        updates.install_now(|| {}).is_err(),
        "a download that does not verify is not installed"
    );
    fake.0.lock().unwrap().failing.clear();
    let mut stopped = false;
    updates.install_now(|| stopped = true).unwrap();
    assert!(stopped, "before_install ran");
    assert_eq!(
        fake.calls().last(),
        Some(&Call::Install {
            version: "1.2.0".into(),
            restart: true
        })
    );
}

#[test]
fn installing_now_with_nothing_found_fails() {
    let fake = Fake::answering(&[Ok(None)]);
    let (handler, _events) = recorder();
    let updates = Updates::start(fake, options(None), handler);
    assert!(updates
        .install_now(|| panic!("nothing to install"))
        .is_err());
}
