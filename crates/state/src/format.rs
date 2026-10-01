use std::io;
use std::path::{Path, PathBuf};

/// The file in a state directory that holds its format number.
pub const FORMAT_FILE: &str = "format";

/// One step between formats: it brings the files in the state directory it is given from format
/// `i` to `i + 1`.
///
/// The format number is written only after the last step, so a step cut short — a crash, a full
/// disk — runs again the next time. Every step must therefore be safe to run twice, and must write
/// what it makes before it removes what it replaces.
pub type Step = fn(&Path) -> io::Result<()>;

/// The format of the files in a state directory, as [`Versioned::read`] finds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A format this release knows — the current one or an older one.
    Known(u32),
    /// Newer than this release, or a format file that does not hold a number. Either way, not this
    /// release's to rewrite.
    Unknown,
}

/// The formats an app's state directories have been in, as the steps between them.
///
/// The current format is the number of steps: an app that has never changed its files has one
/// step, from format 0 (files written before the directory said what format it was in) to format 1,
/// that does nothing.
///
/// ```
/// use tauri_kit_state::{Format, Step, Versioned};
///
/// const STEPS: [Step; 1] = [|_| Ok(())];
/// const FORMATS: Versioned = Versioned::new(&STEPS);
///
/// let dir = tempfile::tempdir()?;
/// assert_eq!(FORMATS.read(dir.path())?, Format::Known(0));
/// assert!(FORMATS.bring_up_to_date(dir.path(), |path, bytes| std::fs::write(path, bytes))?);
/// assert_eq!(FORMATS.read(dir.path())?, Format::Known(1));
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Versioned<'s> {
    steps: &'s [Step],
}

impl<'s> Versioned<'s> {
    /// The formats reached by `steps`, in order: `steps[i]` brings format `i` to `i + 1`.
    pub const fn new(steps: &'s [Step]) -> Self {
        Self { steps }
    }

    /// The format this release writes.
    pub fn current(&self) -> u32 {
        self.steps.len() as u32
    }

    /// The format of the files in `dir`. A directory that never said — or does not exist yet — is
    /// in format 0.
    pub fn read(&self, dir: &Path) -> io::Result<Format> {
        match std::fs::read_to_string(dir.join(FORMAT_FILE)) {
            Ok(text) => Ok(match text.trim().parse::<u32>() {
                Ok(n) if n <= self.current() => Format::Known(n),
                // A format file that is not a number is not taken for format 0: that would
                // "bring up to date", and so rewrite, whatever is there.
                _ => Format::Unknown,
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Format::Known(0)),
            Err(e) => Err(e),
        }
    }

    /// Whether this release may write into `dir`: not when its format is [unknown](Format::Unknown).
    /// An app that may not should go on with its defaults and write nothing there.
    pub fn writable(&self, dir: &Path) -> io::Result<bool> {
        Ok(self.read(dir)? != Format::Unknown)
    }

    /// Brings the files in `dir` to the current format, running the steps from its format on and
    /// then writing the format number with `write` (which should write atomically — see
    /// `tauri_kit_fs::write_atomic`). Returns `false`, having changed nothing, when the format is
    /// [unknown](Format::Unknown).
    pub fn bring_up_to_date(
        &self,
        dir: &Path,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<bool> {
        let from = match self.read(dir)? {
            Format::Unknown => return Ok(false),
            Format::Known(n) if n == self.current() => return Ok(true),
            Format::Known(n) => n,
        };
        for step in &self.steps[from as usize..] {
            step(dir)?;
        }
        write(
            &dir.join(FORMAT_FILE),
            self.current().to_string().as_bytes(),
        )?;
        Ok(true)
    }
}

/// Moves `dir/rel`, a file that cannot be read, out of the way and keeps it — as
/// `<rel>.unreadable-<now_secs>`, or with `-2`, `-3`, … when that is taken — so the app can go on
/// with its defaults instead of writing them over the file, and the file is still there for
/// someone to look at. Returns where it went. Never replaces anything.
pub fn set_aside(dir: &Path, rel: &str, now_secs: u64) -> io::Result<PathBuf> {
    let from = dir.join(rel);
    let mut to = dir.join(format!("{rel}.unreadable-{now_secs}"));
    let mut n = 1;
    loop {
        match tauri_kit_fs::rename_new(&from, &to) {
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists && n <= tauri_kit_fs::CLAIM_TRIES =>
            {
                n += 1;
                to = dir.join(format!("{rel}.unreadable-{now_secs}-{n}"));
            }
            moved => return moved.map(|()| to),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const STEPS: [Step; 1] = [|_| Ok(())];
    const FORMATS: Versioned = Versioned::new(&STEPS);

    fn plain_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, bytes)
    }

    #[test]
    fn a_directory_that_never_said_is_brought_to_the_current_format() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        assert_eq!(FORMATS.read(&dir).unwrap(), Format::Known(0));
        assert!(FORMATS.bring_up_to_date(&dir, plain_write).unwrap());
        assert_eq!(std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap(), "1");
        assert_eq!(FORMATS.read(&dir).unwrap(), Format::Known(1));
    }

    #[test]
    fn bringing_up_to_date_again_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        FORMATS.bring_up_to_date(tmp.path(), plain_write).unwrap();
        let wrote = Cell::new(false);
        assert!(FORMATS
            .bring_up_to_date(tmp.path(), |_, _| {
                wrote.set(true);
                Ok(())
            })
            .unwrap());
        assert!(!wrote.get());
    }

    #[test]
    fn steps_run_from_the_format_found_in_order() {
        const STEPS: [Step; 3] = [
            |dir| std::fs::write(dir.join("log"), "0"),
            |dir| {
                let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
                std::fs::write(dir.join("log"), log + "1")
            },
            |dir| {
                let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
                std::fs::write(dir.join("log"), log + "2")
            },
        ];
        let formats = Versioned::new(&STEPS);
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(FORMAT_FILE), "1").unwrap();
        assert!(formats.bring_up_to_date(tmp.path(), plain_write).unwrap());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("log")).unwrap(),
            "12"
        );
        assert_eq!(formats.read(tmp.path()).unwrap(), Format::Known(3));
    }

    #[test]
    fn a_step_that_fails_leaves_the_format_where_it_was() {
        const STEPS: [Step; 2] = [|_| Ok(()), |_| Err(io::Error::other("disk full"))];
        let formats = Versioned::new(&STEPS);
        let tmp = tempfile::tempdir().unwrap();
        assert!(formats.bring_up_to_date(tmp.path(), plain_write).is_err());
        assert_eq!(formats.read(tmp.path()).unwrap(), Format::Known(0));
    }

    #[test]
    fn a_newer_format_is_left_exactly_as_it_is_and_not_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join(FORMAT_FILE), "2").unwrap();
        std::fs::write(dir.join("order.json"), b"{\"from\":\"a newer release\"}").unwrap();

        assert!(!FORMATS.bring_up_to_date(dir, plain_write).unwrap());
        assert_eq!(std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap(), "2");
        assert_eq!(
            std::fs::read(dir.join("order.json")).unwrap(),
            b"{\"from\":\"a newer release\"}"
        );
        assert!(!FORMATS.writable(dir).unwrap());
    }

    #[test]
    fn a_format_file_that_is_not_a_number_is_unknown_not_old() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(FORMAT_FILE), "two").unwrap();
        assert_eq!(FORMATS.read(tmp.path()).unwrap(), Format::Unknown);
        assert!(!FORMATS.writable(tmp.path()).unwrap());
    }

    #[test]
    fn an_unreadable_file_is_set_aside_with_its_bytes_and_never_over_another() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("favorites.json"), b"{half written").unwrap();
        let first = set_aside(dir, "favorites.json", 1_700_000_000).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"{half written");
        assert!(!dir.join("favorites.json").exists());

        std::fs::write(dir.join("favorites.json"), b"again").unwrap();
        let second = set_aside(dir, "favorites.json", 1_700_000_000).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"{half written");
        assert_eq!(std::fs::read(&second).unwrap(), b"again");
    }
}
