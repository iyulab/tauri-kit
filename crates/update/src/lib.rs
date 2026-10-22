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
//!   tried again for that version until `every` has passed or a newer release appears — so a broken
//!   release does not cost a download at every launch, even for an app that looks at every launch.
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
//! - **Which releases.** A [`Channel`]: stable offers plain releases only, beta pre-releases too
//!   (`1.3.0-beta.2`). The channel is the app's to remember, like looking on or off
//!   ([`UpdateOptions::channel`], [`Updates::set_channel`]). Leaving beta never goes back down: an app
//!   on `1.3.0-beta.2` waits for `1.3.0`, which semver orders after it.
//! - **What kind of release.** [`Kind`]: a fix (only the patch number rose) or a feature (the minor or
//!   major did), counted from the versions — the versioning policy already says what each number
//!   means. [`Event::Found`] and [`Status`] carry it; how to tell the person (a fix quietly, a feature
//!   with a word) is the app's.
//!
//! Releases published on GitHub are one such place: [`github::Releases`] finds the newest on a
//! channel (the `latest` redirect, or the release list for beta) and downloads its installer, over
//! the app's HTTP client or, with the `github` feature, `ureq`.
//!
//! A source that downloads an installer itself checks who signed it before handing it on:
//! [`verify::authenticode`] (feature `authenticode`) compares the signer with an [`verify::Expect`] —
//! subject, issuer, thumbprint — rather than the display name alone.
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
//! use tauri_kit_update::{Channel, Found, Source};
//!
//! struct Plugin(tauri::AppHandle);
//!
//! impl Source for Plugin {
//!     type Update = Update;
//!     type Installer = Vec<u8>;
//!
//!     fn check(&self, channel: Channel) -> Result<Option<Found<Update>>, String> {
//!         // One manifest per channel, both signed with the updater key.
//!         let manifest = match channel {
//!             Channel::Stable => "https://example.com/releases/latest/download/latest.json",
//!             Channel::Beta => "https://example.com/releases/download/beta/latest.json",
//!         };
//!         let updater = self.0.updater_builder().endpoints(vec![manifest.parse().unwrap()])
//!             .map_err(|e| e.to_string())?.build().map_err(|e| e.to_string())?;
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

pub mod github;
#[cfg(feature = "authenticode")]
pub mod verify;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Which releases to offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Plain releases only. The default.
    #[default]
    Stable,
    /// Pre-releases too (`1.3.0-beta.2`), and plain releases as they come.
    Beta,
}

/// What a newer release is to the running one, by its version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Only the patch number rose (or a pre-release of the same version moved on): fixes.
    Fix,
    /// The minor or major number rose: something new.
    Feature,
}

/// A newer release a [`Source`] found: its version, and whatever the source needs to download and
/// install it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<U> {
    /// The release's version, `major.minor.patch` with a pre-release tail on the beta channel.
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

    /// The newest release on `channel`, or `None` when there is none to offer. A release that is not
    /// newer than the running version — or a pre-release on the stable channel — is ignored, so a
    /// source may simply report the latest one.
    fn check(&self, channel: Channel) -> Result<Option<Found<Self::Update>>, String>;

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
    /// Which releases it offers. Default [`Channel::Stable`].
    pub channel: Channel,
}

impl UpdateOptions {
    /// Options for the running version `current`, with the defaults.
    pub fn new(current: impl Into<String>) -> Self {
        UpdateOptions {
            current: current.into(),
            every: Duration::from_secs(12 * 60 * 60),
            state_file: None,
            enabled: true,
            channel: Channel::Stable,
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
        /// A fix or a feature, to the running version.
        kind: Kind,
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
    /// What that release is to the running version.
    pub kind: Option<Kind>,
    /// The channel it offers releases from.
    pub channel: Channel,
    /// The version whose installer is downloaded and checked, if any.
    pub ready: Option<String>,
    /// An installer is being downloaded.
    pub downloading: bool,
    /// The version whose download failed, if any — kept across launches with a state file. It is not
    /// downloaded again until [`UpdateOptions::every`] has passed since, or a newer release is found;
    /// the moment to offer the person another way to get it.
    pub failed: Option<String>,
    /// When it last looked, as seconds since the Unix epoch.
    pub last_checked: Option<u64>,
    /// When the source last answered a look, as seconds since the Unix epoch — kept across launches
    /// with a state file. A look the source did not answer leaves it as it was, so with nothing
    /// [`available`](Self::available) this tells «up to date» (it answered) from «not known» (it
    /// never has).
    pub last_answered: Option<u64>,
}

/// Whether `candidate` is a release to offer over `current` on `channel`: newer in semver order
/// (a pre-release before its release — `1.3.0-beta.2` < `1.3.0`), and on the stable channel a plain
/// release. A version that does not read as semver is never newer. The running version may be a
/// pre-release itself: an app that installed a beta is offered the release that follows it.
pub fn is_newer(candidate: &str, current: &str, channel: Channel) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(a), Some(b)) => (channel == Channel::Beta || a.pre.is_empty()) && a > b,
        _ => false,
    }
}

/// What `candidate` is to `current` — a fix when their major and minor numbers are the same, a
/// feature when not. `None` when either does not read as semver.
pub fn kind_of(candidate: &str, current: &str) -> Option<Kind> {
    let (a, b) = (Version::parse(candidate)?, Version::parse(current)?);
    Some(if (a.major, a.minor) == (b.major, b.minor) {
        Kind::Fix
    } else {
        Kind::Feature
    })
}

/// A semver version, ordered as semver orders them (build metadata is not part of the order).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Identifier>,
}

/// One dot-separated part of a pre-release tail: numbers order below words, and by value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    Number(u64),
    Word(String),
}

impl Version {
    fn parse(version: &str) -> Option<Version> {
        let version = version.strip_prefix('v').unwrap_or(version);
        let version = version.split_once('+').map_or(version, |(v, _)| v);
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (version, None),
        };
        // Digits only, and no leading zero but in `0` itself (semver §2, §9).
        let numeric = |part: &str| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        };
        let number = |part: &str| numeric(part).then(|| part.parse().ok()).flatten();
        let mut parts = core.split('.');
        let (major, minor, patch) = (
            number(parts.next()?)?,
            number(parts.next()?)?,
            number(parts.next()?)?,
        );
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|part| {
                    if part.is_empty()
                        || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    {
                        None
                    } else if part.bytes().all(|b| b.is_ascii_digit()) {
                        numeric(part)
                            .then(|| part.parse().ok())
                            .flatten()
                            .map(Identifier::Number)
                    } else {
                        Some(Identifier::Word(part.to_owned()))
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| {
                // A release orders after its pre-releases; between pre-releases, part by part, and a
                // longer tail after the shorter one it starts with.
                match (self.pre.is_empty(), other.pre.is_empty()) {
                    (true, true) => std::cmp::Ordering::Equal,
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    (false, false) => self.pre.cmp(&other.pre),
                }
            })
    }
}

const FORMAT: &str = "tauri-kit-update/1";

/// What survives a launch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    format: String,
    checked_at: Option<u64>,
    answered_at: Option<u64>,
    announced: Option<String>,
    ready: Option<String>,
    failed: Option<String>,
    failed_at: Option<u64>,
}

type Handler = Box<dyn for<'a> FnMut(Event<'a>) + Send>;
type Kept<S> = (Found<<S as Source>::Update>, <S as Source>::Installer);

struct Inner<S: Source> {
    source: S,
    current: String,
    every: Duration,
    state_file: Option<PathBuf>,
    enabled: AtomicBool,
    channel: Mutex<Channel>,
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
            if is_newer(&version, &options.current, options.channel) {
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
            channel: Mutex::new(options.channel),
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
        // One lock for both: a guard taken inside the struct expression lives to its end, so a
        // second `lock(&inner.saved)` there would wait on the first.
        let (failed, last_checked, last_answered) = {
            let saved = lock(&inner.saved);
            (saved.failed.clone(), saved.checked_at, saved.answered_at)
        };
        let available = lock(&inner.found).as_ref().map(|f| f.version.clone());
        Status {
            enabled: inner.enabled.load(Ordering::SeqCst),
            kind: available
                .as_deref()
                .and_then(|v| kind_of(v, &inner.current)),
            channel: *lock(&inner.channel),
            available,
            ready: lock(&inner.ready).as_ref().map(|(f, _)| f.version.clone()),
            downloading: inner.downloading.load(Ordering::SeqCst),
            failed,
            last_checked,
            last_answered,
        }
    }

    /// Turns looking on its own on or off. Off lets go of a release found and its installer; on
    /// looks at once when a look is due. Remembering the choice is the app's.
    pub fn set_enabled(&self, on: bool) {
        let inner = &self.inner;
        inner.enabled.store(on, Ordering::SeqCst);
        if !on {
            inner.let_go();
            inner.save();
        }
        inner.wake.1.notify_all();
    }

    /// Offers releases from `channel` from now on. A change lets go of the release found and its
    /// installer and looks again at once (when looking is on); the same channel changes nothing.
    /// Remembering the choice is the app's.
    pub fn set_channel(&self, channel: Channel) {
        let inner = &self.inner;
        if std::mem::replace(&mut *lock(&inner.channel), channel) == channel {
            return;
        }
        inner.let_go();
        lock(&inner.saved).checked_at = None;
        inner.save();
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
    /// Lets go of the release found and its installer.
    fn let_go(&self) {
        *lock(&self.found) = None;
        let kept = lock(&self.ready).take();
        if let Some((found, _)) = kept {
            self.source.forget(&found.version);
        }
        lock(&self.saved).ready = None;
    }

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
        let channel = *lock(&self.channel);
        let answer = self.source.check(channel);
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
            Ok(found) => found.filter(|f| is_newer(&f.version, &self.current, channel)),
        };
        let first_time = {
            let mut saved = lock(&self.saved);
            let now = now();
            saved.checked_at = Some(now);
            saved.answered_at = saved.checked_at;
            // A download that failed gets another chance once `every` has passed since, or for a
            // newer release — not at every look, which may be every launch.
            let waited = saved
                .failed_at
                .is_none_or(|at| now < at || now - at >= self.every.as_secs());
            let other = found.as_ref().map(|f| f.version.as_str()) != saved.failed.as_deref();
            if waited || other {
                saved.failed = None;
                saved.failed_at = None;
            }
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
            kind: kind_of(&version, &self.current).unwrap_or(Kind::Feature),
            first_time,
        });
        Looked::Found(version)
    }

    /// Downloads the installer of the release found, unless it is ready, its download failed and
    /// may not be tried again yet (see [`Status::failed`]), or another download is under way.
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
                    {
                        let mut saved = lock(&self.saved);
                        saved.failed = Some(target.version.clone());
                        saved.failed_at = Some(now());
                    }
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

    const S: Channel = Channel::Stable;
    const B: Channel = Channel::Beta;

    #[test]
    fn on_stable_only_a_higher_plain_version_is_newer() {
        assert!(is_newer("1.2.0", "1.1.9", S));
        assert!(is_newer("v2.0.0", "1.99.99", S));
        assert!(is_newer("0.10.0", "0.9.0", S), "numbers, not text");
        assert!(!is_newer("1.1.0", "1.1.0", S));
        assert!(!is_newer("1.0.9", "1.1.0", S));
        assert!(
            !is_newer("1.2.0-beta.1", "1.1.0", S),
            "a pre-release is not offered on stable"
        );
        assert!(!is_newer("1.2", "1.1.0", S));
        assert!(!is_newer("1.2.0.1", "1.1.0", S));
        assert!(
            !is_newer("01.2.0", "1.1.0", S),
            "a number with a leading zero is not semver"
        );
        assert!(!is_newer("1.2.0", "not a version", S));
        assert!(
            is_newer("1.2.0+build.7", "1.1.0", S),
            "build metadata is not part of the order"
        );
    }

    #[test]
    fn beta_offers_pre_releases_in_semver_order() {
        assert!(is_newer("1.3.0-beta.1", "1.2.0", B));
        assert!(is_newer("1.3.0-beta.2", "1.3.0-beta.1", B));
        assert!(
            is_newer("1.3.0-beta.10", "1.3.0-beta.9", B),
            "numbers by value"
        );
        assert!(is_newer("1.3.0-rc.1", "1.3.0-beta.9", B), "words by text");
        assert!(
            is_newer("1.3.0-beta.1.1", "1.3.0-beta.1", B),
            "a longer tail after its start"
        );
        assert!(is_newer("1.3.0-beta", "1.3.0-1", B), "words after numbers");
        assert!(is_newer("1.3.0", "1.3.0-beta.2", B));
        assert!(
            !is_newer("1.3.0-beta.1", "1.3.0", B),
            "never back to a pre-release of the running release"
        );
        assert!(!is_newer("1.3.0-beta..1", "1.2.0", B));
    }

    #[test]
    fn leaving_beta_waits_for_the_release_and_never_goes_down() {
        assert!(
            !is_newer("1.2.0", "1.3.0-beta.2", S),
            "the last release is older than the beta"
        );
        assert!(
            is_newer("1.3.0", "1.3.0-beta.2", S),
            "the release that follows the beta"
        );
    }

    #[test]
    fn the_kind_is_counted_from_the_versions() {
        assert_eq!(kind_of("1.2.1", "1.2.0"), Some(Kind::Fix));
        assert_eq!(kind_of("1.3.0", "1.2.9"), Some(Kind::Feature));
        assert_eq!(kind_of("2.0.0", "1.9.9"), Some(Kind::Feature));
        assert_eq!(kind_of("0.14.0", "0.13.13"), Some(Kind::Feature));
        assert_eq!(kind_of("1.3.0-beta.2", "1.3.0-beta.1"), Some(Kind::Fix));
        assert_eq!(kind_of("1.3.0", "1.3.0-beta.2"), Some(Kind::Fix));
        assert_eq!(kind_of("1.3.0-beta.1", "1.2.0"), Some(Kind::Feature));
        assert_eq!(kind_of("x", "1.2.0"), None);
    }
}
