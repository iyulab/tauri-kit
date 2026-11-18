//! What a panic leaves behind before the process ends.
//!
//! A desktop app built with `panic = "abort"` (the usual choice: unwinding across a web view's
//! native interface is not safe) ends on a panic with nothing but an exit code — the message, the
//! location and the stack are gone, and the next panic of the same kind is as mute as the first.
//! [`PanicHook`] keeps them:
//!
//! - **a log on the device** ([`PanicHook::log`]): the time, the thread, the panic's message, its
//!   location and a backtrace, appended to a file of the app's choosing. It is the person's own file,
//!   like any log the app keeps; nothing in it leaves the device. When the file grows past
//!   [`PanicHook::max_log_bytes`] it is moved aside to `<name>.1` (replacing the previous one) and
//!   a new one is started.
//! - **a content-free report** ([`PanicHook::report`]): a [`Report`] of kind `Panic` built from the
//!   location and the backtrace's frames the layer's rule recognises — never the message — written
//!   by a [`Reporter`] like any other failure, for an app that sends reports.
//!
//! The hook runs the hook that was installed before it afterwards, so the default message on
//! standard error is kept. It never panics itself: a log or report that cannot be written is
//! skipped.

use std::backtrace::Backtrace;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Layer, Report, Reporter};

/// The size past which a panic log is moved aside, unless [`PanicHook::max_log_bytes`] says otherwise.
pub const MAX_PANIC_LOG_BYTES: u64 = 1024 * 1024;

/// A panic hook that writes a log entry and, optionally, a content-free report before the process
/// ends. Build it, then [`install`](PanicHook::install) it first thing in `main`.
///
/// ```no_run
/// use std::sync::Arc;
/// use tauri_kit_diagnostics::{FrameRule, Layer, PanicHook, Reporter};
///
/// PanicHook::new()
///     .log("/path/to/logs/shell-panic.log")
///     .report(Arc::new(Reporter::new("/path/to/logs/reports/shell.jsonl")),
///             Layer::new("shell", FrameRule::rust_source()), env!("CARGO_PKG_VERSION"))
///     .install();
/// ```
#[derive(Debug, Default)]
pub struct PanicHook {
    log: Option<PathBuf>,
    max_log_bytes: Option<u64>,
    report: Option<(Arc<Reporter>, Layer, String)>,
}

impl PanicHook {
    /// A hook that keeps nothing until [`log`](Self::log) or [`report`](Self::report) is given.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends each panic — time, thread, message, location, backtrace — to `file`. The folder is
    /// made if it is missing.
    pub fn log(mut self, file: impl Into<PathBuf>) -> Self {
        self.log = Some(file.into());
        self
    }

    /// Moves the log aside once it is larger than `max` bytes, instead of [`MAX_PANIC_LOG_BYTES`].
    pub fn max_log_bytes(mut self, max: u64) -> Self {
        self.max_log_bytes = Some(max);
        self
    }

    /// Also records a [`Report`] of kind `Panic` for `layer` through `reporter`: the location and
    /// the frames the layer's rule recognises, never the message.
    pub fn report(
        mut self,
        reporter: Arc<Reporter>,
        layer: Layer,
        version: impl Into<String>,
    ) -> Self {
        self.report = Some((reporter, layer, version.into()));
        self
    }

    /// Installs the hook for the whole process, in front of the one installed before it (which
    /// still runs afterwards).
    pub fn install(self) {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            self.keep(info);
            previous(info);
        }));
    }

    /// What the hook does with one panic; separate from [`install`](Self::install) so that it can
    /// be called on its own.
    pub fn keep(&self, info: &PanicHookInfo<'_>) {
        let backtrace = Backtrace::force_capture();
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        if let Some(file) = &self.log {
            let entry = log_entry(
                &utc_now(),
                std::thread::current().name().unwrap_or("<unnamed>"),
                &message_of(info),
                &location,
                &backtrace.to_string(),
            );
            // Best effort: a log that cannot be written must not turn a panic into a second one.
            let _ = append(
                file,
                self.max_log_bytes.unwrap_or(MAX_PANIC_LOG_BYTES),
                &entry,
            );
        }
        if let Some((reporter, layer, version)) = &self.report {
            let stack = format!("{location}\n{backtrace}");
            let _ = reporter.record(Report::new(layer, "Panic", &stack, version));
        }
    }
}

/// The panic's message: its payload when that is text, otherwise a note that it is not.
fn message_of(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "(the panic's payload is not text)".to_owned()
    }
}

/// One log entry, as written.
fn log_entry(time: &str, thread: &str, message: &str, location: &str, backtrace: &str) -> String {
    format!("=== panic {time} thread '{thread}' at {location}\n{message}\n{backtrace}\n")
}

fn append(file: &Path, max: u64, entry: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(file).map(|m| m.len() > max).unwrap_or(false) {
        let mut aside = file.as_os_str().to_owned();
        aside.push(".1");
        fs::rename(file, PathBuf::from(aside))?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(file)?;
    f.write_all(entry.as_bytes())?;
    f.flush()
}

/// The current time in UTC, to the second (`2026-11-19T03:10:51Z`).
fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_counted_from_the_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_411), (2025, 11, 19));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn a_log_past_its_size_is_moved_aside_and_a_new_one_started() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("logs").join("panic.log");
        append(&file, 10, "first entry, longer than ten bytes\n").unwrap();
        append(&file, 10, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "second\n");
        assert_eq!(
            fs::read_to_string(dir.path().join("logs").join("panic.log.1")).unwrap(),
            "first entry, longer than ten bytes\n"
        );
    }
}
