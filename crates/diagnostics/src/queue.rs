//! A segment's file: one line per report or event, appended by one process, read back in stretches.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::Report;

/// A launch that fails this often is failing the same way; more reports would say nothing new.
/// The reports a [`Reporter`] writes by default.
pub const MAX_REPORTS: usize = 50;

/// The kind of the one last report a [`Reporter`] writes once it has written its most.
pub const CAPPED_KIND: &str = "ReportsCapped";

/// How many reports one [`Batch`] holds at most — what goes out in one request.
pub(crate) const BATCH: usize = 100;

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
        append_line(&self.file, report)
    }
}

/// Appends `item` to `file` as one JSON line, creating the file and its folders when needed.
pub(crate) fn append_line<T: Serialize>(file: &Path, item: &T) -> io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(item).map_err(io::Error::other)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(line.as_bytes())
}

/// Reports of a file that have not been sent, as they go out in one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// The reports, in the order they were written — at most 100. Empty when every line of the
    /// stretch was something other than a report; such a batch is still recorded as sent.
    pub reports: Vec<Report>,
    /// How far into the file this batch reaches, in bytes: what to record with [`mark_sent`] once
    /// the batch is handled.
    pub end: usize,
}

/// The reports a segment's `file` has gained since the offset `sent` records, each verified by
/// `registry` ([`Registry::verify_report`](crate::Registry::verify_report) — a line that does not verify
/// is passed over), in the batches they go out in.
///
/// For an app that sends with an HTTP client of its own — with the `appinsights` feature,
/// `Sink::send_pending` does all of it. Send the batches in order and record each one with
/// [`mark_sent`] once it is handled; stop at the first that is not, so it and those after it are
/// read again next time. It is [`pending_reports`](crate::pending_reports) as batches.
///
/// Only whole lines are read: a last line without its newline (one still being written) is left
/// for later, and a line that is not a report is passed over. A `file` shorter than what `sent`
/// records is a new file and is read from its start; a missing `file` has nothing unsent.
pub fn unsent(registry: &crate::Registry, file: &Path, sent: &Path) -> io::Result<Vec<Batch>> {
    Ok(crate::pending_reports(registry, file, sent)?
        .into_iter()
        .map(|p| Batch {
            reports: p.items,
            end: p.end,
        })
        .collect())
}

/// Lines of a file that have not been sent, read as `T`, as they go out in one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending<T> {
    /// The lines that read as `T`, in the order they were written — at most 100. Empty when no
    /// line of the stretch did; such a stretch is still recorded as sent.
    pub items: Vec<T>,
    /// How far into the file this stretch reaches, in bytes: what to record with [`mark_sent`]
    /// once it is handled.
    pub end: usize,
}

/// What a file of `T` lines — reports, session events — has gained since the offset `sent`
/// records, in the stretches it goes out in. [`unsent`] is this for reports; read it the same way.
pub fn pending<T: DeserializeOwned>(file: &Path, sent: &Path) -> io::Result<Vec<Pending<T>>> {
    use std::io::{Read, Seek};
    let mut pending = match std::fs::File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let len = usize::try_from(pending.metadata()?.len()).map_err(io::Error::other)?;
    // A file shorter than what was sent is a new file: the old one was deleted.
    let mut offset = read_offset(sent)
        .filter(|&offset| offset <= len)
        .unwrap_or(0);
    // Only what follows what was sent is read: a segment's file only grows.
    pending.seek(io::SeekFrom::Start(offset as u64))?;
    let mut bytes = Vec::new();
    pending.read_to_end(&mut bytes)?;
    // Only whole lines: a launch may still be writing the last one.
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let lines: Vec<&[u8]> = bytes[..end].split_inclusive(|&b| b == b'\n').collect();
    Ok(lines
        .chunks(BATCH)
        .map(|batch| {
            offset += batch.iter().map(|line| line.len()).sum::<usize>();
            Pending {
                items: batch
                    .iter()
                    .filter_map(|line| serde_json::from_slice(line).ok())
                    .collect(),
                end: offset,
            }
        })
        .collect())
}

/// Records in `sent` that its report file has been sent up to `end` — a [`Batch::end`].
pub fn mark_sent(sent: &Path, end: usize) -> io::Result<()> {
    std::fs::write(sent, end.to_string())
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
    fn unsent_reads_what_followed_the_last_send_and_mark_sent_moves_past_it() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = (
            dir.path().join("reports.jsonl"),
            dir.path().join("reports.sent"),
        );
        assert_eq!(
            unsent(&crate::Registry::default(), &file, &sent).unwrap(),
            Vec::new(),
            "a missing file"
        );

        let reporter = Reporter::new(&file).max_reports(BATCH + 10);
        for i in 0..BATCH + 1 {
            reporter
                .record(Report::new(&ui(), &format!("Failure{i}"), "", "1.2.3"))
                .unwrap();
        }
        let batches = unsent(&crate::Registry::default(), &file, &sent).unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].reports.len(), BATCH);
        assert_eq!(batches[1].reports[0].kind, format!("Failure{BATCH}"));
        let len = std::fs::metadata(&file).unwrap().len() as usize;
        assert!(batches[0].end < len);
        assert_eq!(batches[1].end, len);

        // The first batch handled, the second not: only the second is read again.
        mark_sent(&sent, batches[0].end).unwrap();
        let again = unsent(&crate::Registry::default(), &file, &sent).unwrap();
        assert_eq!(again, batches[1..].to_vec());
        mark_sent(&sent, again[0].end).unwrap();
        assert_eq!(
            unsent(&crate::Registry::default(), &file, &sent).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn unsent_passes_over_lines_that_are_not_reports_and_leaves_a_line_still_being_written() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = (
            dir.path().join("reports.jsonl"),
            dir.path().join("reports.sent"),
        );
        let report = serde_json::to_string(&Report::new(&ui(), "RangeError", "", "1.2.3")).unwrap();
        std::fs::write(&file, format!("not a report\n{report}\n{{\"layer\":\"ui\"")).unwrap();
        let batches = unsent(&crate::Registry::default(), &file, &sent).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].reports.len(), 1);
        assert_eq!(batches[0].end, "not a report\n".len() + report.len() + 1);

        // A stretch of nothing but other lines is still a batch, so it can be recorded as sent.
        std::fs::write(&file, "not a report\n").unwrap();
        std::fs::remove_file(&sent).unwrap_or_default();
        let batches = unsent(&crate::Registry::default(), &file, &sent).unwrap();
        assert_eq!(batches.len(), 1);
        assert!(batches[0].reports.is_empty());
    }

    #[test]
    fn a_file_shorter_than_what_was_sent_is_read_from_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = (
            dir.path().join("reports.jsonl"),
            dir.path().join("reports.sent"),
        );
        mark_sent(&sent, 999_999).unwrap();
        Reporter::new(&file)
            .record(Report::new(&ui(), "TypeError", "", "1.2.3"))
            .unwrap();
        assert_eq!(
            unsent(&crate::Registry::default(), &file, &sent).unwrap()[0]
                .reports
                .len(),
            1
        );
    }
}
