//! The report file: one report per line, appended by a launch, trimmed before it grows too large.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::Report;

/// A launch that fails this often is failing the same way; more reports would say nothing new.
/// The reports a [`Reporter`] writes by default.
pub const MAX_REPORTS: usize = 50;

/// The kind of the one last report a [`Reporter`] writes once it has written its most.
pub const CAPPED_KIND: &str = "ReportsCapped";

/// How large a report file may grow before [`trim`] cuts it, as a default to pass it: several
/// hundred reports, which only a device failing on every launch for a long time would write.
pub const MAX_FILE_BYTES: usize = 1 << 20;

/// The reports of one launch, appended to a file (JSON Lines) where the person can read them —
/// exactly what would be sent.
///
/// Each failure — the same layer, kind and first frame — is written once per `Reporter`, and a
/// `Reporter` writes at most [`MAX_REPORTS`] (see [`Reporter::max_reports`]); past that, one last
/// report of kind [`CAPPED_KIND`] says the rest were left out. Make one per launch.
#[derive(Debug)]
pub struct Reporter {
    file: PathBuf,
    max_reports: usize,
    written: Mutex<Written>,
}

/// What a launch has written: which failures, and how many reports.
#[derive(Debug, Default)]
struct Written {
    failures: HashSet<(String, String, Option<String>)>,
    count: usize,
}

impl Reporter {
    /// A reporter that appends to `file`, creating it and its folders when it first writes.
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Reporter {
            file: file.into(),
            max_reports: MAX_REPORTS,
            written: Mutex::default(),
        }
    }

    /// Writes at most `max` reports, then the one that says the rest were left out, instead of
    /// [`MAX_REPORTS`].
    pub fn max_reports(mut self, max: usize) -> Self {
        self.max_reports = max;
        self
    }

    /// The file reports are appended to.
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Writes the report unless this failure — the same layer, kind and first frame — was written
    /// already by this reporter, or the reporter has written its most. Returns whether a report was
    /// written.
    pub fn record(&self, report: Report) -> io::Result<bool> {
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if written.count > self.max_reports {
            return Ok(false);
        }
        let report = if written.count == self.max_reports {
            report.of_kind(CAPPED_KIND)
        } else {
            let failure = (
                report.layer.clone(),
                report.kind.clone(),
                report.frames.first().cloned(),
            );
            if !written.failures.insert(failure) {
                return Ok(false);
            }
            report
        };
        self.append(&report)?;
        written.count += 1;
        Ok(true)
    }

    fn append(&self, report: &Report) -> io::Result<()> {
        use std::io::Write;
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut line = serde_json::to_string(report).map_err(io::Error::other)?;
        line.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)?
            .write_all(line.as_bytes())
    }
}

/// Keeps a report file from growing without end.
///
/// Once `file` holds more than `max_bytes`, the oldest whole reports go until it holds at most half
/// of that, and the offset recorded in `sent` (how far into `file` has been sent, a decimal number;
/// see `Sink::send_pending` with the `appinsights` feature) moves back by what went. `sent` is
/// written first — a launch that stops between the two sends a few reports again rather than skip
/// any — and `file` is replaced crash-safely. Call it before the launch writes or sends anything.
/// A missing `file` is nothing to trim. Returns how many bytes were dropped.
pub fn trim(file: &Path, sent: &Path, max_bytes: usize) -> io::Result<usize> {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    if bytes.len() <= max_bytes {
        return Ok(0);
    }
    // Keep from the first report that starts at or after the cut, so it starts with a whole one.
    let cut = bytes.len() - max_bytes / 2;
    let dropped = match bytes[cut - 1..].iter().position(|&b| b == b'\n') {
        Some(i) => cut + i,
        None => bytes.len(),
    };
    if let Some(offset) = read_offset(sent) {
        std::fs::write(sent, offset.saturating_sub(dropped).to_string())?;
    }
    tauri_kit_fs::write_atomic(file, &bytes[dropped..])?;
    Ok(dropped)
}

/// The offset `sent` records, if it records one.
pub(crate) fn read_offset(sent: &Path) -> Option<usize> {
    std::fs::read_to_string(sent)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameRule, Layer};

    fn written(dir: &tempfile::TempDir) -> Vec<serde_json::Value> {
        std::fs::read_to_string(dir.path().join("reports.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn ui() -> Layer {
        Layer::new("ui", FrameRule::web_bundle())
    }

    #[test]
    fn writes_each_failure_once_a_launch() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter::new(dir.path().join("reports.jsonl"));
        let failure = || {
            Report::new(
                &ui(),
                "TypeError",
                "at save (http://tauri.localhost/assets/index-a.js:1:2)",
                "1.2.3",
            )
        };

        assert!(reporter.record(failure()).unwrap());
        assert!(!reporter.record(failure()).unwrap());
        assert!(reporter
            .record(Report::new(
                &ui(),
                "TypeError",
                "at open (http://tauri.localhost/assets/index-a.js:9:9)",
                "1.2.3",
            ))
            .unwrap());
        // The same kind and frame in another layer is another failure.
        assert!(reporter
            .record(Report::new(
                &Layer::new("panel", FrameRule::web_bundle()),
                "TypeError",
                "at save (http://tauri.localhost/assets/index-a.js:1:2)",
                "1.2.3",
            ))
            .unwrap());

        let lines = written(&dir);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["layer"], "ui");
        assert_eq!(lines[0]["kind"], "TypeError");
        assert_eq!(lines[0]["frames"][0], "save index-a.js:1:2");
        assert_eq!(lines[0]["version"], "1.2.3");
        assert_eq!(lines[2]["layer"], "panel");
    }

    #[test]
    fn says_once_when_a_launch_has_reported_enough() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter::new(dir.path().join("reports.jsonl"));
        let host = Layer::new("host", FrameRule::dotnet_method());
        for i in 0..MAX_REPORTS + 10 {
            reporter
                .record(Report::new(&host, &format!("Failure{i}"), "A.B", "1.2.3"))
                .unwrap();
        }
        let lines = written(&dir);
        assert_eq!(lines.len(), MAX_REPORTS + 1);
        assert_eq!(lines[MAX_REPORTS]["kind"], CAPPED_KIND);
        assert_eq!(lines[MAX_REPORTS]["layer"], "host");
        assert_eq!(lines[MAX_REPORTS]["version"], "1.2.3");
        assert_eq!(lines[MAX_REPORTS]["frames"], serde_json::json!([]));
    }

    #[test]
    fn writes_at_most_the_reports_it_is_told() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter::new(dir.path().join("reports.jsonl")).max_reports(2);
        for i in 0..5 {
            reporter
                .record(Report::new(&ui(), &format!("Failure{i}"), "", "1.2.3"))
                .unwrap();
        }
        let kinds: Vec<_> = written(&dir).iter().map(|l| l["kind"].clone()).collect();
        assert_eq!(kinds, ["Failure0", "Failure1", CAPPED_KIND]);
    }

    #[test]
    fn creates_the_folder_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("logs").join("reports.jsonl");
        let reporter = Reporter::new(&file);
        assert!(reporter
            .record(Report::new(&ui(), "TypeError", "", "1.2.3"))
            .unwrap());
        assert_eq!(reporter.file(), file);
        assert!(file.is_file());
    }

    #[test]
    fn trims_the_oldest_reports_once_the_file_passes_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = (
            dir.path().join("reports.jsonl"),
            dir.path().join("reports.sent"),
        );
        let lines: Vec<String> = (0..10).map(|n| format!("{{\"n\":{n:03}}}\n")).collect();
        let text = lines.concat();
        std::fs::write(&file, &text).unwrap();
        // Seven reports were sent before this launch.
        let sent_at = lines[..7].concat().len();
        std::fs::write(&sent, sent_at.to_string()).unwrap();

        // At or under the limit, nothing changes.
        assert_eq!(trim(&file, &sent, text.len()).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

        // Over it, whole reports go from the front until half the limit is left.
        let line = lines[0].len();
        let dropped = trim(&file, &sent, 8 * line).unwrap();
        assert_eq!(dropped, 6 * line);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), lines[6..].concat());
        // What was sent moved back with them: the next send starts at the same report.
        let at: usize = std::fs::read_to_string(&sent).unwrap().parse().unwrap();
        assert_eq!(at, sent_at - dropped);
        assert_eq!(
            &std::fs::read_to_string(&file).unwrap()[at..],
            lines[7..].concat()
        );
    }

    #[test]
    fn trimming_past_what_was_sent_starts_the_next_send_at_the_first_kept_report() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = (
            dir.path().join("reports.jsonl"),
            dir.path().join("reports.sent"),
        );
        std::fs::write(&file, "{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n").unwrap();
        std::fs::write(&sent, "0").unwrap();
        trim(&file, &sent, 16).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"n\":3}\n");
        assert_eq!(std::fs::read_to_string(&sent).unwrap(), "0");
        // No file yet: nothing to do.
        assert_eq!(trim(&dir.path().join("none"), &sent, 16).unwrap(), 0);
    }
}
