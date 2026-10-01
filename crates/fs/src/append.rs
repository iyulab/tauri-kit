//! Appending lines to a log one writes to and never rewrites.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Appends `line` and a newline to the file at `path`, creating the file and its missing parent
/// directories if needed.
///
/// The line and its newline go out in one write and are flushed to the device before this
/// returns. A crash in the middle can still leave a partial last line, so a reader should skip a
/// last line it cannot parse. A line that holds a line break would read back as two, so it is
/// refused with [`io::ErrorKind::InvalidInput`] and nothing is written. Transient refusals
/// ([`is_transient`](crate::is_transient)) of opening the file are retried for
/// [`DEFAULT_PATIENCE`](crate::DEFAULT_PATIENCE).
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// use std::path::Path;
/// tauri_kit_fs::append_line(Path::new("events/this-device.jsonl"), r#"{"n":1}"#)?;
/// # Ok(())
/// # }
/// ```
pub fn append_line(path: &Path, line: &str) -> io::Result<()> {
    if line.contains(['\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a line cannot hold a line break",
        ));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut file =
        crate::patiently(|| fs::OpenOptions::new().create(true).append(true).open(path))?;
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_whole_lines_and_creates_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events").join("a.jsonl");
        append_line(&log, r#"{"n":1}"#).unwrap();
        append_line(&log, r#"{"n":2}"#).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "{\"n\":1}\n{\"n\":2}\n");
    }

    #[test]
    fn refuses_a_line_with_a_line_break() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("a.jsonl");
        for line in ["a\nb", "a\rb"] {
            let err = append_line(&log, line).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
        assert!(!log.exists());
    }
}
