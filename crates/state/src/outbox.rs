use std::io;
use std::path::{Path, PathBuf};

/// Writes an app could not make yet, kept as files until they are made.
///
/// While the app runs, such a write can wait in memory and be retried. Kept only there, it is lost
/// when the app closes before the write could land — a folder that stopped answering, a file that
/// changed under it. An outbox keeps each one as a file of its own in a directory the app chooses
/// (usually inside the folder's state directory), so the next run finds it and tries again; the
/// app removes it with [`Outbox::forget`] once it has landed.
///
/// What a kept write holds is the app's: an outbox stores bytes, in files named
/// `<when>-<n>.<extension>` that list in the order they were kept.
///
/// ```
/// use tauri_kit_state::Outbox;
///
/// let dir = tempfile::tempdir()?;
/// let outbox = Outbox::new(dir.path().join("unsaved"), "json");
/// let id = outbox.keep(1_700_000_000_000, br#"{"path":"a.txt","text":"hello"}"#, |path, bytes| {
///     tauri_kit_fs::write_atomic_new(path, bytes)
/// })?;
/// assert_eq!(outbox.list().len(), 1);
/// outbox.forget(&id)?;
/// assert!(outbox.list().is_empty());
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Outbox {
    dir: PathBuf,
    extension: String,
}

impl Outbox {
    /// An outbox in `dir`, whose files end in `.<extension>`. The directory is made by the first
    /// [`keep`](Self::keep) (through the app's `write`); one that does not exist lists as empty.
    pub fn new(dir: impl Into<PathBuf>, extension: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            extension: extension.into(),
        }
    }

    /// The directory the kept writes are in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Keeps `bytes`, kept at `at_ms` (milliseconds since the Unix epoch), and returns its id.
    ///
    /// `write` must create the file and refuse with [`io::ErrorKind::AlreadyExists`] if one is
    /// there — `tauri_kit_fs::write_atomic_new` does. Several writes are often kept at the same
    /// moment (an app closing keeps every pending one at once); each takes the next free id instead
    /// of writing over another.
    pub fn keep(
        &self,
        at_ms: u64,
        bytes: &[u8],
        write: impl Fn(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<String> {
        let mut n = 0u32;
        loop {
            let id = format!("{at_ms:013}-{n}");
            match write(&self.file_of(&id), bytes) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                written => return written.map(|()| id),
            }
        }
    }

    /// Every kept write, oldest first, as its id and bytes. Files the outbox did not name are not
    /// listed; a file that cannot be read is skipped and left where it is — it may be the only copy
    /// of what someone typed.
    pub fn list(&self) -> Vec<(String, Vec<u8>)> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let suffix = format!(".{}", self.extension);
        let mut kept: Vec<(String, Vec<u8>)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let id = name.strip_suffix(&suffix)?.to_string();
                valid_id(&id).then_some(())?;
                let bytes = std::fs::read(entry.path()).ok()?;
                Some((id, bytes))
            })
            .collect();
        kept.sort_by(|a, b| a.0.cmp(&b.0));
        kept
    }

    /// Removes a kept write once it has landed. One already gone is not an error.
    pub fn forget(&self, id: &str) -> io::Result<()> {
        if !valid_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an outbox id",
            ));
        }
        match std::fs::remove_file(self.file_of(id)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn file_of(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.{}", self.extension))
    }
}

/// Ids are digits and dashes — nothing that could name a file anywhere else.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create(path: &Path, bytes: &[u8]) -> io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::File::create_new(path)?.write_all(bytes)
    }

    #[test]
    fn kept_writes_list_oldest_first_and_go_once_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(tmp.path().join("kept"), "json");
        let second = outbox.keep(20, b"b", create).unwrap();
        let first = outbox.keep(10, b"a", create).unwrap();
        let same_moment = outbox.keep(10, b"a again", create).unwrap();
        assert_ne!(first, same_moment);

        let bytes: Vec<Vec<u8>> = outbox.list().into_iter().map(|(_, b)| b).collect();
        assert_eq!(bytes, [b"a".to_vec(), b"a again".to_vec(), b"b".to_vec()]);

        outbox.forget(&first).unwrap();
        outbox.forget(&first).unwrap();
        let bytes: Vec<Vec<u8>> = outbox.list().into_iter().map(|(_, b)| b).collect();
        assert_eq!(bytes, [b"a again".to_vec(), b"b".to_vec()]);
        outbox.forget(&second).unwrap();
    }

    #[test]
    fn ids_and_file_names_are_stable() {
        // Files kept by one release are found by the next.
        let tmp = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(tmp.path(), "json");
        assert_eq!(outbox.keep(10, b"x", create).unwrap(), "0000000000010-0");
        assert!(tmp.path().join("0000000000010-0.json").is_file());
        assert_eq!(outbox.keep(10, b"y", create).unwrap(), "0000000000010-1");
    }

    #[test]
    fn files_the_outbox_did_not_name_are_not_listed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.json"), b"x").unwrap();
        std::fs::write(tmp.path().join("0000000000010-0.txt"), b"x").unwrap();
        assert!(Outbox::new(tmp.path(), "json").list().is_empty());
    }

    #[test]
    fn a_missing_directory_lists_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Outbox::new(tmp.path().join("none"), "json")
            .list()
            .is_empty());
    }

    #[test]
    fn nothing_outside_the_directory_can_be_named() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(tmp.path(), "json");
        assert!(outbox.forget("../settings").is_err());
        assert!(outbox.forget("").is_err());
    }
}
