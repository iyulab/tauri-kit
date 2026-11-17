//! A page's full screen.
//!
//! A page asks for full screen with `requestFullscreen()` (a video, a game, a slide show). The web view
//! then lays the element over its own bounds — no further — and Esc gives it back. When the web view is
//! a part of the window (a tab beside the app's own controls), the element fills only that part, and
//! the app is the one that has to give it the whole window. [`watch`] tells the app when the page enters
//! and leaves full screen, so it can.
//!
//! **The window itself.** In a Tauri app on Windows, the framework's runtime turns the *window* to full
//! screen whenever any of its web views holds a full-screen element, and back when none does — a browser's
//! convention. An app that wants the page to fill only the window, with no switch of the screen, has no
//! setting for that today; restoring the window from the `on_change` callback works, with a brief flash.
//!
//! - *Windows*: WebView2's `ContainsFullScreenElementChanged`.
//! - *macOS*: not here yet — the system web view shows a full-screen element in a window of its own.

/// Reports `true` when the web view's page enters full screen and `false` when it leaves (Esc, the
/// page's own `exitFullscreen()`, a navigation). Call once per web view, on its thread. A panic in
/// `on_change` is caught.
#[cfg(windows)]
pub fn watch(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    on_change: impl Fn(bool) + 'static,
) -> windows_core::Result<()> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use webview2_com::ContainsFullScreenElementChangedEventHandler;
    use windows_core::BOOL;

    // SAFETY: COM calls on the caller's web view, on its thread; the sender lives for the callback.
    let core = unsafe { controller.CoreWebView2()? };
    let handler =
        ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
            let Some(sender) = sender else { return Ok(()) };
            let mut full = BOOL::default();
            unsafe { sender.ContainsFullScreenElement(&mut full)? };
            let full = full.as_bool();
            let _ = catch_unwind(AssertUnwindSafe(|| on_change(full)));
            Ok(())
        }));
    let mut token = 0i64;
    unsafe { core.add_ContainsFullScreenElementChanged(&handler, &mut token)? };
    Ok(())
}
