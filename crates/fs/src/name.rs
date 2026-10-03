//! Finding a free name in a folder — `report.txt`, then `report (1).txt`, `report (2).txt`, … —
//! and taking it.
//!
//! Two things go wrong when an app does this by hand. `Path::exists` follows symbolic links, so a
//! link that points nowhere reads as free, yet creating or moving onto it is refused: the app picks
//! the same name again and again. And a name that was free when it was picked can be taken before
//! it is used — by a sync client, by another program — so the app either replaces what just
//! arrived or fails. [`is_taken`] counts anything at the path, dangling links included, and
//! [`claim_free_path`] moves on to the next free name when the one it picked is taken first.

use crate::conflict::split_extension;
use std::io;
use std::path::{Path, PathBuf};

/// How a name is numbered when it is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind<'a> {
    /// The number goes before the last extension: `notes.md` → `notes (1).md`. A name that starts
    /// with its only dot (`.profile`) has no extension: `.profile (1)`.
    File,
    /// The number goes before this ending, for files whose kind is told by more than their last
    /// extension: `notes.fd.md` with `.fd.md` → `notes (1).fd.md`, where `File` would give
    /// `notes.fd (1).md` — a name the app no longer reads as its kind. A name that does not end with
    /// it, or is nothing but it, is numbered as `File`.
    Suffix(&'a str),
    /// The number goes at the end, dots and all: `2026.archive` → `2026.archive (1)`.
    Folder,
}

/// How many times [`claim_free_path`] looks for a free name again after the one it picked was
/// taken before it could be used. Each retry means something else took a name within a moment;
/// past this, something refuses every name — a disagreement about what "taken" means — and looping
/// would never end.
pub const CLAIM_TRIES: u32 = 16;

/// Whether anything is at `path` — a file, a folder, or a symbolic link, including one that points
/// nowhere. Creating or moving onto any of these is refused, so this is the test of whether a name
/// is free; [`Path::exists`] follows links and calls a dangling one absent.
pub fn is_taken(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// The first path in `dir` not [taken](is_taken): `dir/name` itself, else `name (1)`, `name (2)`,
/// … numbered as `kind` says.
///
/// The answer is only true at the moment it is given. To create or move something under it, use
/// [`claim_free_path`], which handles the name being taken in between.
///
/// ```
/// use tauri_kit_fs::{free_path, NameKind};
///
/// let dir = tempfile::tempdir()?;
/// std::fs::write(dir.path().join("notes.md"), "")?;
/// assert_eq!(free_path(dir.path(), "notes.md", NameKind::File), dir.path().join("notes (1).md"));
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn free_path(dir: &Path, name: &str, kind: NameKind<'_>) -> PathBuf {
    let first = dir.join(name);
    if !is_taken(&first) {
        return first;
    }
    let (stem, ext) = match kind {
        NameKind::Suffix(suffix) if name.len() > suffix.len() && name.ends_with(suffix) => {
            name.split_at(name.len() - suffix.len())
        }
        NameKind::File | NameKind::Suffix(_) => split_extension(name),
        NameKind::Folder => (name, ""),
    };
    (1u64..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !is_taken(candidate))
        .expect("a folder cannot hold every numbered name")
}

/// Creates something under the first free name in `dir` and returns where it went, with what
/// `create` returned.
///
/// `create` takes the path and must refuse with [`io::ErrorKind::AlreadyExists`] if anything is
/// there — [`std::fs::File::create_new`], [`std::fs::create_dir`], [`write_atomic_new`](crate::write_atomic_new),
/// [`rename_new`](crate::rename_new) all do. When it refuses, the name was taken after it was found
/// free, and the next free name is tried; any other error is returned as it is. After
/// [`CLAIM_TRIES`] such refusals in a row the last one is returned.
///
/// ```
/// use tauri_kit_fs::{claim_free_path, NameKind};
///
/// let dir = tempfile::tempdir()?;
/// std::fs::write(dir.path().join("Untitled.md"), "")?;
/// let (path, ()) = claim_free_path(dir.path(), "Untitled.md", NameKind::File, |p| {
///     tauri_kit_fs::write_atomic_new(p, b"# Hello")
/// })?;
/// assert_eq!(path, dir.path().join("Untitled (1).md"));
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn claim_free_path<T>(
    dir: &Path,
    name: &str,
    kind: NameKind<'_>,
    mut create: impl FnMut(&Path) -> io::Result<T>,
) -> io::Result<(PathBuf, T)> {
    let mut refused = 0;
    loop {
        let path = free_path(dir, name, kind);
        match create(&path) {
            Ok(made) => return Ok((path, made)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && refused < CLAIM_TRIES => {
                refused += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_name_itself_when_free() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            free_path(dir.path(), "a.md", NameKind::File),
            dir.path().join("a.md")
        );
    }

    #[test]
    fn a_file_is_numbered_before_its_last_extension() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.draft.md"), "").unwrap();
        fs::write(dir.path().join("a.draft (1).md"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), "a.draft.md", NameKind::File),
            dir.path().join("a.draft (2).md")
        );
    }

    #[test]
    fn a_file_is_numbered_before_the_suffix_its_kind_is_told_by() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("New form.fd.md"), "").unwrap();
        fs::write(dir.path().join("New form (1).fd.md"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), "New form.fd.md", NameKind::Suffix(".fd.md")),
            dir.path().join("New form (2).fd.md")
        );
    }

    #[test]
    fn a_name_without_the_suffix_is_numbered_as_a_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.md"), "").unwrap();
        fs::write(dir.path().join(".fd.md"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), "notes.md", NameKind::Suffix(".fd.md")),
            dir.path().join("notes (1).md")
        );
        assert_eq!(
            free_path(dir.path(), ".fd.md", NameKind::Suffix(".fd.md")),
            dir.path().join(".fd (1).md")
        );
    }

    #[test]
    fn a_leading_dot_is_not_an_extension() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".profile"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), ".profile", NameKind::File),
            dir.path().join(".profile (1)")
        );
    }

    #[test]
    fn a_folder_is_numbered_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("2026.archive")).unwrap();
        assert_eq!(
            free_path(dir.path(), "2026.archive", NameKind::Folder),
            dir.path().join("2026.archive (1)")
        );
    }

    #[test]
    fn a_name_without_extension_is_numbered_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("README"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), "README", NameKind::File),
            dir.path().join("README (1)")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("a.md");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        assert!(!link.exists());
        assert!(is_taken(&link));
        assert_eq!(
            free_path(dir.path(), "a.md", NameKind::File),
            dir.path().join("a (1).md")
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_dangling_link_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("a.md");
        // Creating a symbolic link needs a privilege or developer mode; without it there is
        // nothing to test here.
        if std::os::windows::fs::symlink_file(dir.path().join("nowhere"), &link).is_err() {
            return;
        }
        assert!(!link.exists());
        assert!(is_taken(&link));
        assert_eq!(
            free_path(dir.path(), "a.md", NameKind::File),
            dir.path().join("a (1).md")
        );
    }

    #[test]
    fn a_name_taken_before_it_is_used_moves_on_to_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = true;
        let (path, ()) = claim_free_path(dir.path(), "a.md", NameKind::File, |p| {
            if first {
                // Another program takes the name between the check and the create.
                first = false;
                fs::write(p, "theirs").unwrap();
            }
            fs::File::create_new(p).map(drop)
        })
        .unwrap();
        assert_eq!(path, dir.path().join("a (1).md"));
        assert_eq!(
            fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "theirs"
        );
    }

    #[test]
    fn a_refusal_of_every_name_ends() {
        let dir = tempfile::tempdir().unwrap();
        let mut calls = 0;
        let err = claim_free_path(dir.path(), "a.md", NameKind::File, |_| -> io::Result<()> {
            calls += 1;
            Err(io::ErrorKind::AlreadyExists.into())
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(calls, CLAIM_TRIES + 1);
    }

    #[test]
    fn another_error_is_returned_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut calls = 0;
        let err = claim_free_path(dir.path(), "a.md", NameKind::File, |_| -> io::Result<()> {
            calls += 1;
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_folder_moves_under_a_free_name_without_replacing() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("src");
        fs::create_dir(&from).unwrap();
        fs::write(from.join("inside.md"), "x").unwrap();
        fs::create_dir(dir.path().join("dest")).unwrap();
        let (path, ()) = claim_free_path(dir.path(), "dest", NameKind::Folder, |p| {
            crate::rename_new(&from, p)
        })
        .unwrap();
        assert_eq!(path, dir.path().join("dest (1)"));
        assert!(path.join("inside.md").is_file());
    }
}
