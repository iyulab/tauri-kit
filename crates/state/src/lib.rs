//! State a desktop app keeps for each folder a person opens — kept outside that folder.
//!
//! Apps that work on a folder of the person's files (a project, a library, a set of notes) need
//! somewhere to put what belongs to the app rather than to the files: preferences that only mean
//! something for one folder, work not yet saved, a list of recent places. Putting it inside the
//! folder adds files nobody asked for, and they travel wherever the folder does. Keeping it beside
//! the app's other data takes three things this crate gives:
//!
//! - **A name for each folder** — [`folder_key`] turns a folder's path into a directory name that
//!   is recognisable by eye and unambiguous, the same for every way of writing the same path, and
//!   the same from one release to the next.
//! - **A format that older releases leave alone** — [`Versioned`] keeps a format number in the
//!   state directory, brings older formats up to date one step at a time, and refuses to write over
//!   a format it does not know: going back to an older release must not rewrite what a newer one
//!   wrote. [`set_aside`] moves a file that cannot be read out of the way instead of losing it.
//! - **Writes kept until they land** — an [`Outbox`] holds writes the app could not make yet as
//!   files, so they survive the app closing, and are tried again the next time.
//!
//! Where the state directories live is the app's choice; this crate names the one inside it:
//!
//! ```no_run
//! use std::path::Path;
//! use tauri_kit_state::folder_key;
//!
//! # fn app_data_dir() -> std::path::PathBuf { unimplemented!() }
//! let folder = Path::new("Projects/garden");
//! let state = app_data_dir().join("folders").join(folder_key(folder));
//! ```

mod format;
mod key;
mod outbox;

pub use format::{set_aside, Format, Step, Versioned, FORMAT_FILE};
pub use key::folder_key;
pub use outbox::Outbox;
