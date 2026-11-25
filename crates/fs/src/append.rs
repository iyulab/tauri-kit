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

/// Appends `line` as [`append_line`] does, to a log that is kept within `max_bytes`: when the
/// file has grown past it, the oldest lines are dropped, keeping the newest whole lines that fit in
/// half of it, so that the file is rewritten once per half its size rather than at each line.
///
/// For a log that is an aid — counts kept on the device, not a record anything depends on. The
/// rewrite goes through [`write_atomic`](crate::write_atomic), so a crash leaves the file as it was
/// before or after, never cut; a line another writer appends during the rewrite can be lost. A
/// reader that needs to know where the log now starts reads its first line.
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// use std::path::Path;
/// tauri_kit_fs::append_line_within(Path::new("shown.jsonl"), r#"{"n":1}"#, 4 << 20)?;
/// # Ok(())
/// # }
/// ```
pub fn append_line_within(path: &Path, line: &str, max_bytes: u64) -> io::Result<()> {
    append_line(path, line)?;
    if fs::metadata(path)?.len() <= max_bytes {
        return Ok(());
    }
    let content = fs::read(path)?;
    let keep = (max_bytes / 2) as usize;
    // The newest whole lines that fit: from the end, back to the first line start within `keep`.
    let mut start = content.len().saturating_sub(keep);
    if start > 0 && content[start - 1] != b'\n' {
        start = match content[start..].iter().position(|b| *b == b'\n') {
            Some(i) => start + i + 1,
            None => content.len(),
        };
    }
    crate::write_atomic(path, &content[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_within_a_size_keeps_its_newest_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("shown.jsonl");
        // Ten lines of ten bytes each (nine and a newline), within 64 bytes.
        for n in 0..10 {
            append_line_within(&log, &format!("line-{n:04}"), 64).unwrap();
        }
        let kept = fs::read_to_string(&log).unwrap();
        assert!(kept.len() as u64 <= 64);
        assert!(
            kept.ends_with("line-0009\n"),
            "the newest line is kept: {kept:?}"
        );
        assert!(
            kept.split_terminator('\n')
                .all(|l| l.starts_with("line-") && l.len() == 9),
            "only whole lines: {kept:?}"
        );
        assert!(!kept.contains("line-0000"), "the oldest went: {kept:?}");
    }

    #[test]
    fn a_log_under_its_size_is_only_appended_to() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("shown.jsonl");
        append_line_within(&log, "a", 64).unwrap();
        append_line_within(&log, "b", 64).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "a\nb\n");
    }

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
