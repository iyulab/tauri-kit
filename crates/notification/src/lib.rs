//! A native desktop notification for an installed app.
//!
//! The web page's own `Notification` API is not a route an installed app can lean on: on macOS the system
//! web view answers `denied` without asking, so a finished background task never reaches a person whose
//! window is behind another. This crate gives the app the operating system's own route and, with it, an
//! honest [`Permission`] — a value to read, ask for, and explain ("notifications are off in System
//! Settings") instead of a failure nobody sees.
//!
//! What it does not do: it supplies no words (a [`Notice`]'s title, body and activation id are the app's) and
//! holds no constant of any app (the bundle identifier is read from the running bundle).
//!
//! macOS: through the user-notification centre. Other systems: not supported yet — [`permission`] answers
//! [`Permission::Unavailable`] and [`show`] fails with [`Error::Unavailable`].

mod platform;

/// Whether the app may show notifications, as the operating system says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// The person allowed notifications for this app.
    Granted,
    /// The person (or an administrator) turned them off; only the person can turn them back on, in the
    /// system settings. Asking again shows nothing.
    Denied,
    /// The person has not been asked yet; [`request_permission`] asks.
    NotDetermined,
    /// This run cannot use the system's notifications at all: not a bundled app on macOS (a development
    /// build started from a terminal has no bundle identifier to register), or a system with no supported
    /// route.
    Unavailable,
}

/// One notice. All of it is the app's: the crate adds no text.
#[derive(Debug, Clone, Default)]
pub struct Notice {
    /// The first line.
    pub title: String,
    /// The text under it; may be empty.
    pub body: String,
    /// Handed to [`on_activation`] when the person clicks the notice; `None` reports nothing.
    pub activation_id: Option<String>,
}

/// Why a notice was not shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// See [`Permission::Unavailable`].
    Unavailable,
    /// The person has not allowed notifications ([`Permission::Denied`] or [`Permission::NotDetermined`]).
    NotPermitted(Permission),
    /// The system refused the request; its own description.
    System(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unavailable => {
                f.write_str("the system's notifications cannot be used in this run")
            }
            Error::NotPermitted(p) => write!(f, "notifications are not allowed ({p:?})"),
            Error::System(m) => write!(f, "the system refused the notice: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// The permission as the system holds it now. Does not ask and does not show anything.
pub fn permission() -> Permission {
    platform::permission()
}

/// Asks the person once if they have not been asked ([`Permission::NotDetermined`]); returns the answer.
/// Otherwise returns the current permission without showing anything. Blocks until the person answers, so
/// call it from a worker, not from the thread that runs the window. If the answer is still
/// [`Permission::NotDetermined`] the system could not show the question — observed for a bundle run from a
/// temporary folder; an installed app in an application folder is asked.
pub fn request_permission() -> Permission {
    platform::request_permission()
}

/// Shows the notice. Fails with [`Error::NotPermitted`] unless the permission is [`Permission::Granted`];
/// it never asks by itself.
pub fn show(notice: &Notice) -> Result<(), Error> {
    platform::show(notice)
}

/// Calls `handler` with the notice's activation id when the person clicks a notice this app showed.
///
/// Call it once, at startup, before the app's window opens: a click that launches a closed app is delivered
/// to the handler set by then. A later call replaces the handler. The handler runs on a thread of the
/// system's choosing, not the one that runs the window. A notice without an activation id reports nothing.
/// Does nothing where [`permission`] is [`Permission::Unavailable`].
pub fn on_activation(handler: impl Fn(&str) + Send + Sync + 'static) {
    platform::on_activation(Box::new(handler))
}
