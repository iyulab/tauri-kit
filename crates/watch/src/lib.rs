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
//!   written, and a removed folder is its files removed. Folders themselves are not reported.
//!   Notifications only say where to look: the watcher keeps a listing of the watched tree and
//!   compares the folders a batch touches with it, so a file the platform did not announce — one
//!   made in a folder that was itself just made, or the old name of a rename — is still reported.
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
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;
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
    probe: Option<(PathBuf, Duration)>,
}

impl fmt::Debug for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watch")
            .field("root", &self.root)
            .field("debounce", &self.debounce)
            .field("ignore", &self.ignore.is_some())
            .field("own_writes", &self.own.is_some())
            .field("probe", &self.probe)
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
            probe: None,
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

    /// Checks every `every` that the watch is still running, and restarts it if it is not.
    ///
    /// On Windows the platform stops a watch when its buffer of changes overflows — a burst of
    /// changes, a sync client catching up — and nothing says so: the watch just goes quiet. With a
    /// probe the watcher writes a short-lived file into `folder` (relative to the watched folder,
    /// and created if missing) and expects to hear about it; when two probes in a row go unheard it
    /// starts the watch again and sends [`Notice::Rescan`]. Choose a folder the app owns, such as
    /// its hidden state folder. The probe files are named like `tauri-kit-fs` temporary files, so
    /// they are never reported and `sweep_staging` removes any a crash leaves behind; one probe file
    /// is there at a time. `every` must be longer than twice the debounce window — minutes suit a
    /// real app.
    pub fn probe_liveness(mut self, folder: impl Into<PathBuf>, every: Duration) -> Self {
        self.probe = Some((folder.into(), every));
        self
    }

    /// Starts watching. `on_notice` runs on the watcher's own thread, once per batch.
    ///
    /// The watched tree is listed once before watching starts, which takes a moment for a large
    /// folder.
    pub fn start(self, on_notice: impl FnMut(Notice) + Send + 'static) -> io::Result<Watcher> {
        let root = resolved(&self.root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a folder", root.display()),
            ));
        }
        let shared = Arc::new(Shared {
            resolver: Mutex::new(Resolver::new(root.clone(), self.ignore, self.own)),
            on_notice: Mutex::new(Box::new(on_notice)),
            liveness: Mutex::new(Liveness::default()),
            root: root.clone(),
            debounce: self.debounce,
        });
        if let Some((_, every)) = &self.probe {
            if *every <= self.debounce * 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the liveness probe interval must be longer than twice the debounce window",
                ));
            }
        }
        let inner = Arc::new(Mutex::new(Some(shared.watch()?)));
        let probe = match self.probe {
            Some((folder, every)) => Some(Probe::start(
                Arc::clone(&shared),
                Arc::clone(&inner),
                root.join(folder),
                every,
            )?),
            None => None,
        };
        Ok(Watcher { root, inner, probe })
    }
}

type Handler = Box<dyn FnMut(Notice) + Send>;
type PlatformWatch = Debouncer<RecommendedWatcher, RecommendedCache>;

/// What the platform watch's callback and the probe share.
struct Shared {
    root: PathBuf,
    debounce: Duration,
    resolver: Mutex<Resolver>,
    on_notice: Mutex<Handler>,
    liveness: Mutex<Liveness>,
}

impl Shared {
    /// Starts a platform watch that delivers to this.
    fn watch(self: &Arc<Self>) -> io::Result<PlatformWatch> {
        let shared = Arc::clone(self);
        let mut watch = new_debouncer(self.debounce, None, move |result| shared.deliver(result))
            .map_err(notify_error)?;
        watch
            .watch(&self.root, RecursiveMode::Recursive)
            .map_err(notify_error)?;
        Ok(watch)
    }

    fn deliver(&self, result: DebounceEventResult) {
        if let Ok(events) = &result {
            relock(&self.liveness).observe(events);
        }
        let notice = relock(&self.resolver).notice(result);
        if let Some(notice) = notice {
            self.tell(notice);
        }
    }

    fn tell(&self, notice: Notice) {
        (relock(&self.on_notice))(notice);
    }
}

/// A lock that a panic elsewhere does not make unusable: the data it guards stays consistent
/// between the steps that take it.
fn relock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A running watch. Dropping it stops watching.
pub struct Watcher {
    root: PathBuf,
    inner: Arc<Mutex<Option<PlatformWatch>>>,
    probe: Option<Probe>,
}

impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher")
            .field("root", &self.root)
            .field("probe", &self.probe.is_some())
            .finish()
    }
}

impl Watcher {
    /// The watched folder, as an absolute path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // The probe first: it could otherwise start a new platform watch while this one stops.
        self.probe.take();
        relock(&self.inner).take();
    }
}

/// Probes missed in a row before the watch is taken for dead: one can go unheard because a slow
/// disk or a long batch delayed it.
const MISSES_BEFORE_RESTART: u32 = 2;

/// Whether the latest probe was heard.
#[derive(Default)]
struct Liveness {
    /// The file name of the probe in flight.
    pending: Option<OsString>,
    heard: bool,
    misses: u32,
}

impl Liveness {
    /// Starts a probe named `name`; returns whether the watch is to be taken for dead, judging by
    /// the probes before it.
    fn arm(&mut self, name: OsString) -> bool {
        if self.pending.is_some() && !self.heard {
            self.misses += 1;
        } else {
            self.misses = 0;
        }
        self.pending = Some(name);
        self.heard = false;
        self.misses >= MISSES_BEFORE_RESTART
    }

    fn observe(&mut self, events: &[DebouncedEvent]) {
        let Some(pending) = &self.pending else {
            return;
        };
        if events
            .iter()
            .flat_map(|e| e.paths.iter())
            .any(|p| p.file_name() == Some(pending.as_os_str()))
        {
            self.heard = true;
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The thread that probes the watch. Dropping it stops the thread.
struct Probe {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Probe {
    fn start(
        shared: Arc<Shared>,
        inner: Arc<Mutex<Option<PlatformWatch>>>,
        folder: PathBuf,
        every: Duration,
    ) -> io::Result<Self> {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = thread::Builder::new()
            .name("tauri-kit-watch-probe".into())
            .spawn(move || {
                let mut count: u64 = 0;
                // A probe stays until the next one: made and removed within one batch, the two
                // notifications would cancel out and the probe would never be heard.
                let mut last: Option<PathBuf> = None;
                // Anything on the channel, or the watcher dropping its end, stops the probe.
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                    count += 1;
                    let name = OsString::from(format!(
                        "{}watch-probe-{}-{count}",
                        tauri_kit_fs::TEMP_PREFIX,
                        std::process::id()
                    ));
                    if relock(&shared.liveness).arm(name.clone()) {
                        restart(&shared, &inner);
                    }
                    if let Some(previous) = last.take() {
                        let _ = fs::remove_file(previous);
                    }
                    let path = folder.join(&name);
                    // A probe that cannot be written is simply not heard; enough of them restart
                    // the watch, which is also the right answer to a folder that went away.
                    if fs::create_dir_all(&folder)
                        .and_then(|()| fs::write(&path, b""))
                        .is_ok()
                    {
                        last = Some(path);
                    }
                }
                if let Some(previous) = last {
                    let _ = fs::remove_file(previous);
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Replaces a watch that went quiet with a new one, and tells the app to read everything again.
fn restart(shared: &Arc<Shared>, inner: &Mutex<Option<PlatformWatch>>) {
    let Ok(fresh) = shared.watch() else {
        // Not now (the folder may be gone); the next probes try again.
        return;
    };
    let stale = relock(inner).replace(fresh);
    drop(stale);
    relock(&shared.resolver).reindex();
    relock(&shared.liveness).reset();
    shared.tell(Notice::Rescan);
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
///
/// Notifications alone do not say everything that changed: on Linux a file made in a folder that
/// was itself just made can go unannounced (the folder is watched only once its own notification
/// is handled), and on macOS a rename may name only its new path. So the resolver keeps a listing
/// of every watched folder and treats a notification as a reason to look again: the folders a
/// batch touches are listed anew and compared with what they held, and a new folder is read whole.
struct Resolver {
    root: PathBuf,
    ignore: Option<IgnoreRule>,
    own: Option<OwnWrites>,
    /// Each watched folder (absolute) and its entries when last listed: name → is a folder.
    folders: HashMap<PathBuf, HashMap<OsString, bool>>,
}

/// The changes of one batch, each path once, in the order first found.
#[derive(Default)]
struct Batch {
    seen: HashSet<PathBuf>,
    changes: Vec<Change>,
}

impl Batch {
    fn push(&mut self, path: PathBuf, kind: ChangeKind) {
        if self.seen.insert(path.clone()) {
            self.changes.push(Change { path, kind });
        }
    }
}

impl Resolver {
    fn new(root: PathBuf, ignore: Option<IgnoreRule>, own: Option<OwnWrites>) -> Self {
        let mut resolver = Self {
            root,
            ignore,
            own,
            folders: HashMap::new(),
        };
        resolver.reindex();
        resolver
    }

    /// Lists the whole tree again, forgetting what was known.
    fn reindex(&mut self) {
        self.folders.clear();
        let root = self.root.clone();
        self.read_folder(&root, &mut Vec::new());
    }

    fn notice(&mut self, result: DebounceEventResult) -> Option<Notice> {
        let lost = match &result {
            // Errors from the platform watcher: what was missed is unknown.
            Err(_) => true,
            Ok(events) => events.iter().any(|e| e.need_rescan()),
        };
        if lost {
            self.reindex();
            return Some(Notice::Rescan);
        }
        let changes = self.changes(&result.unwrap_or_default());
        (!changes.is_empty()).then_some(Notice::Changed(changes))
    }

    fn changes(&mut self, events: &[DebouncedEvent]) -> Vec<Change> {
        let mut batch = Batch::default();
        let mut touched = HashSet::new();
        let mut folders: Vec<PathBuf> = Vec::new();
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
            if !touched.insert(path.clone()) || self.relative_if_watched(path).is_none() {
                continue;
            }
            // The folder it is in is looked at again, and so is the path if it is a folder now.
            if let Some(parent) = path.parent() {
                if parent.starts_with(&self.root) && !folders.iter().any(|f| f == parent) {
                    folders.push(parent.to_path_buf());
                }
            }
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => {
                    if !folders.contains(path) {
                        folders.push(path.clone());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // A file that changed — or one there but unreadable for now, held open by another
                // program: it changed too.
                _ => self.written(path, &mut batch),
            }
        }
        for folder in folders {
            self.relist(&folder, &mut batch);
        }
        batch.changes
    }

    /// Compares a folder with what it held when last listed.
    fn relist(&mut self, folder: &Path, batch: &mut Batch) {
        let before = self.folders.get(folder).cloned();
        let Some(now) = self.list(folder) else {
            // The folder is gone, and with it everything that was in it.
            if before.is_some() {
                self.removed_tree(folder, batch);
            }
            return;
        };
        let before = before.unwrap_or_default();
        for (name, was_folder) in &before {
            if now.get(name) == Some(was_folder) {
                continue;
            }
            let path = folder.join(name);
            if *was_folder {
                self.removed_tree(&path, batch);
            } else {
                self.removed(&path, batch);
            }
        }
        self.folders.insert(folder.to_path_buf(), now.clone());
        let mut found = Vec::new();
        for (name, is_folder) in &now {
            if before.get(name) == Some(is_folder) {
                continue;
            }
            let path = folder.join(name);
            if *is_folder {
                self.read_folder(&path, &mut found);
            } else {
                found.push(path);
            }
        }
        for file in found {
            self.written(&file, batch);
        }
    }

    /// Lists a folder and everything below it into the index, collecting the files in `found`.
    fn read_folder(&mut self, folder: &Path, found: &mut Vec<PathBuf>) {
        let Some(entries) = self.list(folder) else {
            return;
        };
        self.folders.insert(folder.to_path_buf(), entries.clone());
        for (name, is_folder) in entries {
            let path = folder.join(name);
            if is_folder {
                self.read_folder(&path, found);
            } else {
                found.push(path);
            }
        }
    }

    /// A folder's watched entries, or None if it cannot be listed.
    fn list(&self, folder: &Path) -> Option<HashMap<OsString, bool>> {
        let entries = fs::read_dir(folder).ok()?;
        let mut listed = HashMap::new();
        for entry in entries.flatten() {
            if self.relative_if_watched(&entry.path()).is_none() {
                continue;
            }
            // A link is an entry of its own: the watch does not follow it.
            let is_folder = entry.file_type().is_ok_and(|t| t.is_dir());
            listed.insert(entry.file_name(), is_folder);
        }
        Some(listed)
    }

    /// The path relative to the root, unless it is outside it, the root itself, a temporary file
    /// or ignored.
    fn relative_if_watched<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        let relative = path.strip_prefix(&self.root).ok()?;
        if relative.as_os_str().is_empty() || is_temporary(relative) {
            return None;
        }
        if self.ignore.as_ref().is_some_and(|rule| rule(relative)) {
            return None;
        }
        Some(relative)
    }

    fn written(&self, path: &Path, batch: &mut Batch) {
        let Some(relative) = self.relative_if_watched(path) else {
            return;
        };
        if self
            .own
            .as_ref()
            .is_some_and(|own| own.holds_own_write(path))
        {
            return;
        }
        batch.push(relative.to_path_buf(), ChangeKind::Written);
    }

    fn removed(&self, path: &Path, batch: &mut Batch) {
        if let Some(own) = &self.own {
            own.forget(path);
        }
        if let Some(relative) = self.relative_if_watched(path) {
            batch.push(relative.to_path_buf(), ChangeKind::Removed);
        }
    }

    /// A folder that is gone: every file it was known to hold is removed.
    fn removed_tree(&mut self, folder: &Path, batch: &mut Batch) {
        let Some(entries) = self.folders.remove(folder) else {
            return;
        };
        for (name, is_folder) in entries {
            let path = folder.join(name);
            if is_folder {
                self.removed_tree(&path, batch);
            } else {
                self.removed(&path, batch);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_full::notify::event::{CreateKind, ModifyKind, RenameMode};
    use notify_debouncer_full::notify::Event;
    use std::time::Instant;

    fn event(kind: EventKind, path: PathBuf) -> DebouncedEvent {
        DebouncedEvent::new(Event::new(kind).add_path(path), Instant::now())
    }

    fn resolver(root: &Path) -> Resolver {
        Resolver::new(resolved(root).unwrap(), None, None)
    }

    fn change(path: &Path, kind: ChangeKind) -> Change {
        Change {
            path: path.to_path_buf(),
            kind,
        }
    }

    #[test]
    fn a_rename_announced_by_its_new_name_only_still_reports_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "x").unwrap();
        let mut r = resolver(dir.path());
        fs::rename(dir.path().join("a.md"), dir.path().join("b.md")).unwrap();
        let root = r.root.clone();
        let changes = r.changes(&[event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            root.join("b.md"),
        )]);
        assert_eq!(
            changes,
            vec![
                change(Path::new("b.md"), ChangeKind::Written),
                change(Path::new("a.md"), ChangeKind::Removed),
            ]
        );
    }

    #[test]
    fn files_in_a_new_folder_are_reported_when_only_the_folder_was_announced() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = resolver(dir.path());
        fs::create_dir_all(dir.path().join("docs").join("2026")).unwrap();
        fs::write(dir.path().join("docs").join("2026").join("note.md"), "x").unwrap();
        let root = r.root.clone();
        let changes = r.changes(&[event(
            EventKind::Create(CreateKind::Folder),
            root.join("docs"),
        )]);
        assert_eq!(
            changes,
            vec![change(
                &Path::new("docs").join("2026").join("note.md"),
                ChangeKind::Written
            )]
        );
    }

    #[test]
    fn a_removed_folder_is_its_files_removed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs").join("old")).unwrap();
        fs::write(dir.path().join("docs").join("a.md"), "x").unwrap();
        fs::write(dir.path().join("docs").join("old").join("b.md"), "x").unwrap();
        let mut r = resolver(dir.path());
        fs::remove_dir_all(dir.path().join("docs")).unwrap();
        let root = r.root.clone();
        let mut changes = r.changes(&[event(
            EventKind::Remove(notify::event::RemoveKind::Folder),
            root.join("docs"),
        )]);
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            changes,
            vec![
                change(&Path::new("docs").join("a.md"), ChangeKind::Removed),
                change(
                    &Path::new("docs").join("old").join("b.md"),
                    ChangeKind::Removed
                ),
            ]
        );
        assert!(!r.folders.keys().any(|f| f.starts_with(root.join("docs"))));
    }

    #[test]
    fn a_lost_batch_lists_the_tree_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = resolver(dir.path());
        fs::write(dir.path().join("a.md"), "x").unwrap();
        assert_eq!(
            r.notice(Err(vec![notify::Error::generic("overflow")])),
            Some(Notice::Rescan)
        );
        let root = r.root.clone();
        assert!(r.folders[&root].contains_key(&OsString::from("a.md")));
    }

    #[test]
    fn two_unheard_probes_in_a_row_mean_the_watch_is_dead() {
        let mut l = Liveness::default();
        assert!(!l.arm("a".into()), "nothing was pending");
        assert!(!l.arm("b".into()), "one miss is tolerated");
        assert!(l.arm("c".into()), "two in a row are not");
        l.reset();
        assert!(!l.arm("d".into()));
        let heard = event(
            EventKind::Create(CreateKind::File),
            PathBuf::from("x").join("d"),
        );
        l.observe(&[heard]);
        assert!(!l.arm("e".into()), "a heard probe clears the misses");
        assert_eq!(l.misses, 0);
    }

    #[test]
    fn a_watch_that_went_quiet_is_restarted_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, notices) = std::sync::mpsc::channel();
        let watcher = Watch::new(dir.path())
            .debounce(Duration::from_millis(50))
            .probe_liveness(".state", Duration::from_millis(150))
            .start(move |n| {
                let _ = tx.send(n);
            })
            .unwrap();
        // What an overflowing buffer does on Windows: the platform watch stops, silently.
        relock(&watcher.inner).take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match notices.recv_timeout(left) {
                Ok(Notice::Rescan) => break,
                Ok(_) => continue,
                Err(_) => panic!("no rescan after the watch went quiet"),
            }
        }
        fs::write(dir.path().join("after.md"), "x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match notices.recv_timeout(left) {
                Ok(Notice::Changed(c)) if c.iter().any(|c| c.path == Path::new("after.md")) => {
                    break
                }
                Ok(_) => continue,
                Err(_) => panic!("the restarted watch does not report changes"),
            }
        }
    }
}
