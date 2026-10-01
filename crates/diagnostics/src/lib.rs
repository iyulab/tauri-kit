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
//! - **frames**: the lines of a stack the layer's [`FrameRule`] recognises as the app's own code,
//!   rewritten to a file name and position or a method name; every other line is dropped whole.
//!   Rules are provided for a web bundle the app serves ([`WebBundle`]), the app's Rust source
//!   ([`RustSource`]) and .NET methods ([`FrameRule::DotNetMethod`]).
//! - **version**, **os**, **arch** and **time** (UTC, to the second).
//!
//! A [`Reporter`] appends the reports of one launch to a JSON Lines file the person can read —
//! exactly what would be sent — writing each failure once and at most [`MAX_REPORTS`]. [`trim`]
//! keeps that file from growing without end. With the `appinsights` feature, `Sink` sends what the
//! file gained since the last send to an Azure Application Insights resource, as exception
//! telemetry, and keeps what could not go out for the next launch.
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
//! # Features
//!
//! - `appinsights` (off by default): `Sink`, which sends queued reports to Application Insights
//!   over HTTPS with [`ureq`](https://docs.rs/ureq) — the OS's TLS and certificate store, and the
//!   system proxy on Windows. Without it the crate makes no network requests and has no HTTP
//!   dependency.

mod queue;
mod report;

#[cfg(feature = "appinsights")]
mod appinsights;

pub use queue::{trim, Reporter, CAPPED_KIND, MAX_FILE_BYTES, MAX_REPORTS};
pub use report::{FrameRule, Layer, Report, RustSource, WebBundle, MAX_FRAMES, UNRECOGNIZED_KIND};

#[cfg(feature = "appinsights")]
pub use appinsights::Sink;

/// The HTTP client [`Sink`] sends with, so an app can build its own agent with the same version.
#[cfg(feature = "appinsights")]
pub use ureq;
