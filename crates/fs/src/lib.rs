//! Crash-safe file writes for desktop apps.
//!
//! Writing a file in place truncates it first, so a crash, a power loss or a full disk in the middle
//! of the write leaves the user with a half-written or empty file. The remedy is well known and easy
//! to get subtly wrong: write the new content to a temporary file on the same volume, flush it to
//! the device, rename it over the target, and — on platforms where the rename itself is buffered —
//! flush the directory too. After [`write_atomic`] returns, the target holds either its old content
//! or the new content, never a mix.
//!
//! Where the temporary file lives matters to apps that watch or sync the folder they write into:
//! a temp file beside the target shows up in file watchers, sync clients and version control as a
//! short-lived extra file. [`write_atomic_staged`] takes a staging directory the app chooses instead
//! (it must be on the same volume as the target, or the rename fails with an error rather than
//! silently copying). Temp files left behind by a crash are removed with [`sweep_staging`], which
//! only ever touches files this crate created.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use std::path::Path;
//! tauri_kit_fs::write_atomic(Path::new("settings.json"), br#"{"theme":"dark"}"#)?;
//! # Ok(())
//! # }
//! ```

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Prefix of every temporary file this crate creates. [`sweep_staging`] removes only files that
/// carry it, so a staging directory can be shared with other tools without losing their files.
pub const TEMP_PREFIX: &str = ".tauri-kit-tmp-";

/// Replaces `path` with `content` so that a crash leaves either the old or the new content.
///
/// The temporary file is created beside the target, which guarantees the same volume. Missing
/// parent directories are created.
pub fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let dir = parent_of(path)?;
    fs::create_dir_all(dir)?;
    write_via(dir, path, content)
}

/// Like [`write_atomic`], but stages the temporary file in `staging` instead of beside the target.
///
/// Use this when the target's folder is watched or synced and a transient extra file there would be
/// noticed. `staging` must be on the same volume as `path`: a rename cannot cross volumes, and this
/// function returns that error instead of falling back to a non-atomic copy. `staging` is created if
/// missing.
pub fn write_atomic_staged(path: &Path, content: &[u8], staging: &Path) -> io::Result<()> {
    fs::create_dir_all(staging)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    write_via(staging, path, content)
}

/// Removes temporary files left in `staging` by writes that never finished (a crash or power loss
/// between creating the temp file and renaming it). Call it once at startup.
///
/// Only files named with [`TEMP_PREFIX`] are removed. A missing directory is not an error. Files
/// that cannot be removed — for example one still held open by another running instance — are
/// skipped. Returns how many files were removed.
pub fn sweep_staging(staging: &Path) -> io::Result<usize> {
    let entries = match fs::read_dir(staging) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let ours = entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX);
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if ours && is_file && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
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

fn write_via(temp_dir: &Path, path: &Path, content: &[u8]) -> io::Result<()> {
    let mut tmp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(temp_dir)?;
    tmp.write_all(content)?;
    // Flush to the device, not only the OS cache: without it the rename can reach disk before the
    // data does, and a power loss leaves a renamed but empty file.
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    sync_parent(path)
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
        let f = content.join("note.md");
        write_atomic_staged(&f, b"body", &staging).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "body");
        assert_eq!(names(&content), vec!["note.md"]);
        assert!(names(&staging).is_empty(), "the temp file was renamed away");
    }

    #[test]
    fn sweep_removes_only_its_own_leftovers() {
        let staging = tempfile::tempdir().unwrap();
        fs::write(staging.path().join(format!("{TEMP_PREFIX}abc")), "orphan").unwrap();
        fs::write(staging.path().join("someone-else.tmp"), "keep").unwrap();
        fs::create_dir(staging.path().join(format!("{TEMP_PREFIX}dir"))).unwrap();

        assert_eq!(sweep_staging(staging.path()).unwrap(), 1);
        assert_eq!(
            names(staging.path()),
            vec![format!("{TEMP_PREFIX}dir"), "someone-else.tmp".to_string()]
        );
    }

    #[test]
    fn sweeping_a_missing_directory_is_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(sweep_staging(&root.path().join("absent")).unwrap(), 0);
    }

    #[test]
    fn a_bare_file_name_writes_into_the_current_directory() {
        assert_eq!(parent_of(Path::new("a.txt")).unwrap(), Path::new("."));
    }
}
