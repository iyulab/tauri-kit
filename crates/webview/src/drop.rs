//! Where files dropped onto a page come from.
//!
//! A page that takes files from other apps by drag and drop gets `File` objects — names, sizes,
//! contents — but never where they are on disk. An app that works with the files where they are
//! (copies them, opens them, indexes them) needs that location. In a Tauri app the framework's own
//! drop handler reports paths, but on Windows it takes HTML5 drag and drop away from the page (the
//! page gets `dragstart` and `dragend` and never a `drop`), so an app with drag and drop of its own
//! turns that handler off — and with it the paths.
//!
//! WebView2's own way back is a message: the page posts the dropped `File` objects along with a
//! message, and the host reads each one's path. [`receive_dropped_files`] is the host half; the page
//! half is one call in its `drop` handler:
//!
//! ```js
//! // `prefix` is the app's own string; anything after it is the app's too (an id to answer, say).
//! window.chrome.webview.postMessageWithAdditionalObjects(`${prefix}${id}`, event.dataTransfer.files)
//! ```
//!
//! Only messages whose text starts with the app's prefix are read; every other message — the app's
//! framework's own, for one — is left to its handlers. Windows only: the other platforms' web views
//! hand a page no such objects.

use std::path::PathBuf;

/// The files of one drop, as the host received them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedFiles {
    /// The message's text, prefix included, as the page sent it.
    pub message: String,
    /// The URL of the document that sent it — for the app to check against its own origins.
    pub source: String,
    /// The location of each dropped file the message carried, in the page's order. A dropped folder
    /// is a path like a file's.
    pub paths: Vec<PathBuf>,
}

/// Whether a message's text is one the app asked to receive.
#[cfg(any(windows, test))]
fn is_for(message: &str, prefix: &str) -> bool {
    !prefix.is_empty() && message.starts_with(prefix)
}

/// Calls `on_files` with the files of each drop the page reports in a message starting with
/// `prefix` (see the [module](self) documentation for the page's half). Messages without that
/// prefix are not read. A message with the prefix but no files still arrives, with no paths — the
/// page dropped something that is not a file.
///
/// `prefix` must not be empty: the app's framework sends messages of its own on the same channel.
#[cfg(windows)]
pub fn receive_dropped_files(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    prefix: &str,
    on_files: impl Fn(DroppedFiles) + 'static,
) -> windows_core::Result<()> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2File, ICoreWebView2WebMessageReceivedEventArgs2,
    };
    use webview2_com::{take_pwstr, WebMessageReceivedEventHandler};
    use windows_core::{Interface, PWSTR};

    if prefix.is_empty() {
        return Err(windows_core::Error::new(
            windows_core::HRESULT(0x8007_0057u32 as i32), // E_INVALIDARG
            "the prefix must not be empty",
        ));
    }
    let prefix = prefix.to_owned();
    // SAFETY: COM calls on the web view the caller holds, on its thread; the arguments live for the
    // callback, and every string the runtime hands over is freed by `take_pwstr`.
    let core = unsafe { controller.CoreWebView2()? };
    let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut text = PWSTR::null();
        // A message that is not a string (JSON posted with `postMessage(object)`) is not one of ours.
        if unsafe { args.TryGetWebMessageAsString(&mut text) }.is_err() {
            return Ok(());
        }
        let message = take_pwstr(text);
        if !is_for(&message, &prefix) {
            return Ok(());
        }
        let mut source = PWSTR::null();
        unsafe { args.Source(&mut source)? };
        let source = take_pwstr(source);
        let mut paths = Vec::new();
        if let Ok(args) = args.cast::<ICoreWebView2WebMessageReceivedEventArgs2>() {
            if let Ok(objects) = unsafe { args.AdditionalObjects() } {
                let mut count = 0u32;
                unsafe { objects.Count(&mut count)? };
                for i in 0..count {
                    let Ok(object) = (unsafe { objects.GetValueAtIndex(i) }) else {
                        continue;
                    };
                    // Only files carry a path; other objects a page may post are skipped.
                    let Ok(file) = object.cast::<ICoreWebView2File>() else {
                        continue;
                    };
                    let mut path = PWSTR::null();
                    if unsafe { file.Path(&mut path) }.is_ok() {
                        paths.push(PathBuf::from(take_pwstr(path)));
                    }
                }
            }
        }
        let _ = catch_unwind(AssertUnwindSafe(|| {
            on_files(DroppedFiles {
                message,
                source,
                paths,
            })
        }));
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { core.add_WebMessageReceived(&handler, &mut token)? };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_messages_that_start_with_the_prefix_are_read() {
        assert!(is_for("app-drop:7", "app-drop:"));
        assert!(is_for("app-drop:", "app-drop:"));
        assert!(!is_for("{\"cmd\":\"x\"}", "app-drop:"));
        assert!(!is_for("xapp-drop:7", "app-drop:"));
        // An empty prefix would take every message on the channel.
        assert!(!is_for("anything", ""));
    }
}
