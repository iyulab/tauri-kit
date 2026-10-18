//! Error reports for desktop apps that carry no content: what failed and where in the app's own
//! code, never what the failure said.
//!
//! **Messages are never taken.** An exception's message, a panic's payload, a file path in a stack
//! — any of them can hold a path to the person's files, a file name, a person's name or a value
//! they typed, and none of that may leave the device, whatever the person has agreed to. So a
//! [`Report`] is built from an allowlist only, never by removing what looks private:
//!
//! - **layer**: a name the app gives each part of itself that fails on its own (its UI, its Rust
//!   process, a helper process) — a [`Layer`].
//! - **kind**: a type or class name, or a code of the app's own, kept only if it is a plain
//!   identifier — whole, or replaced by [`UNRECOGNIZED_KIND`].
//! - **details**: facts the app names ([`Report::detail`]) — a status code, an error code of its
//!   own — each kept only if its value is a plain identifier or a whole number.
//! - **frames**: the lines of a stack the layer's [`FrameRule`] recognises as the app's own code,
//!   rewritten to a file name and position or a method name; every other line is dropped whole.
//!   Rules are provided for a web bundle the app serves ([`WebBundle`]), the app's Rust source
//!   ([`RustSource`]) and .NET methods ([`FrameRule::DotNetMethod`]).
//! - **version**, **os**, **arch** and **time** (UTC, to the second).
//!
//! A [`Reporter`] appends the reports of one launch to a JSON Lines file the person can read —
//! exactly what would be sent — writing each failure once and at most [`MAX_REPORTS`]. [`trim`]
//! keeps that file from growing without end. [`Sink`] makes what goes out to an Azure Application
//! Insights resource, as exception telemetry; with the `appinsights` feature it also sends what the
//! file gained since the last send and keeps what could not go out for the next launch. An app that
//! sends with an HTTP client of its own reads the same with [`unsent`] and records what went with
//! [`mark_sent`].
//!
//! ```no_run
//! use std::path::Path;
//! use tauri_kit_diagnostics::{trim, FrameRule, Layer, Report, Reporter, MAX_FILE_BYTES};
//!
//! let dir = Path::new("/path/to/logs");
//! let (file, sent) = (dir.join("reports.jsonl"), dir.join("reports.sent"));
//! // Before this launch writes or sends anything: the file is evidence, not an archive.
//! trim(&file, &sent, MAX_FILE_BYTES)?;
//! let reporter = Reporter::new(&file);
//!
//! let ui = Layer::new("ui", FrameRule::web_bundle());
//! let shell = Layer::new("shell", FrameRule::rust_source());
//! const VERSION: &str = env!("CARGO_PKG_VERSION");
//!
//! // What the UI caught: a class name and a stack. The message in the stack's first line goes.
//! reporter.record(Report::new(
//!     &ui,
//!     "TypeError",
//!     "TypeError: cannot read 'title' of undefined\n    at save (http://tauri.localhost/assets/index-a1.js:3:120)",
//!     VERSION,
//! ))?;
//! // A panic: where it happened, not what it said.
//! reporter.record(Report::new(&shell, "Panic", "src/main.rs:42:9", VERSION))?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # Sessions
//!
//! The same file-then-send path carries when the app starts and ends and which of its screens it
//! spent time on — never what a screen showed or who the person is. A [`Session`] appends a
//! [`SessionEvent`] when it starts, when it ends as it should, and, on the next start, for a
//! session that did not end (a crash, a lost machine). Its fields are an allowlist too: a random
//! [`InstallId`] made on the device (never from its hardware), a random session id, the version,
//! the platform, the time, the interface's language, the app's own name for its edition, and time
//! on each of the app's [`Screens`] — a name not among them is recorded as [`UNRECOGNIZED_KIND`],
//! whole. [`Sink::session_items`] makes Application Insights users, sessions and page views of
//! them, and [`Sink`] sends a sessions file as it sends a reports file ([`Telemetry`], [`pending`]).
//! Whether an app may send them, and what it tells the person, is the app's to decide.
//!
//! # Events
//!
//! What the app wants to count — a feature used, a step that failed and which of a few ways — it
//! declares in a [`Registry`] it compiles in: each event's name and a few fields, each of a closed kind
//! ([`FieldKind`]: a choice among declared values, some of them, a count that is bucketed or capped,
//! true or false, a duration). [`Events`] writes only what the registry declares, to its own JSON Lines
//! file, each line with the session's random id — not the installation's — and drops what does not
//! fit, saying so with [`DROPPED_EVENT`] (never with the undeclared name or value). What a sender reads
//! back goes through [`Registry::verify`] ([`pending_events`]), so a line edited into the file is not
//! sent; [`Sink::event_item`] makes the Application Insights item.
//!
//! # Features
//!
//! - `appinsights` (off by default): `Sink::send_pending`, which sends queued reports to Application Insights
//!   over HTTPS with [`ureq`](https://docs.rs/ureq) — the OS's TLS and certificate store, and the
//!   system proxy on Windows. Without it the crate makes no network requests and has no HTTP
//!   dependency — [`Sink`] still makes the items, for an HTTP client of the app's own.
//! - `appinsights-rustls` (off by default): the same sending, with rustls doing the handshake
//!   against the OS's certificate store — for an app that keeps OpenSSL out of its dependency
//!   graph (native TLS is OpenSSL on Linux, so `appinsights` puts `openssl-sys` in the lock file).
//!   With both on, `appinsights` decides.

mod event;
mod queue;
mod report;
mod session;
mod sink;

#[cfg(any(feature = "appinsights", feature = "appinsights-rustls"))]
mod appinsights;

pub use event::{
    pending_events, Event, EventLine, EventName, Events, FieldKind, Recorded, Registry,
    RegistryError, Value, Why, DROPPED_EVENT, MAX_COUNT, MAX_DURATION, MAX_FIELDS, MAX_PER_EVENT,
    MAX_VALUES,
};
pub use queue::{
    mark_sent, pending, trim, unsent, Batch, Pending, Reporter, CAPPED_KIND, MAX_FILE_BYTES,
    MAX_REPORTS,
};
pub use report::{
    FrameRule, Layer, Report, RustSource, WebBundle, MAX_DETAILS, MAX_FRAMES, UNRECOGNIZED_KIND,
};
pub use session::{
    Ended, InstallId, ScreenTime, Screens, Session, SessionBuilder, SessionEvent, SessionKind,
    MAX_SCREENS,
};
pub use sink::{Sink, Telemetry};

/// The HTTP client [`Sink`] sends with, so an app can build its own agent with the same version.
#[cfg(any(feature = "appinsights", feature = "appinsights-rustls"))]
pub use ureq;
