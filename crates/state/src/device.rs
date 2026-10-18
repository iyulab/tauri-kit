use std::io;
use std::path::{Path, PathBuf};

/// The file, in the app's data directory, that [`InstallId::in_dir`] keeps the identifier in.
pub const INSTALL_ID_FILE: &str = "install-id";

/// A random identifier of this installation of the app: made on the device the first time it is
/// needed, from the operating system's random generator, and kept in a file.
///
/// It is never derived from the hardware, the account or anything else on the device, so it names
/// an installation and nothing more: deleting the file makes a new one. It is 32 lowercase
/// hexadecimal digits.
///
/// An app has one: the diagnostics crate takes this same type, so what an app's own records call
/// the installation and what its reports call it are the same value as long as both read it from
/// the same file — [`InstallId::in_dir`] names that file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallId(String);

impl InstallId {
    /// The identifier kept in `file`, or a new one written there (crash-safely, with its folders)
    /// when `file` is missing or holds anything other than an identifier.
    pub fn load_or_create(file: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(file) {
            Ok(text) if is_id(text.trim()) => return Ok(InstallId(text.trim().to_string())),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let id = random_id()?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        tauri_kit_fs::write_atomic(file, id.as_bytes())?;
        Ok(InstallId(id))
    }

    /// The identifier kept in [`INSTALL_ID_FILE`] in the app's data directory `dir`, made there
    /// the first time.
    pub fn in_dir(dir: &Path) -> io::Result<Self> {
        Self::load_or_create(&dir.join(INSTALL_ID_FILE))
    }

    /// The identifier, as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn is_id(raw: &str) -> bool {
    raw.len() == 32 && raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What [`relocate`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Relocated {
    /// There was nothing at the old place: a fresh install, or one already moved.
    Nothing,
    /// The old directory became the new one, whole.
    Moved,
    /// The new place already holds something, so the old directory was left where it is — the app
    /// has run under its new identifier before, and what it wrote there is not overwritten.
    Kept {
        /// The directory left in place.
        old: PathBuf,
    },
    /// The old directory could not be renamed (another volume, a file held open), so its contents
    /// were copied into the new place and removed from the old one — all but `left`, the entries
    /// (relative to the old directory) that could not be removed. The copy is whole either way.
    Copied {
        /// Entries still at the old place.
        left: Vec<PathBuf>,
    },
}

/// Moves an app's data directory from where an earlier identifier kept it (`old`) to where the
/// current one does (`new`), once: call it at start-up, before anything opens a file in either.
///
/// Nothing is overwritten. When `new` already exists and is not empty, `old` stays where it is
/// ([`Relocated::Kept`]); an empty `new` — a directory made before the move ran — is taken as
/// absent. The rename is tried first, which is one step on the same volume; when it is refused,
/// the contents are copied (each file crash-safely) and only then removed from `old`, so a move cut
/// short leaves everything readable at one place or the other, and running it again finishes it.
pub fn relocate(old: &Path, new: &Path) -> io::Result<Relocated> {
    match std::fs::symlink_metadata(old) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(Relocated::Nothing),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Relocated::Nothing),
        Err(e) => return Err(e),
    }
    match std::fs::read_dir(new) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Ok(Relocated::Kept {
                    old: old.to_path_buf(),
                });
            }
            std::fs::remove_dir(new)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(old, new).is_ok() {
        return Ok(Relocated::Moved);
    }
    copy_tree(old, new)?;
    let mut left = Vec::new();
    remove_tree(old, Path::new(""), &mut left);
    Ok(Relocated::Copied { left })
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            tauri_kit_fs::write_atomic(&target, &std::fs::read(entry.path())?)?;
        }
        // A link is not followed: what it points at is not the app's to carry along.
    }
    Ok(())
}

/// Removes what it can under `dir`, recording in `left` what it could not (relative to the
/// directory `relocate` was handed).
fn remove_tree(dir: &Path, rel: &Path, left: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        left.push(rel.to_path_buf());
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let here = rel.join(entry.file_name());
        let removed = match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                remove_tree(&path, &here, left);
                continue;
            }
            Ok(_) => std::fs::remove_file(&path).is_ok(),
            Err(_) => false,
        };
        if !removed {
            left.push(here);
        }
    }
    if std::fs::remove_dir(dir).is_err() && !left.iter().any(|p| p.starts_with(rel)) {
        left.push(rel.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_install_id_is_made_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstallId::in_dir(dir.path()).unwrap();
        assert!(is_id(first.as_str()));
        assert_eq!(InstallId::in_dir(dir.path()).unwrap(), first);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(INSTALL_ID_FILE)).unwrap(),
            first.as_str()
        );
    }

    #[test]
    fn a_file_that_is_not_an_id_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(INSTALL_ID_FILE), "not an id").unwrap();
        let id = InstallId::in_dir(dir.path()).unwrap();
        assert!(is_id(id.as_str()));
    }

    #[test]
    fn nothing_at_the_old_place_is_nothing_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new");
        assert_eq!(
            relocate(&dir.path().join("old"), &new).unwrap(),
            Relocated::Nothing
        );
        assert!(!new.exists());
    }

    #[test]
    fn the_old_directory_becomes_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("com.example.old");
        std::fs::create_dir_all(old.join("folders/a")).unwrap();
        std::fs::write(old.join("settings.json"), "{}").unwrap();
        std::fs::write(old.join("folders/a/state"), "x").unwrap();
        let new = dir.path().join("deeper/com.example.new");
        assert_eq!(relocate(&old, &new).unwrap(), Relocated::Moved);
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(new.join("folders/a/state")).unwrap(),
            "x"
        );
        // Run again (the next start-up): nothing left to do.
        assert_eq!(relocate(&old, &new).unwrap(), Relocated::Nothing);
    }

    #[test]
    fn an_empty_new_directory_does_not_stop_the_move() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), "{}").unwrap();
        let new = dir.path().join("new");
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(relocate(&old, &new).unwrap(), Relocated::Moved);
        assert!(new.join("settings.json").exists());
    }

    #[test]
    fn what_the_new_place_holds_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("settings.json"), "old").unwrap();
        std::fs::write(new.join("settings.json"), "new").unwrap();
        assert_eq!(
            relocate(&old, &new).unwrap(),
            Relocated::Kept { old: old.clone() }
        );
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(old.join("settings.json")).unwrap(),
            "old"
        );
    }

    #[test]
    fn a_copy_carries_everything_and_reports_what_stayed() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        std::fs::create_dir_all(old.join("sub")).unwrap();
        std::fs::write(old.join("a"), "1").unwrap();
        std::fs::write(old.join("sub/b"), "2").unwrap();
        let new = dir.path().join("new");
        copy_tree(&old, &new).unwrap();
        let mut left = Vec::new();
        remove_tree(&old, Path::new(""), &mut left);
        assert!(left.is_empty(), "{left:?}");
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(new.join("sub/b")).unwrap(), "2");
    }
}
