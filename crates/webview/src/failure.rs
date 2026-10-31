//! A web view process that failed — and what the window can do about it.
//!
//! The window of an app is drawn by processes the runtime starts and the app does not own. When the one that
//! draws the page ends, the window stays on screen, blank or frozen, and nothing in the app says so: the
//! framework on top of the runtime does not pass the failure on. WebView2 raises `ProcessFailed` for it, with the
//! kind of process and, on a current runtime, why it went. [`watch`] subscribes to it and hands the app a
//! [`Failed`] made of closed values only — never a process description, a path or any text of the page — so the app
//! can count it, show a message in the person's language, or reload.
//!
//! What to do is the app's: whether to reload once or ask, whether to report it, what to tell the person.
//! [`reload`] is there for the common answer to a main frame whose render process ended — the page is gone but
//! the web view is not, and a reload starts it again. After [`Process::Browser`] the web view itself is unusable
//! and only the window's owner can start a new one.
//!
//! The values are the ones WebView2 documents (`COREWEBVIEW2_PROCESS_FAILED_KIND`, `…_REASON`); a number
//! a newer runtime adds arrives as [`Process::Unknown`] / [`Cause::Unknown`] instead of being refused. Not
//! supported on macOS and Linux, where the system web view reports a terminated content process through the
//! framework's own navigation delegate.

/// The process that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Process {
    /// The browser process — the web view as a whole is gone.
    Browser,
    /// The render process of the main frame — the page is gone, blank or frozen.
    Render,
    /// A render process that stopped answering (the page is frozen, not gone).
    RenderUnresponsive,
    /// The render process of a frame other than the main one.
    FrameRender,
    /// A utility process (a service such as networking or audio).
    Utility,
    /// The sandbox helper process.
    SandboxHelper,
    /// The GPU process.
    Gpu,
    /// A plugin process.
    Plugin,
    /// The broker of a plugin process.
    PluginBroker,
    /// A process this crate does not know — the runtime's own `UNKNOWN_PROCESS_EXITED`, or a kind a newer runtime added.
    Unknown,
}

impl Process {
    /// The value WebView2 gives (`COREWEBVIEW2_PROCESS_FAILED_KIND`).
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Browser,
            1 => Self::Render,
            2 => Self::RenderUnresponsive,
            3 => Self::FrameRender,
            4 => Self::Utility,
            5 => Self::SandboxHelper,
            6 => Self::Gpu,
            7 => Self::Plugin,
            8 => Self::PluginBroker,
            _ => Self::Unknown,
        }
    }

    /// Whether the page the person was looking at is gone or frozen: the main frame's render process ended
    /// or stopped answering. A reload ([`reload`]) is the answer for exactly these; the browser process ending
    /// is not one (the web view itself is unusable).
    pub fn page_lost(self) -> bool {
        matches!(self, Self::Render | Self::RenderUnresponsive)
    }
}

/// Why the process failed, when the runtime says (a current runtime does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// It ended for no reason the runtime could tell.
    Unexpected,
    /// It stopped answering.
    Unresponsive,
    /// Something ended it (the task manager, a security product, the person).
    Terminated,
    /// It crashed.
    Crashed,
    /// It could not start.
    LaunchFailed,
    /// It ran out of memory.
    OutOfMemory,
    /// The profile it was using was deleted.
    ProfileDeleted,
    /// The runtime did not say why, or said something this crate does not know.
    Unknown,
}

impl Cause {
    /// The value WebView2 gives (`COREWEBVIEW2_PROCESS_FAILED_REASON`).
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Unexpected,
            1 => Self::Unresponsive,
            2 => Self::Terminated,
            3 => Self::Crashed,
            4 => Self::LaunchFailed,
            5 => Self::OutOfMemory,
            6 => Self::ProfileDeleted,
            _ => Self::Unknown,
        }
    }
}

/// One failure of a web view process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed {
    /// Which process.
    pub process: Process,
    /// Why, when the runtime said.
    pub cause: Cause,
    /// The process's exit code, when the runtime gave one (`0` when not).
    pub exit_code: i32,
}

/// Calls `on_failed` each time a process of the web view fails. Call once per web view, on its thread; `on_failed`
/// runs on that thread too, so it must not block. A panic in it is caught and ignored — the web view's event
/// must not unwind into the runtime.
#[cfg(windows)]
pub fn watch(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    on_failed: impl Fn(Failed) + 'static,
) -> windows_core::Result<()> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2ProcessFailedEventArgs2, COREWEBVIEW2_PROCESS_FAILED_KIND,
        COREWEBVIEW2_PROCESS_FAILED_REASON,
    };
    use webview2_com::ProcessFailedEventHandler;
    use windows_core::Interface;

    // SAFETY: COM calls on the web view the caller holds, on its thread.
    let core = unsafe { controller.CoreWebView2()? };
    let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
        unsafe { args.ProcessFailedKind(&mut kind)? };
        // A runtime older than the second interface still tells the kind; the rest stays unknown.
        let (cause, exit_code) = match args.cast::<ICoreWebView2ProcessFailedEventArgs2>() {
            Ok(args) => {
                let mut reason = COREWEBVIEW2_PROCESS_FAILED_REASON(-1);
                let mut exit_code = 0i32;
                unsafe {
                    let _ = args.Reason(&mut reason);
                    let _ = args.ExitCode(&mut exit_code);
                }
                (Cause::from_raw(reason.0), exit_code)
            }
            Err(_) => (Cause::Unknown, 0),
        };
        let failed = Failed {
            process: Process::from_raw(kind.0),
            cause,
            exit_code,
        };
        let _ = catch_unwind(AssertUnwindSafe(|| on_failed(failed)));
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { core.add_ProcessFailed(&handler, &mut token)? };
    Ok(())
}

/// Reloads the document of the web view — what to do for a main frame whose render process ended
/// ([`Process::page_lost`]). Call on the web view's thread.
#[cfg(windows)]
pub fn reload(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
) -> windows_core::Result<()> {
    // SAFETY: a COM call on the web view the caller holds, on its thread.
    unsafe { controller.CoreWebView2()?.Reload() }
}

#[cfg(test)]
mod tests {
    use super::{Cause, Process};

    #[test]
    fn every_kind_webview2_documents_is_named_and_the_rest_is_unknown() {
        let named = [
            (0, Process::Browser),
            (1, Process::Render),
            (2, Process::RenderUnresponsive),
            (3, Process::FrameRender),
            (4, Process::Utility),
            (5, Process::SandboxHelper),
            (6, Process::Gpu),
            (7, Process::Plugin),
            (8, Process::PluginBroker),
        ];
        for (raw, process) in named {
            assert_eq!(Process::from_raw(raw), process, "kind {raw}");
        }
        // 9 is the runtime's own «unknown process»; a number above it is a kind a newer runtime added.
        for raw in [9, 10, 99, -1, i32::MAX] {
            assert_eq!(Process::from_raw(raw), Process::Unknown, "kind {raw}");
        }
    }

    #[test]
    fn every_reason_webview2_documents_is_named_and_the_rest_is_unknown() {
        let named = [
            (0, Cause::Unexpected),
            (1, Cause::Unresponsive),
            (2, Cause::Terminated),
            (3, Cause::Crashed),
            (4, Cause::LaunchFailed),
            (5, Cause::OutOfMemory),
            (6, Cause::ProfileDeleted),
        ];
        for (raw, cause) in named {
            assert_eq!(Cause::from_raw(raw), cause, "reason {raw}");
        }
        for raw in [7, 99, -1, i32::MAX] {
            assert_eq!(Cause::from_raw(raw), Cause::Unknown, "reason {raw}");
        }
    }

    #[test]
    fn only_the_main_frames_render_process_loses_the_page() {
        assert!(Process::Render.page_lost());
        assert!(Process::RenderUnresponsive.page_lost());
        for other in [
            Process::Browser,
            Process::FrameRender,
            Process::Utility,
            Process::SandboxHelper,
            Process::Gpu,
            Process::Plugin,
            Process::PluginBroker,
            Process::Unknown,
        ] {
            assert!(!other.page_lost(), "{other:?}");
        }
    }

    /// The numbers above are the runtime's: the bindings this crate builds against say the same.
    #[cfg(windows)]
    #[test]
    fn the_raw_values_are_the_bindings_values() {
        use webview2_com::Microsoft::Web::WebView2::Win32::*;
        assert_eq!(COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED.0, 0);
        assert_eq!(COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED.0, 1);
        assert_eq!(
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE.0,
            2
        );
        assert_eq!(
            COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED.0,
            3
        );
        assert_eq!(COREWEBVIEW2_PROCESS_FAILED_KIND_UNKNOWN_PROCESS_EXITED.0, 9);
        assert_eq!(COREWEBVIEW2_PROCESS_FAILED_REASON_UNEXPECTED.0, 0);
        assert_eq!(COREWEBVIEW2_PROCESS_FAILED_REASON_CRASHED.0, 3);
        assert_eq!(COREWEBVIEW2_PROCESS_FAILED_REASON_PROFILE_DELETED.0, 6);
    }
}
