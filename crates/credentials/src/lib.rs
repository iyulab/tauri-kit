//! Secrets in the OS credential store — Windows Credential Manager, macOS Keychain, or the Secret
//! Service on Linux — with test and development builds kept off the installed app's entries.
//!
//! A desktop app keeps each secret under one fixed entry, so anything that writes or clears an
//! entry writes or clears *the* entry: the one the installed copy of the app relies on. A test that
//! ends by clearing it deletes the user's real token; a development build signs in over it. So each
//! kind of build gets its own entry:
//!
//! | build | service name |
//! |---|---|
//! | `cargo test` | `<service>.test` |
//! | debug build (`cargo build`, `tauri dev`) | `<service>.dev` |
//! | release build | `<service>` |
//!
//! The kind of build is decided **where [`build_kind!`] is written**, at compile time. It has to be:
//! a library cannot tell that the app using it is being tested, because dependencies are compiled
//! without `cfg(test)`. A macro expands in the caller's crate, where it can. Compile time rather
//! than an environment variable, because nothing can be forgotten at launch and a shipped binary
//! offers no way to be pointed at another entry.
//!
//! ```no_run
//! # fn main() -> Result<(), tauri_kit_credentials::Error> {
//! use tauri_kit_credentials::{build_kind, Credentials};
//!
//! let credentials = Credentials::new("com.example.app", build_kind!());
//! credentials.set("api-key", "s3cret")?;
//! assert_eq!(credentials.get("api-key")?.as_deref(), Some("s3cret"));
//! credentials.delete("api-key")?;
//! # Ok(())
//! # }
//! ```
//!
//! The platform's store is installed on first use, unless the app has already installed a default
//! store of its own with [`keyring_core::set_default_store`] — including the in-memory
//! `keyring_core::mock` store, which is how tests run without touching the real one.
//!
//! On Windows, the NuGet package `TauriKit.Credentials` reads and writes the same entries from .NET
//! (a generic credential named `<account>.<service>`), so an app's .NET backend shares its secrets.
//!
//! ## Long secrets
//!
//! The Windows Credential Manager holds at most 2,560 bytes in one entry — 1,280 UTF-16 units, the
//! way both implementations write a secret — which a signed-in session (an access token, a refresh
//! token, some claims) easily passes. The other platforms have no such limit. So on Windows a longer
//! secret is kept in parts, in the same layout from Rust and .NET:
//!
//! - A secret that fits is one entry, exactly as before.
//! - A longer one: the entry for `<account>` holds `"\0tauri-kit-credentials/parts=<n>\0"` followed
//!   by the first part, and the entry for `<account>/<i>` holds part `i` (1 to `n - 1`). Parts are cut
//!   on UTF-16 units without splitting a surrogate pair. The parts are written before the first entry.
//! - Reading joins the parts. A part that is missing is an error — a cut secret is never handed out.
//! - Writing a shorter secret, or deleting, removes the parts the earlier one left.
//!
//! A secret may therefore not begin with `'\0'`.

use std::sync::{Mutex, OnceLock};

pub use keyring_core::Error;

/// Result of a credential operation.
pub type Result<T> = std::result::Result<T, Error>;

/// Which kind of build is running. Obtain it with [`build_kind!`] so it describes the app, not
/// this library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildKind {
    /// A test run. Checked before `Dev`: test builds are debug builds too, and a test run must not
    /// clear what a development session signed in with.
    Test,
    /// A debug build of the app.
    Dev,
    /// A release build — the entries the installed app uses.
    Release,
}

impl BuildKind {
    /// The suffix this kind adds to a service name, if any.
    fn suffix(self) -> Option<&'static str> {
        match self {
            BuildKind::Test => Some("test"),
            BuildKind::Dev => Some("dev"),
            BuildKind::Release => None,
        }
    }
}

/// The [`BuildKind`] of the crate this is written in, decided when that crate is compiled.
#[macro_export]
macro_rules! build_kind {
    () => {
        if cfg!(test) {
            $crate::BuildKind::Test
        } else if cfg!(debug_assertions) {
            $crate::BuildKind::Dev
        } else {
            $crate::BuildKind::Release
        }
    };
}

/// The marker that opens the first entry of a secret kept in parts (see «Long secrets»).
const PARTS_MARKER: &str = "\0tauri-kit-credentials/parts=";

/// The most parts a secret is kept in — far past any token, short of filling the store by mistake.
const MAX_PARTS: usize = 64;

/// UTF-16 units one Windows Credential Manager entry holds (2,560 bytes).
#[cfg(target_os = "windows")]
const PLATFORM_PART_UNITS: Option<usize> = Some(1280);
#[cfg(not(target_os = "windows"))]
const PLATFORM_PART_UNITS: Option<usize> = None;

/// Secrets an app keeps under one service name, one per account.
#[derive(Debug, Clone)]
pub struct Credentials {
    service: String,
    part_units: Option<usize>,
}

impl Credentials {
    /// Secrets under `service` (for example the app's identifier), as seen by this kind of build.
    pub fn new(service: &str, kind: BuildKind) -> Self {
        let service = match kind.suffix() {
            Some(suffix) => format!("{service}.{suffix}"),
            None => service.to_string(),
        };
        Self {
            service,
            part_units: PLATFORM_PART_UNITS,
        }
    }

    /// Keeps secrets longer than `units` UTF-16 units in parts, as Windows needs — for testing the
    /// layout on any platform against an in-memory store. Not for apps: the platform's own limit is
    /// already the default.
    #[doc(hidden)]
    pub fn with_part_units(mut self, units: usize) -> Self {
        self.part_units = Some(units);
        self
    }

    /// The service name entries are actually stored under.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Stores `secret` for `account`, replacing any earlier one.
    pub fn set(&self, account: &str, secret: &str) -> Result<()> {
        if secret.starts_with('\0') {
            return Err(Error::Invalid(
                "secret".into(),
                "a secret may not begin with a NUL character".into(),
            ));
        }
        let before = self.part_count(account)?;
        let parts = self.split(secret)?;
        for (i, part) in parts.iter().enumerate().skip(1) {
            self.entry(&part_account(account, i))?.set_password(part)?;
        }
        let head = if parts.len() == 1 {
            parts[0].clone()
        } else {
            format!("{PARTS_MARKER}{}\0{}", parts.len(), parts[0])
        };
        self.entry(account)?.set_password(&head)?;
        self.delete_parts(account, parts.len(), before)
    }

    /// The secret stored for `account`, or `None` when there is none.
    pub fn get(&self, account: &str) -> Result<Option<String>> {
        let Some(head) = self.read(account)? else {
            return Ok(None);
        };
        let Some((count, first)) = parse_head(&head)? else {
            return Ok(Some(head));
        };
        let mut secret = String::from(first);
        for i in 1..count {
            match self.read(&part_account(account, i))? {
                Some(part) => secret.push_str(&part),
                None => {
                    return Err(Error::Invalid(
                        account.into(),
                        format!("part {i} of {count} of this secret is missing"),
                    ))
                }
            }
        }
        Ok(Some(secret))
    }

    /// Whether a secret is stored for `account`, without handing it out.
    pub fn contains(&self, account: &str) -> Result<bool> {
        Ok(self.get(account)?.is_some())
    }

    /// Removes the secret for `account`. Removing one that is not there is not an error.
    pub fn delete(&self, account: &str) -> Result<()> {
        let before = self.part_count(account)?;
        self.delete_parts(account, 1, before)?;
        remove(self.entry(account)?)
    }

    fn entry(&self, account: &str) -> Result<keyring_core::Entry> {
        ensure_store()?;
        keyring_core::Entry::new(&self.service, account)
    }

    fn read(&self, account: &str) -> Result<Option<String>> {
        match self.entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// How many parts the secret now under `account` is kept in (1 when it is one entry or none).
    /// A first entry whose marker cannot be read counts as one: writing over it is how it is fixed.
    fn part_count(&self, account: &str) -> Result<usize> {
        Ok(match self.read(account)? {
            Some(head) => parse_head(&head).ok().flatten().map_or(1, |(n, _)| n),
            None => 1,
        })
    }

    /// Removes parts `from` up to (not including) `to`.
    fn delete_parts(&self, account: &str, from: usize, to: usize) -> Result<()> {
        for i in from.max(1)..to {
            remove(self.entry(&part_account(account, i))?)?;
        }
        Ok(())
    }

    /// `secret` cut into parts that each fit one entry, the first leaving room for the marker.
    fn split(&self, secret: &str) -> Result<Vec<String>> {
        let Some(units) = self.part_units else {
            return Ok(vec![secret.to_string()]);
        };
        if secret.encode_utf16().count() <= units {
            return Ok(vec![secret.to_string()]);
        }
        // The marker's count is written with at most two digits (MAX_PARTS), so its room is fixed.
        let first_room = units - (PARTS_MARKER.encode_utf16().count() + 3);
        let mut parts = vec![String::new()];
        let mut room = first_room;
        let mut used = 0;
        for c in secret.chars() {
            let width = c.len_utf16();
            if used + width > room {
                parts.push(String::new());
                room = units;
                used = 0;
            }
            parts.last_mut().unwrap().push(c);
            used += width;
        }
        if parts.len() > MAX_PARTS {
            return Err(Error::TooLong("secret".into(), (MAX_PARTS * units) as u32));
        }
        Ok(parts)
    }
}

fn part_account(account: &str, i: usize) -> String {
    format!("{account}/{i}")
}

/// `(count, first part)` when `head` opens a secret kept in parts, `None` when it is the whole secret.
fn parse_head(head: &str) -> Result<Option<(usize, &str)>> {
    let Some(rest) = head.strip_prefix(PARTS_MARKER) else {
        return Ok(None);
    };
    let bad = || {
        Error::Invalid(
            "secret".into(),
            "the first entry's parts marker is malformed".into(),
        )
    };
    let (count, first) = rest.split_once('\0').ok_or_else(bad)?;
    let count: usize = count.parse().map_err(|_| bad())?;
    if !(2..=MAX_PARTS).contains(&count) {
        return Err(bad());
    }
    Ok(Some((count, first)))
}

fn remove(entry: keyring_core::Entry) -> Result<()> {
    match entry.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Installs the platform's store as the default once, unless the app already installed one.
///
/// Only success is remembered. A store that is not reachable yet (a Linux session bus still starting)
/// is tried again on the next call, and its own error is what the caller sees.
fn ensure_store() -> Result<()> {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    static INSTALLING: Mutex<()> = Mutex::new(());
    if INSTALLED.get().is_some() {
        return Ok(());
    }
    let _one_at_a_time = INSTALLING.lock().unwrap_or_else(|e| e.into_inner());
    if INSTALLED.get().is_none() {
        if keyring_core::get_default_store().is_none() {
            install_platform_store()?;
        }
        let _ = INSTALLED.set(());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_platform_store() -> Result<()> {
    keyring_core::set_default_store(windows_native_keyring_store::Store::new()?);
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_platform_store() -> Result<()> {
    keyring_core::set_default_store(apple_native_keyring_store::keychain::Store::new()?);
    Ok(())
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn install_platform_store() -> Result<()> {
    keyring_core::set_default_store(zbus_secret_service_keyring_store::Store::new()?);
    Ok(())
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    )
)))]
fn install_platform_store() -> Result<()> {
    Err(Error::NoDefaultStore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_build_gets_its_own_service_name() {
        let service = |kind| {
            Credentials::new("com.example.app", kind)
                .service()
                .to_string()
        };
        assert_eq!(service(BuildKind::Release), "com.example.app");
        assert_eq!(service(BuildKind::Dev), "com.example.app.dev");
        assert_eq!(service(BuildKind::Test), "com.example.app.test");
    }

    #[test]
    fn the_macro_describes_the_crate_it_is_written_in() {
        assert_eq!(build_kind!(), BuildKind::Test);
    }
}
