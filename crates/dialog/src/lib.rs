//! One route for an app's open and save dialogs, which a test build can answer.
//!
//! An end-to-end run of the built app stops at the first native dialog: the page cannot reach it, and
//! the dialog belongs to the operating system. So the app opens every dialog through [`answer_or`],
//! handing it what it asks and a closure that shows the app's own dialog (with whatever dialog library
//! it uses). In a normal build that closure is all there is.
//!
//! Built with the `test-answers` feature — only for builds made for tests — a run started with the
//! environment variable [`ANSWERS`] naming a file takes each answer from that file instead, and shows
//! nothing:
//!
//! - The file holds one answer per line, as JSON, in the order the dialogs will open:
//!   `{"kind":"save","path":"…"}`, `{"kind":"open","paths":["…"]}` (one path for [`Kind::Open`],
//!   any number for [`Kind::OpenMany`]), `{"kind":"folder","path":"…"}`, or `{"kind":"cancel"}`.
//!   The test may append answers while the app runs; nothing in it is rewritten.
//! - Each dialog asked is appended, as JSON, to the same path with `.asked` added:
//!   `{"kind","title","defaultPath","filters":[{"name","extensions"}]}` — so a test can check what
//!   was asked (the default file name, the filters), and the n-th dialog takes the n-th answer.
//! - A dialog with no answer left, or an answer of another kind than the dialog, is an [`Error`],
//!   never a dialog shown: a run with nobody at the screen fails where it went wrong instead of
//!   waiting on a window nobody sees.

use std::path::{Path, PathBuf};

/// The environment variable that names the answers file of a test run (with the `test-answers` feature).
pub const ANSWERS: &str = "TAURI_KIT_DIALOG_ANSWERS";

/// What a dialog picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Where to save a file.
    Save,
    /// One file to open.
    Open,
    /// Files to open, any number.
    OpenMany,
    /// A folder.
    Folder,
}

impl Kind {
    /// The name the answers and the asked log use.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Save => "save",
            Kind::Open => "open",
            Kind::OpenMany => "open-many",
            Kind::Folder => "folder",
        }
    }
}

/// A group of file types a dialog offers: its name as shown, and the extensions, without the dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filter<'a> {
    pub name: &'a str,
    pub extensions: &'a [&'a str],
}

/// What the app asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ask<'a> {
    pub kind: Kind,
    /// The dialog's title.
    pub title: &'a str,
    /// The file or folder the dialog starts at, or proposes (a save's file name).
    pub default_path: Option<&'a Path>,
    pub filters: &'a [Filter<'a>],
}

/// What the dialog answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The paths picked — one, except for [`Kind::OpenMany`].
    Paths(Vec<PathBuf>),
    /// The dialog was closed without a pick.
    Cancelled,
}

/// Why a test build could not answer a dialog. A normal build never gives one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The dialog opened with no answer left for it.
    Unanswered { kind: Kind, title: String },
    /// The next answer is for another kind of dialog, or has the wrong number of paths.
    Mismatch { kind: Kind, answer: String },
    /// The answers file could not be read, or the asked log written.
    Queue(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unanswered { kind, title } => {
                write!(f, "unanswered dialog: {} \"{title}\"", kind.name())
            }
            Error::Mismatch { kind, answer } => {
                write!(
                    f,
                    "a {} dialog cannot take the answer {answer}",
                    kind.name()
                )
            }
            Error::Queue(message) => write!(f, "dialog answers: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Answers `ask` by calling `show`, the app's own dialog — or, in a test run of a build with the
/// `test-answers` feature, with the next answer from the file [`ANSWERS`] names (see the crate docs).
// Without the feature, what is asked is the app's own dialog's to show.
#[cfg_attr(not(feature = "test-answers"), allow(unused_variables))]
pub fn answer_or(ask: &Ask<'_>, show: impl FnOnce() -> Answer) -> Result<Answer, Error> {
    #[cfg(feature = "test-answers")]
    if let Some(file) = std::env::var_os(ANSWERS).filter(|f| !f.is_empty()) {
        return answers::take(Path::new(&file), ask);
    }
    Ok(show())
}

#[cfg(feature = "test-answers")]
mod answers;
