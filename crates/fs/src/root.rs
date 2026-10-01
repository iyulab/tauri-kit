//! A folder the app works inside, and the paths that stay inside it.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// A folder whose contents the app reads and writes by paths relative to it, and which no such path
/// may leave — a folder the person picked, say, where every file name the app is handed may come
/// from that folder's own contents.
///
/// [`resolve`](Root::resolve) refuses a path with a `..` or `.` component, an absolute path, a
/// drive or UNC prefix, an empty path, and a path that a symbolic link or junction inside the folder
/// leads outside of. A refusal is an error [`is_outside`] recognises.
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// use tauri_kit_fs::Root;
///
/// let root = Root::open("/path/to/picked-folder")?;
/// let file = root.prepare("notes/today.md")?; // creates `notes/` if needed
/// tauri_kit_fs::write_atomic(&file, b"# Today")?;
/// assert!(tauri_kit_fs::is_outside(&root.resolve("../elsewhere.md").unwrap_err()));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// The folder at `path`, by its canonical location (on Windows without the `\\?\` prefix).
    /// Fails with [`io::ErrorKind::NotFound`] if there is no folder there.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Root> {
        let path = path.as_ref();
        let canonical = dunce::canonicalize(path)?;
        if !canonical.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not a folder", path.display()),
            ));
        }
        Ok(Root { path: canonical })
    }

    /// The folder's canonical location.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The location of `rel` inside the folder, or an error [`is_outside`] recognises. `rel` need
    /// not exist; where it does not, the nearest part of it that does is checked.
    pub fn resolve(&self, rel: impl AsRef<Path>) -> io::Result<PathBuf> {
        let rel = rel.as_ref();
        if rel.as_os_str().is_empty()
            || !rel.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(outside(rel));
        }
        let joined = self.path.join(rel);
        // A link inside the folder may lead outside it: check where the nearest existing ancestor
        // (or the path itself) really is.
        let mut probe = joined.as_path();
        loop {
            match fs::symlink_metadata(probe) {
                Ok(_) => {
                    let real = dunce::canonicalize(probe)?;
                    if !real.starts_with(&self.path) {
                        return Err(outside(rel));
                    }
                    return Ok(joined);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            match probe.parent() {
                Some(parent) => probe = parent,
                None => return Err(outside(rel)),
            }
        }
    }

    /// Like [`resolve`](Root::resolve), and creates the missing folders on the way to `rel`, so a
    /// file can be written there. The path is checked again once they exist: creating them may
    /// have followed a link that another program put in place meanwhile.
    pub fn prepare(&self, rel: impl AsRef<Path>) -> io::Result<PathBuf> {
        let rel = rel.as_ref();
        let path = self.resolve(rel)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.resolve(rel)
    }
}

/// Whether [`Root::resolve`] or [`Root::prepare`] refused a path for leaving the folder.
pub fn is_outside(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|inner| inner.is::<Outside>())
}

#[derive(Debug)]
struct Outside(PathBuf);

impl fmt::Display for Outside {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is not a path inside the folder", self.0.display())
    }
}

impl std::error::Error for Outside {}

fn outside(rel: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, Outside(rel.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::open(dir.path()).unwrap();
        (dir, root)
    }

    fn refused(root: &Root, rel: &str) -> bool {
        root.resolve(rel).is_err_and(|e| is_outside(&e))
    }

    #[test]
    fn resolves_plain_relative_paths_that_need_not_exist() {
        let (_dir, root) = root();
        let p = root.resolve("forms/bug report.md").unwrap();
        assert!(p.starts_with(root.path()));
        assert!(p.ends_with("forms/bug report.md"));
    }

    #[test]
    fn refuses_dot_components() {
        let (_dir, root) = root();
        for rel in ["..", "../x.md", "a/../../x.md", "a/../b.md", "./a.md"] {
            assert!(refused(&root, rel), "{rel} should be refused");
        }
        // A `.` after the first component is dropped when the path is read, so it names the same
        // place as without it — inside.
        assert!(root.resolve("a/./b.md").is_ok());
    }

    #[test]
    fn refuses_absolute_and_empty_paths() {
        let (dir, root) = root();
        let abs = dir.path().join("a.md");
        for rel in ["", "/a.md", abs.to_str().unwrap()] {
            assert!(refused(&root, rel), "{rel:?} should be refused");
        }
        #[cfg(windows)]
        for rel in [r"C:\a.md", "C:a.md", r"\\server\share\a.md"] {
            assert!(refused(&root, rel), "{rel:?} should be refused");
        }
    }

    #[test]
    fn other_errors_are_not_outside() {
        let err = Root::open("/surely/not/a/folder/here").unwrap_err();
        assert!(!is_outside(&err));
    }

    #[test]
    fn prepare_creates_the_folders_on_the_way() {
        let (_dir, root) = root();
        let p = root.prepare("a/b/c.md").unwrap();
        assert!(p.parent().unwrap().is_dir());
        assert!(!p.exists());
    }

    #[test]
    fn a_file_is_not_a_root() {
        let (dir, _root) = root();
        let file = dir.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        assert_eq!(
            Root::open(&file).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_that_leave_the_folder() {
        let (_dir, root) = root();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(refused(&root, "escape/a.md"));
        assert!(root.prepare("escape/a.md").is_err_and(|e| is_outside(&e)));
    }

    #[cfg(windows)]
    #[test]
    fn refuses_junctions_that_leave_the_folder() {
        let (_dir, root) = root();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("escape");
        // A directory junction needs no privilege, unlike a symbolic link.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink /J failed");
        assert!(refused(&root, "escape/a.md"));
        assert!(root.prepare("escape/a.md").is_err_and(|e| is_outside(&e)));
    }
}
