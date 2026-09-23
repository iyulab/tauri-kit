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

use std::sync::OnceLock;

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

/// Secrets an app keeps under one service name, one per account.
#[derive(Debug, Clone)]
pub struct Credentials {
    service: String,
}

impl Credentials {
    /// Secrets under `service` (for example the app's identifier), as seen by this kind of build.
    pub fn new(service: &str, kind: BuildKind) -> Self {
        let service = match kind.suffix() {
            Some(suffix) => format!("{service}.{suffix}"),
            None => service.to_string(),
        };
        Self { service }
    }

    /// The service name entries are actually stored under.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Stores `secret` for `account`, replacing any earlier one.
    pub fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.entry(account)?.set_password(secret)
    }

    /// The secret stored for `account`, or `None` when there is none.
    pub fn get(&self, account: &str) -> Result<Option<String>> {
        match self.entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether a secret is stored for `account`, without handing it out.
    pub fn contains(&self, account: &str) -> Result<bool> {
        Ok(self.get(account)?.is_some())
    }

    /// Removes the secret for `account`. Removing one that is not there is not an error.
    pub fn delete(&self, account: &str) -> Result<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring_core::Entry> {
        ensure_store()?;
        keyring_core::Entry::new(&self.service, account)
    }
}

/// Installs the platform's store as the default once, unless the app already installed one.
fn ensure_store() -> Result<()> {
    static INSTALLED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    let outcome = INSTALLED.get_or_init(|| {
        if keyring_core::get_default_store().is_some() {
            return Ok(());
        }
        install_platform_store().map_err(|e| e.to_string())
    });
    outcome.clone().map_err(|_| Error::NoDefaultStore)
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
