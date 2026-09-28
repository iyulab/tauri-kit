//! Watch a folder for changes made by other programs — a sync client, another editor, a script —
//! and hear about each changed file once, as it now is on disk.
//!
//! Raw file-system notifications are hard to use directly. One save produces several of them (an
//! atomic write alone is a create, a rename and a modify on Windows), their kinds say little about
//! the file's final state, and the app's own writes come back as if someone else had made them.
//! This crate turns them into what an app acts on:
//!
//! - **Debounced**: notifications within a short window (300 ms by default) arrive as one batch.
//! - **Resolved to what is on disk**: each path in a batch is reported once, as
//!   [`ChangeKind::Written`] if a file is there when the batch is delivered, or
//!   [`ChangeKind::Removed`] if nothing is. A rename is the old path removed and the new one
//!   written. Folders are not reported, except as removed paths.
//! - **The app's own writes left out**: record what the app writes with [`OwnWrites::record`], and
//!   a change whose content is what the app last wrote there is not reported — however many
//!   notifications or batches the write took.
//! - **Temporary files left out**: files named the way `tauri-kit-fs` names its temporary files,
//!   and anything the app's [`Watch::ignore`] rule matches.
//! - **Honest about loss**: when the platform says notifications were dropped, the app gets
//!   [`Notice::Rescan`] and should read the folder again instead of trusting what it knows.
//!
//! ```no_run
//! use tauri_kit_watch::{Notice, OwnWrites, Watch};
//!
//! let own = OwnWrites::new();
//! let watcher = Watch::new("/path/to/vault")
//!     .ignore(|path| path.starts_with(".git"))
//!     .own_writes(&own)
//!     .start(|notice| match notice {
//!         Notice::Changed(changes) => { /* reload what changed */ }
//!         Notice::Rescan => { /* read everything again */ }
//!     })?;
//!
//! let path = std::path::Path::new("/path/to/vault/note.md");
//! own.record(path, b"# Note");
//! if let Err(e) = tauri_kit_fs::write_atomic(path, b"# Note") {
//!     own.forget(path);
//!     return Err(e);
//! }
//! // `watcher` stops watching when dropped.
//! # Ok::<(), std::io::Error>(())
//! ```

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{fmt, fs, io};

use notify_debouncer_full::notify::{self, EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache,
};

/// How long notifications are gathered before a batch is delivered, unless the app chooses.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);

/// What a watcher tells the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Files changed by something other than the app, each path once, in the order first seen.
    Changed(Vec<Change>),
    /// Notifications were lost — the platform's buffer overflowed, or the watcher failed. What the
    /// app knows about the folder may be stale: read it again.
    Rescan,
}

/// One changed path, relative to the watched folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// A path's state on disk when its change is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A file is there: created, replaced or modified.
    Written,
    /// Nothing is there any more: the file (or folder) was deleted or moved away.
    Removed,
}

type IgnoreRule = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// A watch to start: the folder and how its changes are reported.
pub struct Watch {
    root: PathBuf,
    debounce: Duration,
    ignore: Option<IgnoreRule>,
    own: Option<OwnWrites>,
}

impl fmt::Debug for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watch")
            .field("root", &self.root)
            .field("debounce", &self.debounce)
            .field("ignore", &self.ignore.is_some())
            .field("own_writes", &self.own.is_some())
            .finish()
    }
}

impl Watch {
    /// Watches `root` and everything below it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            debounce: DEFAULT_DEBOUNCE,
            ignore: None,
            own: None,
        }
    }

    /// How long notifications are gathered into one batch. Longer means fewer, larger batches and
    /// a later report; [`DEFAULT_DEBOUNCE`] suits interactive apps.
    pub fn debounce(mut self, window: Duration) -> Self {
        self.debounce = window;
        self
    }

    /// Paths not to report. The rule gets each path relative to the watched folder; a folder the
    /// app keeps its own state in (`.git`, a cache) is a typical match.
    pub fn ignore(mut self, rule: impl Fn(&Path) -> bool + Send + Sync + 'static) -> Self {
        self.ignore = Some(Arc::new(rule));
        self
    }

    /// Leaves out changes whose content is what the app recorded writing.
    pub fn own_writes(mut self, own: &OwnWrites) -> Self {
        self.own = Some(own.clone());
        self
    }

    /// Starts watching. `on_notice` runs on the watcher's own thread, once per batch.
    pub fn start(self, mut on_notice: impl FnMut(Notice) + Send + 'static) -> io::Result<Watcher> {
        let root = resolved(&self.root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a folder", root.display()),
            ));
        }
        let resolver = Resolver {
            root: root.clone(),
            ignore: self.ignore,
            own: self.own,
        };
        let mut debouncer =
            new_debouncer(self.debounce, None, move |result: DebounceEventResult| {
                if let Some(notice) = resolver.notice(result) {
                    on_notice(notice);
                }
            })
            .map_err(notify_error)?;
        debouncer
            .watch(&root, RecursiveMode::Recursive)
            .map_err(notify_error)?;
        Ok(Watcher {
            root,
            _debouncer: debouncer,
        })
    }
}

/// A running watch. Dropping it stops watching.
pub struct Watcher {
    root: PathBuf,
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher").field("root", &self.root).finish()
    }
}

impl Watcher {
    /// The watched folder, as an absolute path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// What the app last wrote to each path, so its own writes are not reported back to it.
///
/// Record a write *before* making it: its notifications can arrive before the write returns. A
/// change is left out while the file holds exactly the recorded content; the first change to
/// anything else is reported and the record dropped. If the write fails, [`forget`](Self::forget)
/// the path. Cloning shares the record.
#[derive(Clone, Default)]
pub struct OwnWrites {
    known: Arc<Mutex<HashMap<PathBuf, Fingerprint>>>,
}

impl fmt::Debug for OwnWrites {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnWrites")
            .field("paths", &self.lock().len())
            .finish()
    }
}

impl OwnWrites {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that the app is about to write `content` to `path`.
    pub fn record(&self, path: &Path, content: &[u8]) {
        if let Some(key) = key(path) {
            self.lock().insert(key, Fingerprint::of(content));
        }
    }

    /// Drops the record for `path` — after a write that failed, or once the app no longer cares.
    pub fn forget(&self, path: &Path) {
        if let Some(key) = key(path) {
            self.lock().remove(&key);
        }
    }

    /// Whether the file at `path` (absolute) holds what the app last recorded writing there.
    /// A file that differs, is gone, or cannot be read ends the record.
    fn holds_own_write(&self, path: &Path) -> bool {
        let mut known = self.lock();
        let Some(expected) = known.get(path).copied() else {
            return false;
        };
        let current = tauri_kit_fs::patiently(|| fs::read(path)).map(|c| Fingerprint::of(&c));
        if current.ok() == Some(expected) {
            return true;
        }
        known.remove(path);
        false
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<PathBuf, Fingerprint>> {
        // A panic while holding the lock leaves a plain map behind; it is still usable.
        self.known.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A path as notifications name it: absolute, in the platform's own form, and — outside Windows —
/// with links in its folders resolved, since notifications name the real location (on macOS the
/// temporary folder `/var/…` is reported as `/private/var/…`). On Windows resolving would add the
/// `\\?\` prefix that notifications do not carry, so the path is only made absolute there.
fn resolved(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    if cfg!(windows) {
        return Ok(absolute);
    }
    if let Ok(real) = fs::canonicalize(&absolute) {
        return Ok(real);
    }
    // Not there (yet): resolve the folder it would be in.
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => Ok(resolved(parent)?.join(name)),
        _ => Ok(absolute),
    }
}

fn key(path: &Path) -> Option<PathBuf> {
    resolved(path).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    len: usize,
    hash: u64,
}

impl Fingerprint {
    fn of(content: &[u8]) -> Self {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        Self {
            len: content.len(),
            hash: hasher.finish(),
        }
    }
}

/// Turns a debounced batch into what the app is told.
struct Resolver {
    root: PathBuf,
    ignore: Option<IgnoreRule>,
    own: Option<OwnWrites>,
}

impl Resolver {
    fn notice(&self, result: DebounceEventResult) -> Option<Notice> {
        let events = match result {
            Ok(events) => events,
            // Errors from the platform watcher: what was missed is unknown.
            Err(_) => return Some(Notice::Rescan),
        };
        if events.iter().any(|e| e.need_rescan()) {
            return Some(Notice::Rescan);
        }
        let changes = self.changes(&events);
        (!changes.is_empty()).then_some(Notice::Changed(changes))
    }

    fn changes(&self, events: &[DebouncedEvent]) -> Vec<Change> {
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        let paths = events
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                )
            })
            .flat_map(|e| e.paths.iter());
        for path in paths {
            if !seen.insert(path.clone()) {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&self.root) else {
                continue;
            };
            if relative.as_os_str().is_empty() || is_temporary(relative) {
                continue;
            }
            if self.ignore.as_ref().is_some_and(|rule| rule(relative)) {
                continue;
            }
            let kind = match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => continue,
                Ok(_) => {
                    if self
                        .own
                        .as_ref()
                        .is_some_and(|own| own.holds_own_write(path))
                    {
                        continue;
                    }
                    ChangeKind::Written
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if let Some(own) = &self.own {
                        own.forget(path);
                    }
                    ChangeKind::Removed
                }
                // There but unreadable for now (held open by another program): it changed.
                Err(_) => ChangeKind::Written,
            };
            changes.push(Change {
                path: relative.to_path_buf(),
                kind,
            });
        }
        changes
    }
}

/// A temporary file of `tauri-kit-fs`: it only exists while a write is in progress.
fn is_temporary(relative: &Path) -> bool {
    relative
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(tauri_kit_fs::TEMP_PREFIX))
}

fn notify_error(e: notify::Error) -> io::Error {
    match e.kind {
        notify::ErrorKind::Io(io) => io,
        notify::ErrorKind::PathNotFound => io::Error::new(io::ErrorKind::NotFound, e.to_string()),
        _ => io::Error::other(e.to_string()),
    }
}
