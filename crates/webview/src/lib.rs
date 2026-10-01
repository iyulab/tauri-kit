//! The web view runtime under a desktop app's window.
//!
//! On Windows the window is drawn by Microsoft Edge WebView2, which is installed apart from the
//! app. Two things every app that draws its window with it ends up doing:
//!
//! - **Say so when the runtime is missing.** When no runtime can be found, the window cannot open
//!   and the framework stops with a message of its own, in English. [`runtime`] looks first — by
//!   asking the runtime's own loader, which finds the files, not merely a registration that can
//!   outlive them — and [`alert`] shows the app's own message, in the person's language, before any
//!   window exists. The words are the app's; this crate only knows how to ask and how to show.
//! - **Keep form entries out of the web view's profile.** A web view remembers what is typed into
//!   forms, for autofill, in its own profile folder — outside whatever the app keeps its data in,
//!   and outside its encryption. With autofill turned off on the window (`general_autofill_enabled(false)`
//!   on a Tauri window builder), [`forget_form_entries`] clears what an earlier version may have
//!   remembered, and [`remove_profile_snapshots`] removes the copies of the profile the runtime takes
//!   when it updates itself, which would carry those entries along.
//!
//! ```no_run
//! use tauri_kit_webview::{alert, runtime, Runtime};
//!
//! if let Runtime::Missing = runtime() {
//!     alert("The window cannot open", "Install the Microsoft Edge WebView2 Runtime, then start the app again.");
//!     std::process::exit(1);
//! }
//! ```
//!
//! On macOS and Linux the system web view is part of the operating system: [`runtime`] answers
//! [`Runtime::System`] and there is no profile to clear.

use std::io;
use std::path::{Path, PathBuf};

/// The web view runtime this computer offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runtime {
    /// A WebView2 runtime whose files are on disk, with its version.
    Installed(String),
    /// No WebView2 runtime that can start — none installed, or one registered without its files.
    Missing,
    /// The operating system's own web view (macOS, Linux), which is always there.
    System,
}

/// Which web view runtime this computer offers, asked of the runtime's loader.
#[cfg(windows)]
pub fn runtime() -> Runtime {
    use webview2_com::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
    use windows_core::{PCWSTR, PWSTR};

    let mut version = PWSTR::null();
    // SAFETY: a null folder asks for the installed runtime; on success the loader hands over a
    // string that `take_pwstr` frees.
    match unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) } {
        Ok(()) if !version.is_null() => Runtime::Installed(webview2_com::take_pwstr(version)),
        _ => Runtime::Missing,
    }
}

/// Which web view runtime this computer offers: the system's own.
#[cfg(not(windows))]
pub fn runtime() -> Runtime {
    Runtime::System
}

/// Shows a message before any window exists and waits for the person to close it: a message box
/// on Windows, standard error elsewhere.
#[cfg(windows)]
pub fn alert(title: &str, body: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (title, body) = (wide(title), wide(body));
    // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

/// Shows a message before any window exists: standard error, where there is no message box to
/// show it in without a window toolkit.
#[cfg(not(windows))]
pub fn alert(title: &str, body: &str) {
    eprintln!("{title}\n\n{body}");
}

/// Where WebView2 keeps the copies of a profile it takes when the runtime updates itself, under
/// the folder the app gives the web view for its data (for a Tauri app, its local app data folder).
pub fn profile_snapshots(data_dir: &Path) -> PathBuf {
    data_dir.join("EBWebView").join("Snapshots")
}

/// Removes the copies of the web view profile the runtime keeps from before its updates. They exist
/// only to roll the runtime back and are taken again from the current profile; a copy taken before
/// autofill was turned off still holds the entries typed into it. Call it before the web view opens.
/// Nothing to remove is not an error.
pub fn remove_profile_snapshots(data_dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(profile_snapshots(data_dir)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Clears the form entries and saved passwords the web view's profile remembered — what an
/// earlier version of the app let it keep before autofill was turned off. The clearing finishes in
/// the background.
///
/// For a Tauri window: `window.with_webview(|w| { let _ = forget_form_entries(&w.controller()); })`.
#[cfg(windows)]
pub fn forget_form_entries(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
) -> windows_core::Result<()> {
    use webview2_com::ClearBrowsingDataCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Profile2, ICoreWebView2_13, COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL,
        COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE,
    };
    use windows_core::Interface;

    // SAFETY: COM calls on the controller the caller holds, on the thread the web view lives on
    // (`with_webview` runs its closure there).
    unsafe {
        let profile = controller
            .CoreWebView2()?
            .cast::<ICoreWebView2_13>()?
            .Profile()?
            .cast::<ICoreWebView2Profile2>()?;
        profile.ClearBrowsingData(
            COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL
                | COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE,
            &ClearBrowsingDataCompletedHandler::create(Box::new(|_| Ok(()))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_snapshots_are_removed_and_the_rest_of_the_profile_kept() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = profile_snapshots(dir.path())
            .join("1.0.0.0")
            .join("Default");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(
            snapshot.join("Web Data"),
            "an entry typed into an earlier version",
        )
        .unwrap();
        let profile = dir.path().join("EBWebView").join("Default");
        std::fs::create_dir_all(&profile).unwrap();

        remove_profile_snapshots(dir.path()).unwrap();
        assert!(!profile_snapshots(dir.path()).exists());
        assert!(profile.exists(), "the profile itself stays");
    }

    #[test]
    fn no_snapshots_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        remove_profile_snapshots(dir.path()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn an_installed_runtime_reports_its_version() {
        // Every Windows machine these tests run on has the runtime (it ships with Windows 11 and
        // with the hosted runners).
        match runtime() {
            Runtime::Installed(version) => assert!(version.split('.').count() >= 3, "{version}"),
            other => panic!("expected an installed runtime, got {other:?}"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_use_the_system_web_view() {
        assert_eq!(runtime(), Runtime::System);
    }
}
