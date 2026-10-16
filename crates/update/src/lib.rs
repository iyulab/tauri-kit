//! Keep an installed desktop app up to date.
//!
//! Looking for a newer release is one request. Doing it well is a set of quiet decisions every app
//! makes again:
//!
//! - **When to look.** At launch and on a schedule — but a schedule kept in memory starts over at
//!   every launch, so an app opened ten times a day asks ten times. [`Updates`] keeps when it last
//!   looked in a small state file and looks when that is long enough ago, or when the clock went
//!   backwards.
//! - **What to say.** A newer version is announced once ([`Event::Found`] with `first_time`), not
//!   at every look.
//! - **When to download.** In the background, once per version; a download that failed is not
//!   tried again until the next look, so a broken release does not cost a download at every launch.
//! - **When to install.** When the app closes ([`Updates::install_on_close`]), or now when the
//!   person asks ([`Updates::install_now`]). Either way `before_install` runs first: a helper the
//!   app started holds its own program open, and the installer must replace it — stop it there.
//! - **What is left over.** An installer kept from an earlier run is used again
//!   ([`Source::restore`]); one for a version already installed is let go ([`Source::forget`]).
//!
//! Where releases come from, how a download is checked and how an installer runs belong to the
//! [`Source`]: Tauri's updater plugin (signed `latest.json`), or an app's own (a release page and an
//! Authenticode check, say). The source's methods block and run on the supervisor's thread, never
//! on the caller's — wrap an async API with the runtime's `block_on` there.
//!
//! Turning looking off, and remembering that, is the app's: it passes [`UpdateOptions::enabled`]
//! and calls [`Updates::set_enabled`]. A look by hand ([`Updates::check_now`]) works either way.
//!
//! On Windows the installer also has to wait for the closing app and its helpers to let go of their
//! files. The crate's `nsis/wait-for-exit.nsh` is an installer hook that does — see that file.
//!
//! # Tauri's updater plugin as the source
//!
//! The crate does not depend on Tauri. With `tauri-plugin-updater`, the source is a few lines in the
//! app — the plugin checks `latest.json` and the installer against the updater key:
//!
//! ```ignore
//! use tauri_plugin_updater::{Update, UpdaterExt};
//! use tauri_kit_update::{Found, Source};
//!
//! struct Plugin(tauri::AppHandle);
//!
//! impl Source for Plugin {
//!     type Update = Update;
//!     type Installer = Vec<u8>;
//!
//!     fn check(&self) -> Result<Option<Found<Update>>, String> {
//!         let updater = self.0.updater().map_err(|e| e.to_string())?;
//!         let update = tauri::async_runtime::block_on(updater.check()).map_err(|e| e.to_string())?;
//!         Ok(update.map(|u| Found { version: u.version.clone(), update: u }))
//!     }
//!
//!     fn download(&self, found: &Found<Update>) -> Result<Vec<u8>, String> {
//!         tauri::async_runtime::block_on(found.update.download(|_, _| {}, || {})).map_err(|e| e.to_string())
//!     }
//!
//!     fn install(&self, found: Found<Update>, installer: Vec<u8>, restart: bool) -> Result<(), String> {
//!         let installed = found.update.install(installer).map_err(|e| e.to_string());
//!         if restart && installed.is_ok() {
//!             self.0.restart();
//!         }
//!         installed
//!     }
//! }
//! ```
//!
//! `block_on` is safe there because the methods run on this crate's own thread, never on the
//! runtime's. A source with its own installer files implements [`Source::restore`] and
//! [`Source::forget`] too.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A newer release a [`Source`] found: its version, and whatever the source needs to download and
/// install it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<U> {
    /// The release's version, `major.minor.patch`.
    pub version: String,
    /// The source's own handle on it.
    pub update: U,
}

/// Where releases come from. Each method blocks; [`Updates`] calls them on its own thread.
pub trait Source: Send + Sync + 'static {
    /// The source's handle on a release it found.
    type Update: Clone + Send + 'static;
    /// A downloaded, checked installer: its bytes, or the file it was saved to.
    type Installer: Send + 'static;

    /// The newest release, or `None` when there is none to offer. A release that is not newer
    /// than the running version is ignored, so a source may simply report the latest one.
    fn check(&self) -> Result<Option<Found<Self::Update>>, String>;

    /// Downloads the release and checks it — against the updater's key, a publisher's signature.
    /// An installer that does not check out is an error, never an installer.
    fn download(&self, found: &Found<Self::Update>) -> Result<Self::Installer, String>;

    /// Runs the installer. `restart` asks it to start the new version afterwards (installing now);
    /// without it, as the app closes, it only installs. On success this usually does not return:
    /// the installer ends the app.
    fn install(
        &self,
        found: Found<Self::Update>,
        installer: Self::Installer,
        restart: bool,
    ) -> Result<(), String>;

    /// An installer for `version` kept from an earlier run — a file downloaded and checked
    /// before — to install without downloading again. Default none: a source that keeps installers
    /// in memory downloads again after a restart.
    fn restore(&self, version: &str) -> Option<(Found<Self::Update>, Self::Installer)> {
        let _ = version;
        None
    }

    /// The installer kept for `version` is no longer wanted — that version is installed, or a
    /// newer one replaced it, or looking was turned off. Default nothing.
    fn forget(&self, version: &str) {
        let _ = version;
    }
}

/// How [`Updates`] looks and keeps what it found.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// The running version, `major.minor.patch`.
    pub current: String,
    /// How long after one look the next is due. Default twelve hours.
    pub every: Duration,
    /// Where when it last looked, the version it announced and the installer it keeps are
    /// remembered across launches. Without it, every launch looks at once.
    pub state_file: Option<PathBuf>,
    /// Whether it looks on its own. Default on.
    pub enabled: bool,
}

impl UpdateOptions {
    /// Options for the running version `current`, with the defaults.
    pub fn new(current: impl Into<String>) -> Self {
        UpdateOptions {
            current: current.into(),
            every: Duration::from_secs(12 * 60 * 60),
            state_file: None,
            enabled: true,
        }
    }
}

/// Which step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Asking for the newest release.
    Check,
    /// Downloading or checking an installer.
    Download,
}

/// What [`Updates`] did. Reported on its own thread with none of its locks held — from inside the
/// call that reports it, so a handler must not call back into [`Updates`]; hand the work to another
/// thread instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// A newer release was found. `first_time` is true the first time this version is seen, across
    /// launches when there is a state file — the moment to tell the person.
    Found {
        /// Its version.
        version: &'a str,
        /// Seen for the first time.
        first_time: bool,
    },
    /// Its installer is downloaded and checked: it is installed when the app closes.
    Ready {
        /// Its version.
        version: &'a str,
    },
    /// A look or a download failed. No network is the common case and not the app's fault; the
    /// next look tries again.
    Failed {
        /// Which step.
        stage: Stage,
        /// The version being downloaded, for a download.
        version: Option<&'a str>,
        /// Why.
        error: &'a str,
    },
}

/// What one look found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Looked {
    /// No newer release than the running one.
    Latest,
    /// A newer release; its installer is being downloaded, or already is.
    Found(String),
    /// The source did not answer.
    Unreachable(String),
}

/// Where updating stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// It looks on its own.
    pub enabled: bool,
    /// The newer release found, if any.
    pub available: Option<String>,
    /// The version whose installer is downloaded and checked, if any.
    pub ready: Option<String>,
    /// An installer is being downloaded.
    pub downloading: bool,
    /// When it last looked, as seconds since the Unix epoch.
    pub last_checked: Option<u64>,
}

/// Whether `candidate` is a newer `major.minor.patch` than `current`. A version that is not of that
/// form — a pre-release, say — is never newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (triple(candidate), triple(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn triple(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let mut parts = version.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}

const FORMAT: &str = "tauri-kit-update/1";

/// What survives a launch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    format: String,
    checked_at: Option<u64>,
    announced: Option<String>,
    ready: Option<String>,
    failed: Option<String>,
}

type Handler = Box<dyn for<'a> FnMut(Event<'a>) + Send>;
type Kept<S> = (Found<<S as Source>::Update>, <S as Source>::Installer);

struct Inner<S: Source> {
    source: S,
    current: String,
    every: Duration,
    state_file: Option<PathBuf>,
    enabled: AtomicBool,
    stopped: AtomicBool,
    downloading: AtomicBool,
    saved: Mutex<Saved>,
    found: Mutex<Option<Found<S::Update>>>,
    ready: Mutex<Option<Kept<S>>>,
    wake: (Mutex<()>, Condvar),
    handler: Mutex<Handler>,
}

/// Keeps an app up to date from a [`Source`]. See the [crate documentation](crate).
///
/// Dropping it ends its thread; nothing is installed.
pub struct Updates<S: Source> {
    inner: Arc<Inner<S>>,
}

fn lock<V>(mutex: &Mutex<V>) -> MutexGuard<'_, V> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl<S: Source> Updates<S> {
    /// Reads what an earlier launch kept, lets go of an installer for a version that is no longer
    /// newer, takes back one that still is, and starts looking on its own thread — at once when a
    /// look is due.
    pub fn start(
        source: S,
        options: UpdateOptions,
        on_event: impl for<'a> FnMut(Event<'a>) + Send + 'static,
    ) -> Self {
        let mut saved = options
            .state_file
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
            .filter(|saved| saved.format == FORMAT)
            .unwrap_or_default();
        let mut ready = None;
        if let Some(version) = saved.ready.clone() {
            if is_newer(&version, &options.current) {
                ready = source.restore(&version);
            } else {
                source.forget(&version);
            }
            if ready.is_none() {
                saved.ready = None;
            }
        }
        let found = ready.as_ref().map(|(found, _): &Kept<S>| found.clone());
        let inner = Arc::new(Inner {
            source,
            current: options.current,
            every: options.every,
            state_file: options.state_file,
            enabled: AtomicBool::new(options.enabled),
            stopped: AtomicBool::new(false),
            downloading: AtomicBool::new(false),
            saved: Mutex::new(saved),
            found: Mutex::new(found),
            ready: Mutex::new(ready),
            wake: (Mutex::new(()), Condvar::new()),
            handler: Mutex::new(Box::new(on_event)),
        });
        inner.save();
        let watcher = Arc::clone(&inner);
        thread::spawn(move || watcher.run());
        Updates { inner }
    }

    /// Where updating stands.
    pub fn status(&self) -> Status {
        let inner = &self.inner;
        Status {
            enabled: inner.enabled.load(Ordering::SeqCst),
            available: lock(&inner.found).as_ref().map(|f| f.version.clone()),
            ready: lock(&inner.ready).as_ref().map(|(f, _)| f.version.clone()),
            downloading: inner.downloading.load(Ordering::SeqCst),
            last_checked: lock(&inner.saved).checked_at,
        }
    }

    /// Turns looking on its own on or off. Off lets go of a release found and its installer; on
    /// looks at once when a look is due. Remembering the choice is the app's.
    pub fn set_enabled(&self, on: bool) {
        let inner = &self.inner;
        inner.enabled.store(on, Ordering::SeqCst);
        if !on {
            *lock(&inner.found) = None;
            let kept = lock(&inner.ready).take();
            if let Some((found, _)) = kept {
                inner.source.forget(&found.version);
            }
            lock(&inner.saved).ready = None;
            inner.save();
        }
        inner.wake.1.notify_all();
    }

    /// Looks now, whether or not it looks on its own — the person asked. A newer release is
    /// downloaded in the background, as when it is found on its own. Blocks for the look.
    pub fn check_now(&self) -> Looked {
        let looked = self.inner.look();
        if matches!(looked, Looked::Found(_)) {
            let inner = Arc::clone(&self.inner);
            thread::spawn(move || inner.fetch());
        }
        looked
    }

    /// Installs the release found now: its installer — downloaded now when it is not ready yet
    /// (this blocks) — then `before_install`, then the installer, asked to start the new version.
    /// Returns only when it could not: nothing found, or the download or the installer failed.
    pub fn install_now(&self, before_install: impl FnOnce()) -> Result<(), String> {
        let inner = &self.inner;
        let kept = lock(&inner.ready).take();
        let (found, installer) = match kept {
            Some(kept) => kept,
            None => {
                let found = lock(&inner.found)
                    .clone()
                    .ok_or_else(|| "no newer version found".to_string())?;
                let installer = inner.source.download(&found)?;
                (found, installer)
            }
        };
        before_install();
        inner.source.install(found, installer, true)
    }

    /// As the app closes: when an installer is ready, `before_install`, then the installer, which
    /// only installs — the person closed the app. Returns whether there was one to install.
    pub fn install_on_close(&self, before_install: impl FnOnce()) -> bool {
        let kept = lock(&self.inner.ready).take();
        let Some((found, installer)) = kept else {
            return false;
        };
        before_install();
        let _ = self.inner.source.install(found, installer, false);
        true
    }
}

impl<S: Source> Drop for Updates<S> {
    fn drop(&mut self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.wake.1.notify_all();
    }
}

impl<S: Source> Inner<S> {
    fn emit(&self, event: Event<'_>) {
        (lock(&self.handler))(event);
    }

    /// Writes what survives a launch. The last writer wins: an app runs one instance, and two
    /// writing at once would cost at most one look too many or one announcement repeated.
    fn save(&self) {
        let Some(path) = &self.state_file else { return };
        let mut saved = lock(&self.saved).clone();
        saved.format = FORMAT.into();
        if let Ok(bytes) = serde_json::to_vec_pretty(&saved) {
            // Losing it costs one early look; never worth stopping the app over.
            let _ = tauri_kit_fs::write_atomic(path, &bytes);
        }
    }

    /// How long until the next look is due; zero when it is due now.
    fn due_in(&self) -> Duration {
        let Some(checked) = lock(&self.saved).checked_at else {
            return Duration::ZERO;
        };
        let now = now();
        if now < checked {
            return Duration::ZERO; // the clock went backwards
        }
        self.every
            .saturating_sub(Duration::from_secs(now - checked))
    }

    fn run(&self) {
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return;
            }
            let enabled = self.enabled.load(Ordering::SeqCst);
            if enabled && self.due_in().is_zero() {
                self.look();
                self.fetch();
            }
            // Asleep until the next look, or until set_enabled or a drop wakes it. A clock moved
            // forward or a laptop that slept is caught at the next wake, at most an hour on.
            let wait = if enabled {
                self.due_in()
                    .clamp(Duration::from_secs(1), Duration::from_secs(3600))
            } else {
                Duration::from_secs(3600)
            };
            let guard = lock(&self.wake.0);
            let _ = self.wake.1.wait_timeout(guard, wait);
        }
    }

    fn look(&self) -> Looked {
        let answer = self.source.check();
        let found = match answer {
            Err(error) => {
                lock(&self.saved).checked_at = Some(now());
                self.save();
                self.emit(Event::Failed {
                    stage: Stage::Check,
                    version: None,
                    error: &error,
                });
                return Looked::Unreachable(error);
            }
            Ok(found) => found.filter(|f| is_newer(&f.version, &self.current)),
        };
        let first_time = {
            let mut saved = lock(&self.saved);
            saved.checked_at = Some(now());
            // A new look gives a download that failed another chance.
            saved.failed = None;
            match &found {
                Some(found) => {
                    let first = saved.announced.as_deref() != Some(found.version.as_str());
                    saved.announced = Some(found.version.clone());
                    first
                }
                None => false,
            }
        };
        let Some(found) = found else {
            self.save();
            return Looked::Latest;
        };
        let version = found.version.clone();
        let replaced = {
            let mut current = lock(&self.found);
            if current.as_ref().is_some_and(|f| f.version == version) {
                None
            } else {
                *current = Some(found);
                let kept = lock(&self.ready).take();
                kept.map(|(old, _)| old.version)
            }
        };
        if let Some(old) = replaced {
            self.source.forget(&old);
            lock(&self.saved).ready = None;
        }
        self.save();
        self.emit(Event::Found {
            version: &version,
            first_time,
        });
        Looked::Found(version)
    }

    /// Downloads the installer of the release found, unless it is ready, failed since the last
    /// look, or another download is under way.
    fn fetch(&self) {
        if self.downloading.swap(true, Ordering::SeqCst) {
            return;
        }
        let target = {
            let found = lock(&self.found).clone();
            let ready = lock(&self.ready).as_ref().map(|(f, _)| f.version.clone());
            let failed = lock(&self.saved).failed.clone();
            found.filter(|f| {
                ready.as_deref() != Some(f.version.as_str())
                    && failed.as_deref() != Some(f.version.as_str())
            })
        };
        if let Some(target) = target {
            match self.source.download(&target) {
                Ok(installer) => {
                    let version = target.version.clone();
                    let still = lock(&self.found)
                        .as_ref()
                        .is_some_and(|f| f.version == version);
                    if still {
                        *lock(&self.ready) = Some((target, installer));
                        lock(&self.saved).ready = Some(version.clone());
                        self.save();
                        self.emit(Event::Ready { version: &version });
                    } else {
                        self.source.forget(&version);
                    }
                }
                Err(error) => {
                    lock(&self.saved).failed = Some(target.version.clone());
                    self.save();
                    self.emit(Event::Failed {
                        stage: Stage::Download,
                        version: Some(&target.version),
                        error: &error,
                    });
                }
            }
        }
        self.downloading.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_higher_plain_version_is_newer() {
        assert!(is_newer("1.2.0", "1.1.9"));
        assert!(is_newer("v2.0.0", "1.99.99"));
        assert!(is_newer("0.10.0", "0.9.0"), "numbers, not text");
        assert!(!is_newer("1.1.0", "1.1.0"));
        assert!(!is_newer("1.0.9", "1.1.0"));
        assert!(
            !is_newer("1.2.0-beta.1", "1.1.0"),
            "a pre-release is never offered"
        );
        assert!(!is_newer("1.2", "1.1.0"));
        assert!(!is_newer("1.2.0.1", "1.1.0"));
        assert!(!is_newer("1.2.0", "not a version"));
    }
}
