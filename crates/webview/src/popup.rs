//! Windows a page opens.
//!
//! A page opens a window with `window.open` or a `target="_blank"` link. Browsers settle three
//! things about it, and a desktop app that shows web pages has to settle them too:
//!
//! - **Who asked.** A window the person did not ask for — a timer's or a script's `window.open` — is
//!   a popup and is refused. [`opening`] refuses every request that is not known to come from the
//!   person; how a platform says so is below.
//! - **Whether the page keeps a hold on it.** A sign-in or payment page is opened at a given size
//!   with `window.open`, and hands its result back through `window.opener` before closing itself.
//!   Opened anywhere else (a tab of the app's, the system browser) that link is broken and the
//!   sign-in never finishes. [`Opening::KeepOpener`] says: create the window the framework offers,
//!   which keeps it. A sized `about:blank` is the same — a page opens an empty named window and sends
//!   a form into it.
//! - **That `window.close()` closes the window.** A page that closes itself asks the web view to
//!   close, and the framework under it removes the web view but leaves its window standing, empty,
//!   under the page's last title. [`close_window_on_script_close`] closes the window instead.
//!
//! Everything else the person asked for — a link to another site, an unsized `window.open` —
//! is [`Opening::Elsewhere`]: the app opens it where it opens addresses (its own tab, the system
//! browser), or asks first. Which, and anything about the app's own addresses, is the app's.
//!
//! **Who asked, per platform.**
//! - *Windows*: the framework's new-window handler does not pass WebView2's `IsUserInitiated`.
//!   [`report_new_windows`] reads it from the same event and reports it; the app keeps the last
//!   report per web view and reads it in its new-window handler, which the framework runs after the
//!   event (it defers the handler to the message loop). A missing report counts as not the person's.
//! - *macOS*: a web view opens scripted windows by default and does not tell its handler who asked.
//!   [`block_scripted_windows`] turns that off, after which every request reaching the handler came
//!   from the person — pass `Some(true)`.
//! - A window held by [`lock_navigation`](crate::lock_navigation) is not one to use these on: the
//!   lock creates no window and reports each request with who asked, in [`Attempt`](crate::Attempt).
//!   Feed that to [`opening`] instead.

/// What to do with a window a page asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    /// Create the window the framework offers for the request — it keeps `window.opener`.
    KeepOpener,
    /// Do not create it; the app opens the address where it opens addresses, or asks first.
    Elsewhere,
    /// Do not create it, and open nothing: the person did not ask, or it is not a web page.
    Refuse,
}

/// A page's request for a new window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<'a> {
    /// The address the window would show.
    pub url: &'a str,
    /// Whether the person started it (a click, a key) — `None` where the platform does not say,
    /// which counts as no.
    pub user_initiated: Option<bool>,
    /// Whether the page gave the window a size (`window.open` with `width` or `height`).
    pub sized: bool,
}

/// What to do with `request`. Only what the person asked for is opened, and only web pages
/// (`http:`, `https:`) or a sized empty window; the app may still refuse an address of its own.
pub fn opening(request: Request<'_>) -> Opening {
    if request.user_initiated != Some(true) {
        return Opening::Refuse;
    }
    let scheme = request
        .url
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase());
    let web = matches!(scheme.as_deref(), Some("http" | "https"));
    let blank = request.url.eq_ignore_ascii_case("about:blank");
    match (web || blank, request.sized) {
        (true, true) => Opening::KeepOpener,
        (true, false) if web => Opening::Elsewhere,
        _ => Opening::Refuse,
    }
}

/// A new-window request as WebView2 reported it, before the framework's handler runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested {
    pub uri: String,
    pub user_initiated: bool,
}

/// Reports each new-window request of the web view with whether the person started it, without
/// handling it — the window is still the framework's handler's to create or deny. Call once per web
/// view, on its thread. `on_request` runs inside the event: keep it short. A panic in it is caught.
#[cfg(windows)]
pub fn report_new_windows(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    on_request: impl Fn(Requested) + 'static,
) -> windows_core::Result<()> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use webview2_com::{take_pwstr, NewWindowRequestedEventHandler};
    use windows_core::{BOOL, PWSTR};

    // SAFETY: COM calls on the web view the caller holds, on its thread; the event arguments live
    // for the callback, and the address string is freed by `take_pwstr`.
    let core = unsafe { controller.CoreWebView2()? };
    let handler = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut uri = PWSTR::null();
        let mut by_person = BOOL::default();
        let uri = unsafe {
            args.Uri(&mut uri)?;
            args.IsUserInitiated(&mut by_person)?;
            take_pwstr(uri)
        };
        let requested = Requested {
            uri,
            user_initiated: by_person.as_bool(),
        };
        let _ = catch_unwind(AssertUnwindSafe(|| on_request(requested)));
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { core.add_NewWindowRequested(&handler, &mut token)? };
    Ok(())
}

/// Makes the web view refuse windows a script opens without the person — the platform's own popup
/// blocker, off by default on macOS. After it, the new-window handler sees only requests the person
/// started. Call on the main thread.
#[cfg(target_os = "macos")]
pub fn block_scripted_windows(web_view: &objc2_web_kit::WKWebView) {
    // SAFETY: the caller's live web view, on the main thread; its preferences are its own.
    unsafe {
        web_view
            .configuration()
            .preferences()
            .setJavaScriptCanOpenWindowsAutomatically(false);
    }
}

/// Closes the window that holds the web view when its page calls `window.close()` — for a window
/// made for a page's request ([`Opening::KeepOpener`]); a page in the app's own window should not
/// close it. The window is closed the way its close button does, so the app's close handling runs.
/// Call once per web view, on its thread.
#[cfg(windows)]
pub fn close_window_on_script_close(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
) -> windows_core::Result<()> {
    use webview2_com::WindowCloseRequestedEventHandler;
    use windows::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, PostMessageW, GA_ROOT, WM_CLOSE,
    };

    // The web view sits in a child window of the framework's; the window to close is its root.
    let mut parent = HWND::default();
    // SAFETY: COM calls on the web view the caller holds, on its thread.
    let core = unsafe {
        controller.ParentWindow(&mut parent)?;
        controller.CoreWebView2()?
    };
    let parent = parent.0 as isize;
    let handler = WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
        // SAFETY: plain window calls; a window that is gone makes them fail, which is ignored.
        unsafe {
            let root = GetAncestor(parent as _, GA_ROOT);
            if !root.is_null() {
                PostMessageW(root, WM_CLOSE, 0, 0);
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { core.add_WindowCloseRequested(&handler, &mut token)? };
    Ok(())
}

/// The window identifier that marks a window made by [`close_window_on_script_close`] on macOS.
#[cfg(target_os = "macos")]
const CLOSE_ON_SCRIPT_CLOSE: &str = "tauri-kit-webview.close-on-script-close";

/// Closes the window that holds the web view when its page calls `window.close()` (see the Windows
/// entry). The web view must already be in its window. Call on the main thread.
///
/// The close reaches the web view's UI delegate, whose class is shared by every web view the
/// framework makes; the method added to it closes only windows marked by this call.
#[cfg(target_os = "macos")]
pub fn close_window_on_script_close(web_view: &objc2_web_kit::WKWebView) {
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;
    use objc2_app_kit::NSUserInterfaceItemIdentification;
    use objc2_foundation::NSString;
    use objc2_web_kit::WKWebView;

    unsafe extern "C-unwind" fn did_close(_this: &AnyObject, _cmd: Sel, web_view: &WKWebView) {
        if let Some(window) = web_view.window() {
            if window
                .identifier()
                .is_some_and(|id| id.to_string() == CLOSE_ON_SCRIPT_CLOSE)
            {
                window.performClose(None);
            }
        }
    }

    // SAFETY: the caller's live web view, on the main thread. The delegate is the framework's
    // object; a method is added to its class only when the class has none of that name, and the
    // type string matches `void (id self, SEL _cmd, id webView)`.
    unsafe {
        let Some(window) = web_view.window() else {
            return;
        };
        window.setIdentifier(Some(&NSString::from_str(CLOSE_ON_SCRIPT_CLOSE)));
        let Some(delegate) = web_view.UIDelegate() else {
            return;
        };
        let class: &AnyClass = (*(&*delegate as *const _ as *const AnyObject)).class();
        let name = sel!(webViewDidClose:);
        if !objc2::ffi::class_respondsToSelector(class, name).as_bool() {
            let imp: Imp = std::mem::transmute(
                did_close as unsafe extern "C-unwind" fn(&AnyObject, Sel, &WKWebView),
            );
            objc2::ffi::class_addMethod(
                class as *const AnyClass as *mut AnyClass,
                name,
                imp,
                c"v@:@".as_ptr(),
            );
        }
        // The web view reads which methods its delegate answers when the delegate is set; set it
        // again so the added one is seen.
        web_view.setUIDelegate(Some(&delegate));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_person(url: &str, sized: bool) -> Opening {
        opening(Request {
            url,
            user_initiated: Some(true),
            sized,
        })
    }

    #[test]
    fn refuses_what_the_person_did_not_ask_for() {
        for user_initiated in [None, Some(false)] {
            let request = Request {
                url: "https://example.com/",
                user_initiated,
                sized: true,
            };
            assert_eq!(opening(request), Opening::Refuse);
        }
    }

    #[test]
    fn keeps_the_opener_of_a_sized_window() {
        assert_eq!(
            by_person("https://accounts.example.com/signin", true),
            Opening::KeepOpener
        );
        assert_eq!(by_person("about:blank", true), Opening::KeepOpener);
        assert_eq!(by_person("HTTP://example.com/", true), Opening::KeepOpener);
    }

    #[test]
    fn leaves_an_unsized_web_page_to_the_app() {
        assert_eq!(
            by_person("https://example.com/doc", false),
            Opening::Elsewhere
        );
    }

    #[test]
    fn refuses_what_is_not_a_web_page() {
        assert_eq!(by_person("about:blank", false), Opening::Refuse);
        assert_eq!(by_person("file:///etc/hosts", true), Opening::Refuse);
        assert_eq!(by_person("javascript:alert(1)", true), Opening::Refuse);
        assert_eq!(
            by_person("mailto:someone@example.com", true),
            Opening::Refuse
        );
        assert_eq!(by_person("not a url", true), Opening::Refuse);
    }
}
