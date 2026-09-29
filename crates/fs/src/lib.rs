//! Crash-safe file writes for desktop apps.
//!
//! Writing a file in place truncates it first, so a crash, a power loss or a full disk in the middle
//! of the write leaves the user with a half-written or empty file. The remedy is well known and easy
//! to get subtly wrong: write the new content to a temporary file on the same volume, flush it to
//! the device, rename it over the target, and — on platforms where the rename itself is buffered —
//! flush the directory too. After [`write_atomic`] returns, the target holds either its old content
//! or the new content, never a mix.
//!
//! [`write_atomic_new`] lands the same way but never replaces anything: if the target exists when
//! the rename happens — a file, a directory, even a symbolic link to nothing — it fails with
//! [`io::ErrorKind::AlreadyExists`] and the target is left as it was. Use it where two writers may
//! race for the same new name, or where replacing a file the user created would lose data.
//!
//! Where the temporary file lives matters to apps that watch or sync the folder they write into:
//! a temp file beside the target shows up in file watchers, sync clients and version control as a
//! short-lived extra file. [`write_atomic_staged`] takes a staging directory the app chooses instead
//! (it must be on the same volume as the target, or the rename fails with an error rather than
//! silently copying). Temp files left behind by a crash are removed with [`sweep_staging`], which
//! only ever touches files this crate created — or, with [`sweep_staging_prefixed`], files named the
//! way the app says its temp files are named.
//!
//! On Windows, antivirus scanners, search indexers and sync clients open files they see change, and
//! while they hold one, renaming or deleting it fails with "access denied" or "sharing violation".
//! Those refusals last moments, so every write here retries them for up to [`DEFAULT_PATIENCE`]
//! before giving up. [`patiently`] offers the same to the app's own file operations. Elsewhere it
//! runs the operation once.
//!
//! Sync clients keep both sides of a conflicting change, the second under a marked name;
//! [`conflict_copy_of`] recognises those names and says which file each is a copy of.
//!
//! [`Writer`] holds the options — staging directory, temp-file prefix, patience — for apps that
//! write the same way from several places.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use std::path::Path;
//! tauri_kit_fs::write_atomic(Path::new("settings.json"), br#"{"theme":"dark"}"#)?;
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

mod conflict;
pub use conflict::conflict_copy_of;

/// Prefix of every temporary file this crate creates, unless the app chooses its own with
/// [`Writer::temp_prefix`]. [`sweep_staging`] removes only files that carry it, so a staging
/// directory can be shared with other tools without losing their files.
pub const TEMP_PREFIX: &str = ".tauri-kit-tmp-";

/// How long a write keeps retrying a refusal that [`is_transient`] recognises before giving up.
pub const DEFAULT_PATIENCE: Duration = Duration::from_secs(1);

/// Replaces `path` with `content` so that a crash leaves either the old or the new content.
///
/// The temporary file is created beside the target, which guarantees the same volume. Missing
/// parent directories are created.
pub fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    Writer::new().write(path, content)
}

/// Like [`write_atomic`], but stages the temporary file in `staging` instead of beside the target.
///
/// Use this when the target's folder is watched or synced and a transient extra file there would be
/// noticed. `staging` must be on the same volume as `path`: a rename cannot cross volumes, and this
/// function returns that error instead of falling back to a non-atomic copy. `staging` is created if
/// missing.
pub fn write_atomic_staged(path: &Path, content: &[u8], staging: &Path) -> io::Result<()> {
    Writer::new().staging(staging).write(path, content)
}

/// Creates `path` with `content`, crash-safe like [`write_atomic`], but only if nothing is there.
///
/// If anything exists at `path` — a file, a directory, or a symbolic link, even one whose target is
/// missing — this fails with [`io::ErrorKind::AlreadyExists`], leaves it untouched and removes its
/// own temporary file. The check and the landing are one step (a rename that refuses to replace),
/// so two writers racing for the same name cannot both succeed.
pub fn write_atomic_new(path: &Path, content: &[u8]) -> io::Result<()> {
    Writer::new().write_new(path, content)
}

/// Like [`write_atomic_new`], with the temporary file staged in `staging` as
/// [`write_atomic_staged`] does.
pub fn write_atomic_new_staged(path: &Path, content: &[u8], staging: &Path) -> io::Result<()> {
    Writer::new().staging(staging).write_new(path, content)
}

/// Moves the file at `from` to `to`, but only if nothing is at `to`.
///
/// It is a rename, not a copy: the file stays the same file — its creation time, and its history in
/// a sync client, which sees a move rather than a new file and a deleted one. If anything exists at
/// `to` — a file, a directory, a symbolic link — this fails with [`io::ErrorKind::AlreadyExists`] and
/// both names are left as they were; the check and the move are one step, so two apps racing for the
/// same name cannot both succeed. Both must be on the same volume. Transient refusals
/// ([`is_transient`]) are retried for [`DEFAULT_PATIENCE`].
///
/// On Windows this is one `MoveFileExW` that does not replace. Elsewhere the file is linked under the
/// new name, which fails if the name is taken, and then unlinked from the old one: a crash between
/// the two leaves the file under both names, never under neither.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    patiently(|| move_noclobber(from, to))?;
    sync_parent(to)?;
    if parent_of(from)? != parent_of(to)? {
        sync_parent(from)?;
    }
    Ok(())
}

#[cfg(windows)]
fn move_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (from, to) = (wide(from), wide(to));
    // Without MOVEFILE_REPLACE_EXISTING the move fails if the target exists.
    // SAFETY: both are NUL-terminated wide strings that outlive the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn move_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

/// Removes temporary files left in `staging` by writes that never finished (a crash or power loss
/// between creating the temp file and renaming it). Call it once at startup.
///
/// Only files named with [`TEMP_PREFIX`] and last modified more than `older_than` ago are removed.
/// The age is what keeps a sweep away from a write still in progress in another running instance
/// of the app: on Unix an open file can be deleted, so being held open protects nothing. A write
/// takes moments; an age of minutes leaves only true leftovers. A missing directory is not an
/// error; files that cannot be removed are skipped. Returns how many files were removed.
pub fn sweep_staging(staging: &Path, older_than: Duration) -> io::Result<usize> {
    sweep_staging_prefixed(staging, older_than, &[TEMP_PREFIX])
}

/// Like [`sweep_staging`], for temp files whose names start with any of `prefixes`.
///
/// Pass the prefix given to [`Writer::temp_prefix`], together with any prefix earlier versions of
/// the app used, so leftovers from before a rename are cleaned up too. An empty prefix would match
/// every file in the directory and is rejected with [`io::ErrorKind::InvalidInput`], as is an empty
/// list.
pub fn sweep_staging_prefixed(
    staging: &Path,
    older_than: Duration,
    prefixes: &[&str],
) -> io::Result<usize> {
    if prefixes.is_empty() || prefixes.iter().any(|p| p.is_empty()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sweeping needs at least one non-empty prefix",
        ));
    }
    let entries = match fs::read_dir(staging) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = prefixes.iter().any(|p| name.starts_with(p));
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        // A modification time in the future (clock changes) reads as zero age: kept.
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().unwrap_or(Duration::ZERO) >= older_than)
            .unwrap_or(false);
        if ours && is_file && old && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Whether `err` is a refusal that goes away on its own: on Windows, "access denied"
/// (`ERROR_ACCESS_DENIED`, 5) or "sharing violation" (`ERROR_SHARING_VIOLATION`, 32), which is what
/// a rename, create or delete gets while another process — a virus scanner, a search indexer, a
/// sync client — briefly holds the file open. Always false on other platforms, where holding a file
/// open does not block renaming or deleting it.
///
/// "Access denied" is also what a file with no write permission gets, so retrying it only delays
/// that error by the patience allowed; it never turns it into success.
pub fn is_transient(err: &io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(5 | 32))
}

/// Runs `op`, retrying it for up to [`DEFAULT_PATIENCE`] while it fails with an error
/// [`is_transient`] recognises. See [`patiently_for`].
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// use std::path::Path;
/// tauri_kit_fs::patiently(|| std::fs::rename(Path::new("draft.txt"), Path::new("final.txt")))?;
/// # Ok(())
/// # }
/// ```
pub fn patiently<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    patiently_for(DEFAULT_PATIENCE, op)
}

/// Runs `op`, retrying it while it fails with an error [`is_transient`] recognises, with short
/// pauses that grow from 10 to 100 milliseconds, until it succeeds, fails with any other error, or
/// `patience` has passed. The last error is returned. `op` runs at least once; with
/// [`Duration::ZERO`] it runs exactly once. On platforms where no error is transient, `op` runs
/// once.
///
/// `op` must be safe to repeat after a failed attempt — a rename, a create, a delete are.
pub fn patiently_for<T>(
    patience: Duration,
    mut op: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let until = Instant::now() + patience;
    let mut pause = Duration::from_millis(10);
    loop {
        match op() {
            Err(e) if is_transient(&e) => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(e);
                }
                thread::sleep(pause.min(left));
                pause = (pause * 2).min(Duration::from_millis(100));
            }
            done => return done,
        }
    }
}

/// How to write: where the temporary file is staged, how it is named, and how long a transient
/// refusal is retried. The free functions ([`write_atomic`] and friends) use the defaults.
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// use std::path::Path;
/// use tauri_kit_fs::Writer;
///
/// let writer = Writer::new()
///     .staging("/path/to/app-data/staging")
///     .temp_prefix(".myapp-tmp-");
/// writer.write(Path::new("/path/to/documents/report.md"), b"# Report")?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Writer {
    staging: Option<PathBuf>,
    prefix: String,
    patience: Duration,
    #[cfg(feature = "test-hooks")]
    hook: Option<std::sync::Arc<Hook>>,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("Writer");
        s.field("staging", &self.staging)
            .field("prefix", &self.prefix)
            .field("patience", &self.patience);
        #[cfg(feature = "test-hooks")]
        s.field("hook", &self.hook.as_ref().map(|_| "..."));
        s.finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Landing {
    Replace,
    CreateOnly,
}

impl Writer {
    /// Temp file beside the target, named with [`TEMP_PREFIX`], transient refusals retried for
    /// [`DEFAULT_PATIENCE`].
    pub fn new() -> Self {
        Self {
            staging: None,
            prefix: TEMP_PREFIX.to_owned(),
            patience: DEFAULT_PATIENCE,
            #[cfg(feature = "test-hooks")]
            hook: None,
        }
    }

    /// Stages the temporary file in `dir` instead of beside the target. `dir` must be on the same
    /// volume as every target written; it is created if missing.
    pub fn staging(mut self, dir: impl Into<PathBuf>) -> Self {
        self.staging = Some(dir.into());
        self
    }

    /// Names temporary files `<prefix><random>` instead of with [`TEMP_PREFIX`], so the app can
    /// recognise its own leftovers — and sweep them with [`sweep_staging_prefixed`]. The prefix
    /// must be a non-empty plain name: a write with an empty prefix, or one containing a path
    /// separator or `..`, fails with [`io::ErrorKind::InvalidInput`].
    pub fn temp_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// How long transient refusals ([`is_transient`]) of the temp file's creation and of the
    /// landing rename are retried. [`Duration::ZERO`] turns retrying off.
    pub fn patience(mut self, patience: Duration) -> Self {
        self.patience = patience;
        self
    }

    /// Replaces `path` with `content`; see [`write_atomic`] and [`write_atomic_staged`].
    pub fn write(&self, path: &Path, content: &[u8]) -> io::Result<()> {
        self.land(path, content, Landing::Replace)
    }

    /// Creates `path` with `content` only if nothing is there; see [`write_atomic_new`].
    pub fn write_new(&self, path: &Path, content: &[u8]) -> io::Result<()> {
        self.land(path, content, Landing::CreateOnly)
    }

    fn land(&self, path: &Path, content: &[u8], landing: Landing) -> io::Result<()> {
        check_prefix(&self.prefix)?;
        let temp_dir = match &self.staging {
            Some(staging) => {
                fs::create_dir_all(staging)?;
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    fs::create_dir_all(dir)?;
                }
                staging.as_path()
            }
            None => {
                let dir = parent_of(path)?;
                fs::create_dir_all(dir)?;
                dir
            }
        };
        // Not the guarantee — the landing below is — but it spares writing content that cannot land.
        if landing == Landing::CreateOnly && occupied(path)? {
            return Err(already_exists(path));
        }

        let tmp = patiently_for(self.patience, || {
            tempfile::Builder::new()
                .prefix(&self.prefix)
                .tempfile_in(temp_dir)
        })?;
        let mut tmp = self.observe(Step::Created, tmp)?;
        tmp.write_all(content)?;
        let tmp = self.observe(Step::Written, tmp)?;
        // Flush to the device, not only the OS cache: without it the rename can reach disk before
        // the data does, and a power loss leaves a renamed but empty file.
        tmp.as_file().sync_all()?;
        let tmp = self.observe(Step::Synced, tmp)?;

        let temp_path = tmp.path().to_path_buf();
        // Each attempt consumes the temp file; a failed one hands it back for the next. When the
        // last attempt fails, dropping it removes the temp file.
        let mut pending = Some(tmp);
        patiently_for(self.patience, || {
            let tmp = pending
                .take()
                .expect("a failed landing hands the temp file back");
            let landed = match landing {
                Landing::Replace => tmp.persist(path),
                Landing::CreateOnly => tmp.persist_noclobber(path),
            };
            landed.map(drop).map_err(|e| {
                pending = Some(e.file);
                e.error
            })
        })?;
        self.observe_landed(Step::Landed, &temp_path)?;
        sync_parent(path)
    }

    #[cfg(not(feature = "test-hooks"))]
    #[inline(always)]
    fn observe(&self, _step: Step, tmp: NamedTempFile) -> io::Result<NamedTempFile> {
        Ok(tmp)
    }

    #[cfg(not(feature = "test-hooks"))]
    #[inline(always)]
    fn observe_landed(&self, _step: Step, _temp_path: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(feature = "test-hooks")]
pub use step::Step;
#[cfg(not(feature = "test-hooks"))]
use step::Step;

mod step {
    /// A point in a write, between creating the temporary file and making the rename durable
    /// (cargo feature `test-hooks`; see [`Writer::observe_steps`](crate::Writer::observe_steps)).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Step {
        /// The temporary file exists and is empty.
        Created,
        /// The content is written to the temporary file but not yet flushed to the device.
        Written,
        /// The content is on the device; the target is unchanged.
        Synced,
        /// The temporary file has been renamed onto the target; on Unix, the directory entry is not
        /// yet flushed. The temporary path no longer exists.
        Landed,
    }
}

#[cfg(feature = "test-hooks")]
type Hook = dyn Fn(Step, &Path) -> io::Result<()> + Send + Sync;

#[cfg(feature = "test-hooks")]
impl Writer {
    /// **Test support** (cargo feature `test-hooks`): calls `hook` at every [`Step`] of each write
    /// made through this `Writer`, with the temporary file's path.
    ///
    /// The hook can look at the disk mid-write — the target still holding its old content after
    /// [`Step::Synced`], say. Returning an error simulates a crash at that point: the write stops
    /// and returns the error, and the temporary file is left on disk as a crash would leave it,
    /// ready for [`sweep_staging`] to find. Only the hook's errors do that; every other failure
    /// still removes the temporary file.
    ///
    /// The hook belongs to this `Writer` value (and its clones), so tests running in parallel do
    /// not see each other's hooks. Enable the feature from `[dev-dependencies]` only: with Cargo's
    /// feature resolver 2 (the default since edition 2021), a feature enabled there is not
    /// compiled into the app's own builds.
    pub fn observe_steps(
        mut self,
        hook: impl Fn(Step, &Path) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.hook = Some(std::sync::Arc::new(hook));
        self
    }

    fn observe(&self, step: Step, tmp: NamedTempFile) -> io::Result<NamedTempFile> {
        let Some(hook) = &self.hook else {
            return Ok(tmp);
        };
        match hook(step, tmp.path()) {
            Ok(()) => Ok(tmp),
            Err(e) => {
                // Leave the temp file behind, as a crash here would.
                let _ = tmp.keep();
                Err(e)
            }
        }
    }

    fn observe_landed(&self, step: Step, temp_path: &Path) -> io::Result<()> {
        match &self.hook {
            Some(hook) => hook(step, temp_path),
            None => Ok(()),
        }
    }
}

fn check_prefix(prefix: &str) -> io::Result<()> {
    let mut parts = Path::new(prefix).components();
    let plain = matches!(parts.next(), Some(Component::Normal(_)))
        && parts.next().is_none()
        && !prefix.contains(['/', '\\']);
    if plain {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("temp file prefix must be a plain, non-empty name: {prefix:?}"),
        ))
    }
}

/// Whether anything — including a symbolic link that points nowhere — is at `path`.
fn occupied(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn already_exists(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} already exists", path.display()),
    )
}

fn parent_of(path: &Path) -> io::Result<&Path> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => Ok(dir),
        Some(_) => Ok(Path::new(".")),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no parent directory",
        )),
    }
}

/// Makes the rename itself durable where the platform buffers directory entries (Unix). Windows
/// has no directory handle to flush; its rename is already recorded by the file system journal.
#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    fs::File::open(parent_of(path)?)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "old").unwrap();
        write_atomic(&f, b"new").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
    }

    #[test]
    fn creates_the_file_and_missing_parents() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x").join("y").join("a.txt");
        write_atomic(&f, b"hi").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "hi");
    }

    #[test]
    fn leaves_nothing_but_the_target_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(names(dir.path()), vec!["a.txt"]);
    }

    #[test]
    fn staged_write_puts_no_temp_file_in_the_target_folder() {
        let root = tempfile::tempdir().unwrap();
        let content = root.path().join("content");
        let staging = root.path().join("staging");
        let f = content.join("page.md");
        write_atomic_staged(&f, b"body", &staging).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "body");
        assert_eq!(names(&content), vec!["page.md"]);
        assert!(names(&staging).is_empty(), "the temp file was renamed away");
    }

    #[test]
    fn a_new_name_move_keeps_the_file_and_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.md"), dir.path().join("b.md"));
        fs::write(&a, "body").unwrap();
        rename_new(&a, &b).unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "body");
        assert_eq!(names(dir.path()), vec!["b.md"]);
    }

    #[test]
    fn a_new_name_move_never_replaces_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.md"), dir.path().join("b.md"));
        fs::write(&a, "mine").unwrap();
        fs::write(&b, "theirs").unwrap();
        let err = rename_new(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&a).unwrap(), "mine");
        assert_eq!(fs::read_to_string(&b).unwrap(), "theirs");
    }

    #[test]
    fn a_new_name_move_fails_when_the_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let err = rename_new(&dir.path().join("gone.md"), &dir.path().join("b.md")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn a_create_only_write_creates_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("sub").join("new.txt");
        write_atomic_new(&f, b"first").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "first");
        assert_eq!(names(&dir.path().join("sub")), vec!["new.txt"]);
    }

    #[test]
    fn a_create_only_write_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "mine").unwrap();
        let started = Instant::now();
        let err = write_atomic_new(&f, b"theirs").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&f).unwrap(), "mine");
        assert_eq!(names(dir.path()), vec!["a.txt"], "the temp file is gone");
        // Only transient refusals are retried: a real answer comes back at once.
        assert!(started.elapsed() < DEFAULT_PATIENCE / 2);
    }

    #[test]
    fn a_create_only_write_does_not_replace_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("taken");
        fs::create_dir(&d).unwrap();
        let err = write_atomic_new(&d, b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(d.is_dir());
    }

    #[test]
    fn a_refused_create_only_write_leaves_nothing_in_staging() {
        // A refusal at the landing itself is covered by the `test-hooks` tests below.
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        let f = root.path().join("a.txt");
        fs::write(&f, "mine").unwrap();
        let err = write_atomic_new_staged(&f, b"theirs", &staging).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&f).unwrap(), "mine");
        assert!(names(&staging).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_create_only_write_treats_a_dangling_symlink_as_taken() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        let err = write_atomic_new(&link, b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!dir.path().join("nowhere").exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_create_only_write_treats_a_dangling_symlink_as_taken() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        // Creating a symbolic link needs a privilege or Developer Mode on Windows.
        match std::os::windows::fs::symlink_file(dir.path().join("nowhere"), &link) {
            Ok(()) => {}
            Err(e)
                if e.kind() == io::ErrorKind::PermissionDenied
                    || e.raw_os_error() == Some(1314) =>
            {
                return;
            }
            Err(e) => panic!("{e}"),
        }
        let err = write_atomic_new(&link, b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(!dir.path().join("nowhere").exists());
    }

    #[test]
    fn an_unusable_prefix_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        for bad in ["", "..", "a/b", "a\\b", "."] {
            let err = Writer::new().temp_prefix(bad).write(&f, b"x").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert!(!f.exists());
    }

    #[test]
    fn sweep_removes_only_its_own_leftovers() {
        let staging = tempfile::tempdir().unwrap();
        fs::write(staging.path().join(format!("{TEMP_PREFIX}abc")), "orphan").unwrap();
        fs::write(staging.path().join("someone-else.tmp"), "keep").unwrap();
        fs::create_dir(staging.path().join(format!("{TEMP_PREFIX}dir"))).unwrap();

        assert_eq!(sweep_staging(staging.path(), Duration::ZERO).unwrap(), 1);
        assert_eq!(
            names(staging.path()),
            vec![format!("{TEMP_PREFIX}dir"), "someone-else.tmp".to_string()]
        );
    }

    #[test]
    fn a_prefixed_sweep_removes_every_listed_prefix_and_nothing_else() {
        let staging = tempfile::tempdir().unwrap();
        for name in [".app-tmp-1", ".app-old-2", "keep.txt", ".tauri-kit-tmp-3"] {
            fs::write(staging.path().join(name), "x").unwrap();
        }
        let removed =
            sweep_staging_prefixed(staging.path(), Duration::ZERO, &[".app-tmp-", ".app-old-"])
                .unwrap();
        assert_eq!(removed, 2);
        assert_eq!(names(staging.path()), vec![".tauri-kit-tmp-3", "keep.txt"]);
    }

    #[test]
    fn a_sweep_without_a_usable_prefix_is_rejected() {
        let staging = tempfile::tempdir().unwrap();
        fs::write(staging.path().join("keep.txt"), "x").unwrap();
        for prefixes in [&[][..], &[""][..], &[".a-", ""][..]] {
            let err = sweep_staging_prefixed(staging.path(), Duration::ZERO, prefixes).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
        assert_eq!(names(staging.path()), vec!["keep.txt"]);
    }

    #[test]
    fn sweeping_a_missing_directory_is_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            sweep_staging(&root.path().join("absent"), Duration::ZERO).unwrap(),
            0
        );
    }

    #[test]
    fn sweep_leaves_a_recent_temp_file_alone() {
        // It may be another running instance's write in progress.
        let staging = tempfile::tempdir().unwrap();
        fs::write(
            staging.path().join(format!("{TEMP_PREFIX}inflight")),
            "part",
        )
        .unwrap();
        assert_eq!(
            sweep_staging(staging.path(), Duration::from_secs(600)).unwrap(),
            0
        );
        assert_eq!(
            names(staging.path()),
            vec![format!("{TEMP_PREFIX}inflight")]
        );
    }

    #[test]
    fn a_bare_file_name_writes_into_the_current_directory() {
        assert_eq!(parent_of(Path::new("a.txt")).unwrap(), Path::new("."));
    }

    fn sharing_violation() -> io::Error {
        io::Error::from_raw_os_error(32)
    }

    #[test]
    fn transient_refusals_are_retried_only_on_windows() {
        let calls = Cell::new(0);
        let result = patiently(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(sharing_violation())
            } else {
                Ok("done")
            }
        });
        if cfg!(windows) {
            assert_eq!(result.unwrap(), "done");
            assert_eq!(calls.get(), 3);
        } else {
            assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
            assert_eq!(calls.get(), 1);
        }
    }

    #[test]
    fn other_errors_are_not_retried() {
        let calls = Cell::new(0);
        let result: io::Result<()> = patiently(|| {
            calls.set(calls.get() + 1);
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn retrying_gives_up_when_patience_runs_out() {
        let calls = Cell::new(0);
        let started = Instant::now();
        let result: io::Result<()> = patiently_for(Duration::from_millis(150), || {
            calls.set(calls.get() + 1);
            Err(sharing_violation())
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
        assert!(started.elapsed() < Duration::from_secs(2));
        if cfg!(windows) {
            assert!(calls.get() > 1);
        }
    }

    #[test]
    fn no_patience_means_one_attempt() {
        let calls = Cell::new(0);
        let _ = patiently_for(Duration::ZERO, || -> io::Result<()> {
            calls.set(calls.get() + 1);
            Err(sharing_violation())
        });
        assert_eq!(calls.get(), 1);
    }

    /// Holds `path` open the way a scanner or indexer does — without letting anyone delete or
    /// rename over it — and lets go after `hold`.
    #[cfg(windows)]
    fn hold_open(path: &Path, hold: Duration) -> thread::JoinHandle<()> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .unwrap();
        thread::spawn(move || {
            thread::sleep(hold);
            drop(file);
        })
    }

    #[cfg(windows)]
    #[test]
    fn a_write_waits_out_a_file_briefly_held_open() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "old").unwrap();
        let holder = hold_open(&f, Duration::from_millis(200));
        write_atomic(&f, b"new").unwrap();
        holder.join().unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(names(dir.path()), vec!["a.txt"]);
    }

    #[cfg(windows)]
    #[test]
    fn without_patience_a_held_file_refuses_the_write() {
        // The premise of the test above: the held file really does refuse the rename.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "old").unwrap();
        let holder = hold_open(&f, Duration::from_millis(500));
        let err = Writer::new()
            .patience(Duration::ZERO)
            .write(&f, b"new")
            .unwrap_err();
        assert!(is_transient(&err), "{err}");
        holder.join().unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "old");
        assert_eq!(names(dir.path()), vec!["a.txt"], "the temp file is gone");
    }

    #[cfg(feature = "test-hooks")]
    mod hooks {
        use super::*;
        use std::sync::{Arc, Mutex};

        #[test]
        fn every_step_is_observed_in_order() {
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("a.txt");
            fs::write(&f, "old").unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&seen);
            let target = f.clone();
            Writer::new()
                .observe_steps(move |step, _tmp| {
                    let content = fs::read_to_string(&target).unwrap();
                    log.lock().unwrap().push((step, content));
                    Ok(())
                })
                .write(&f, b"new")
                .unwrap();
            let seen = seen.lock().unwrap();
            assert_eq!(
                *seen,
                vec![
                    (Step::Created, "old".to_owned()),
                    (Step::Written, "old".to_owned()),
                    (Step::Synced, "old".to_owned()),
                    (Step::Landed, "new".to_owned()),
                ]
            );
        }

        #[test]
        fn a_crash_before_the_landing_leaves_the_target_and_a_sweepable_temp_file() {
            let root = tempfile::tempdir().unwrap();
            let staging = root.path().join("staging");
            let f = root.path().join("a.txt");
            fs::write(&f, "old").unwrap();
            for step in [Step::Created, Step::Written, Step::Synced] {
                let err = Writer::new()
                    .staging(&staging)
                    .temp_prefix(".app-tmp-")
                    .observe_steps(move |at, _| {
                        if at == step {
                            Err(io::Error::other("crash"))
                        } else {
                            Ok(())
                        }
                    })
                    .write(&f, b"new")
                    .unwrap_err();
                assert_eq!(err.to_string(), "crash");
                assert_eq!(fs::read_to_string(&f).unwrap(), "old", "{step:?}");
                let left = names(&staging);
                assert_eq!(left.len(), 1, "{step:?}: {left:?}");
                assert!(left[0].starts_with(".app-tmp-"));
                assert_eq!(
                    sweep_staging_prefixed(&staging, Duration::ZERO, &[".app-tmp-"]).unwrap(),
                    1
                );
            }
        }

        #[test]
        fn a_synced_temp_file_holds_the_whole_content() {
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("a.txt");
            let content = Arc::new(Mutex::new(None));
            let got = Arc::clone(&content);
            let _ = Writer::new()
                .observe_steps(move |step, tmp| {
                    if step == Step::Synced {
                        *got.lock().unwrap() = Some(fs::read(tmp)?);
                        return Err(io::Error::other("crash"));
                    }
                    Ok(())
                })
                .write(&f, b"all of it");
            assert_eq!(content.lock().unwrap().as_deref(), Some(&b"all of it"[..]));
            assert!(!f.exists());
        }

        #[test]
        fn an_ordinary_refusal_still_removes_the_temp_file() {
            // The name is free when the write starts and taken by the time it lands, so the refusal
            // comes from the landing itself, not the early check.
            let root = tempfile::tempdir().unwrap();
            let staging = root.path().join("staging");
            let f = root.path().join("a.txt");
            let target = f.clone();
            let err = Writer::new()
                .staging(&staging)
                .observe_steps(move |step, _| {
                    if step == Step::Synced {
                        fs::write(&target, "raced")?;
                    }
                    Ok(())
                })
                .write_new(&f, b"theirs")
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read_to_string(&f).unwrap(), "raced");
            assert!(names(&staging).is_empty());
        }
    }
}
