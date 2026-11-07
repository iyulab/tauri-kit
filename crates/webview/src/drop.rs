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
//!
//! macOS has no such message, and its framework drop handler — the one that reports paths there —
//! takes every drag the web view would see, the page's own included. With that handler off, a drag
//! from another app still travels on the system's drag pasteboard, which holds the dropped items'
//! file URLs when the page's `drop` runs. [`drag_pasteboard_paths`] is the host half there: the page
//! names the files it received (each `File`'s name, in its order) and gets their locations back —
//! only when the pasteboard holds exactly those files, so a pasteboard left over from an earlier
//! drag answers nothing. The page half is a command of the app's choosing that passes the names.

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

/// Where the files a page received in its `drop` are, read from the system's drag pasteboard
/// (macOS): `names` are the names of the `File` objects the page got, in its order; the answer is
/// their locations in that order. Empty unless every name matches one dragged item by its last
/// path component and every dragged item one name — a drag pasteboard from another drag holds
/// other files. Two files of one name from two folders are both kept, in the pasteboard's order.
/// Empty on other platforms (see the [module](self) documentation for Windows).
pub fn drag_pasteboard_paths(names: &[String]) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let dragged = read_drag_pasteboard();
    #[cfg(not(target_os = "macos"))]
    let dragged: Vec<PathBuf> = Vec::new();
    in_page_order(names, dragged)
}

/// The dragged paths in the order of the names the page received, each name matched to one path by
/// its last component; none unless every name finds a path and every path a name.
fn in_page_order(names: &[String], dragged: Vec<PathBuf>) -> Vec<PathBuf> {
    if names.is_empty() || names.len() != dragged.len() {
        return Vec::new();
    }
    let mut left: Vec<Option<PathBuf>> = dragged.into_iter().map(Some).collect();
    let mut ordered = Vec::with_capacity(names.len());
    for name in names {
        let found = left.iter_mut().find(|slot| {
            slot.as_ref()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n.to_string_lossy() == name.as_str())
        });
        match found.and_then(Option::take) {
            Some(path) => ordered.push(path),
            None => return Vec::new(),
        }
    }
    ordered
}

/// The file URLs on the system's drag pasteboard, as paths.
#[cfg(target_os = "macos")]
fn read_drag_pasteboard() -> Vec<PathBuf> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardNameDrag, NSPasteboardTypeFileURL};
    use objc2_foundation::NSURL;
    // SAFETY: NSPasteboardNameDrag is the system's constant naming the drag pasteboard.
    let pasteboard = NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag });
    let mut paths = Vec::new();
    if let Some(items) = pasteboard.pasteboardItems() {
        for item in items.iter() {
            // SAFETY: NSPasteboardTypeFileURL is the system's constant for a file URL, read from an
            // item of this pasteboard.
            let Some(text) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
                continue;
            };
            // A file reference URL (file:///.file/id=…) is resolved to the path it names.
            if let Some(path) = NSURL::URLWithString(&text)
                .and_then(|url| url.filePathURL())
                .and_then(|url| url.path())
            {
                paths.push(PathBuf::from(path.to_string()));
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_dragged_paths_come_back_in_the_page_order() {
        let dragged = vec![PathBuf::from("/a/two.txt"), PathBuf::from("/b/one.pdf")];
        assert_eq!(
            in_page_order(&names(&["one.pdf", "two.txt"]), dragged),
            vec![PathBuf::from("/b/one.pdf"), PathBuf::from("/a/two.txt")]
        );
    }

    #[test]
    fn two_files_of_one_name_from_two_folders_are_both_kept() {
        let dragged = vec![PathBuf::from("/a/note.txt"), PathBuf::from("/b/note.txt")];
        assert_eq!(
            in_page_order(&names(&["note.txt", "note.txt"]), dragged),
            vec![PathBuf::from("/a/note.txt"), PathBuf::from("/b/note.txt")]
        );
    }

    #[test]
    fn a_pasteboard_from_another_drag_answers_nothing() {
        // Other names, or a different count: the pasteboard holds some other drag's files.
        let other = vec![PathBuf::from("/a/old.txt")];
        assert!(in_page_order(&names(&["new.txt"]), other.clone()).is_empty());
        assert!(in_page_order(&names(&["old.txt", "new.txt"]), other).is_empty());
        assert!(in_page_order(&[], vec![PathBuf::from("/a/x")]).is_empty());
    }

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
