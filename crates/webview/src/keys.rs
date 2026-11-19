//! Keys the app takes before the page.
//!
//! A web view gives every key to its page. An app that shows a page in a web view of its own still
//! has keys that must work there — "back to the list", "close this", "next tab" — and a page must not
//! be able to swallow them, or to fake them: a key the page could see first, it could also send
//! itself. [`intercept`] hands each key press to the app **before** the page sees it; a key the app
//! takes never reaches the page, every other key does. Nothing is put into the page.
//!
//! [`watch_focus`] also reports when the web view gains or loses keyboard focus.
//!
//! - *Windows*: WebView2's `AcceleratorKeyPressed`, which runs before the page for every key press
//!   and key held with a modifier.
//! - *macOS*: a local event monitor of the app; a key is the web view's when its window is the one
//!   the key went to and the window's first responder is the web view or inside it. The monitor
//!   lives as long as the [`Intercept`] returned.

/// A key, named the same on every platform. Letters follow the keyboard layout in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Key {
    /// A letter, lower case.
    Letter(char),
    /// A digit on the main block.
    Digit(u8),
    /// A digit on the numeric keypad.
    NumpadDigit(u8),
    /// A function key, `F(1)` to `F(24)`.
    F(u8),
    Tab,
    Escape,
    Enter,
    Backspace,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    Up,
    Down,
    /// Any other key.
    Other,
}

/// One key press with the modifiers held. `meta` is the Command key on macOS and the Windows key on
/// Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Stroke {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// Whether the web view has keyboard focus now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    In,
    Out,
}

/// A Windows virtual-key code, as a [`Key`].
#[cfg_attr(not(windows), allow(dead_code))]
fn key_of_virtual(code: u32) -> Key {
    match code {
        0x41..=0x5A => Key::Letter(char::from(b'a' + (code - 0x41) as u8)),
        0x30..=0x39 => Key::Digit((code - 0x30) as u8),
        0x60..=0x69 => Key::NumpadDigit((code - 0x60) as u8),
        0x70..=0x87 => Key::F((code - 0x70 + 1) as u8),
        0x09 => Key::Tab,
        0x1B => Key::Escape,
        0x0D => Key::Enter,
        0x08 => Key::Backspace,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x25 => Key::Left,
        0x27 => Key::Right,
        0x26 => Key::Up,
        0x28 => Key::Down,
        _ => Key::Other,
    }
}

/// A macOS key code (and the characters it typed, without modifiers), as a [`Key`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn key_of_mac(code: u16, characters: &str) -> Key {
    const DIGITS: [(u16, u8); 10] = [
        (29, 0),
        (18, 1),
        (19, 2),
        (20, 3),
        (21, 4),
        (23, 5),
        (22, 6),
        (26, 7),
        (28, 8),
        (25, 9),
    ];
    const NUMPAD: [(u16, u8); 10] = [
        (82, 0),
        (83, 1),
        (84, 2),
        (85, 3),
        (86, 4),
        (87, 5),
        (88, 6),
        (89, 7),
        (91, 8),
        (92, 9),
    ];
    const FUNCTION: [(u16, u8); 12] = [
        (122, 1),
        (120, 2),
        (99, 3),
        (118, 4),
        (96, 5),
        (97, 6),
        (98, 7),
        (100, 8),
        (101, 9),
        (109, 10),
        (103, 11),
        (111, 12),
    ];
    if let Some((_, d)) = NUMPAD.iter().find(|(c, _)| *c == code) {
        return Key::NumpadDigit(*d);
    }
    if let Some((_, d)) = DIGITS.iter().find(|(c, _)| *c == code) {
        return Key::Digit(*d);
    }
    if let Some((_, n)) = FUNCTION.iter().find(|(c, _)| *c == code) {
        return Key::F(*n);
    }
    match code {
        48 => Key::Tab,
        53 => Key::Escape,
        36 | 76 => Key::Enter,
        51 => Key::Backspace,
        116 => Key::PageUp,
        121 => Key::PageDown,
        115 => Key::Home,
        119 => Key::End,
        123 => Key::Left,
        124 => Key::Right,
        126 => Key::Up,
        125 => Key::Down,
        _ => match characters.chars().next() {
            Some(c) if c.is_alphabetic() => Key::Letter(c.to_lowercase().next().unwrap_or(c)),
            _ => Key::Other,
        },
    }
}

/// Hands each key press in the web view to `take` before the page sees it; a press `take` answers
/// `true` for never reaches the page. Call once per web view, on its thread. `take` runs inside the
/// web view's event: keep it short, and open nothing modal from it. A panic in it counts as `false`.
#[cfg(windows)]
pub fn intercept(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    take: impl Fn(Stroke) -> bool + 'static,
) -> windows_core::Result<()> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use webview2_com::AcceleratorKeyPressedEventHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
        COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };

    let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
        let mut code = 0u32;
        // SAFETY: the event's arguments live for the callback; key state is this (UI) thread's.
        unsafe {
            args.KeyEventKind(&mut kind)?;
            if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
            {
                return Ok(());
            }
            args.VirtualKey(&mut code)?;
        }
        let down = |key: u16| unsafe { GetKeyState(i32::from(key)) } < 0;
        let stroke = Stroke {
            key: key_of_virtual(code),
            ctrl: down(VK_CONTROL),
            shift: down(VK_SHIFT),
            alt: down(VK_MENU),
            meta: down(VK_LWIN) || down(VK_RWIN),
        };
        if catch_unwind(AssertUnwindSafe(|| take(stroke))).unwrap_or(false) {
            unsafe { args.SetHandled(true)? };
        }
        Ok(())
    }));
    let mut token = 0i64;
    // SAFETY: a COM call on the caller's web view, on its thread.
    unsafe { controller.add_AcceleratorKeyPressed(&handler, &mut token)? };
    Ok(())
}

/// Reports when the web view gains or loses keyboard focus (WebView2's `GotFocus` and `LostFocus`).
/// Call once per web view, on its thread.
#[cfg(windows)]
pub fn watch_focus(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    on_focus: impl Fn(Focus) + 'static,
) -> windows_core::Result<()> {
    use std::rc::Rc;
    use webview2_com::FocusChangedEventHandler;

    let on_focus: Rc<dyn Fn(Focus)> = Rc::new(on_focus);
    let report = |focus: Focus| {
        let on_focus = on_focus.clone();
        FocusChangedEventHandler::create(Box::new(move |_, _| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_focus(focus)));
            Ok(())
        }))
    };
    let (mut got, mut lost) = (0i64, 0i64);
    // SAFETY: COM calls on the caller's web view, on its thread.
    unsafe {
        controller.add_GotFocus(&report(Focus::In), &mut got)?;
        controller.add_LostFocus(&report(Focus::Out), &mut lost)?;
    }
    Ok(())
}

/// Keeps the app's key monitor of [`intercept`] in place; dropping it removes the monitor.
#[cfg(target_os = "macos")]
pub struct Intercept {
    monitor: Option<objc2::rc::Retained<objc2::runtime::AnyObject>>,
}

#[cfg(target_os = "macos")]
impl Drop for Intercept {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.take() {
            // SAFETY: the monitor this value added.
            unsafe { objc2_app_kit::NSEvent::removeMonitor(&monitor) };
        }
    }
}

/// Hands each key press in the web view to `take` before the page sees it; a press `take` answers
/// `true` for never reaches the page. Keep the returned [`Intercept`] for as long as the keys should
/// be taken. Call on the main thread. A panic in `take` counts as `false`.
#[cfg(target_os = "macos")]
pub fn intercept(
    web_view: &objc2_web_kit::WKWebView,
    take: impl Fn(Stroke) -> bool + 'static,
) -> Intercept {
    use block2::RcBlock;
    use objc2::rc::Weak;
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSView};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::ptr::NonNull;

    let weak = Weak::new(web_view);
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands a live event for the call.
        let event_ref = unsafe { event.as_ref() };
        let Some(view) = weak.load() else {
            return event.as_ptr();
        };
        let ours = match (
            view.window(),
            event_ref.window(objc2::MainThreadMarker::new().expect("main thread")),
        ) {
            (Some(window), Some(target)) if window == target => window
                .firstResponder()
                .and_then(|responder| responder.downcast::<NSView>().ok())
                .is_some_and(|responder| responder.isDescendantOf(&view)),
            _ => false,
        };
        if !ours {
            return event.as_ptr();
        }
        let flags = event_ref.modifierFlags();
        let characters = event_ref
            .charactersIgnoringModifiers()
            .map(|c| c.to_string())
            .unwrap_or_default();
        let stroke = Stroke {
            key: key_of_mac(event_ref.keyCode(), &characters),
            ctrl: flags.contains(NSEventModifierFlags::Control),
            shift: flags.contains(NSEventModifierFlags::Shift),
            alt: flags.contains(NSEventModifierFlags::Option),
            meta: flags.contains(NSEventModifierFlags::Command),
        };
        if catch_unwind(AssertUnwindSafe(|| take(stroke))).unwrap_or(false) {
            std::ptr::null_mut()
        } else {
            event.as_ptr()
        }
    });
    // SAFETY: a local monitor of this app, added on the main thread; removed when `Intercept` drops.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
    };
    Intercept { monitor }
}

/// Keeps the observers of [`watch_focus`] in place; dropping it removes them.
#[cfg(target_os = "macos")]
pub struct FocusWatch {
    observers: Vec<
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>,
    >,
}

#[cfg(target_os = "macos")]
impl Drop for FocusWatch {
    fn drop(&mut self) {
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        for observer in self.observers.drain(..) {
            // SAFETY: an observer this value added to the default center.
            unsafe { center.removeObserver(observer.as_ref()) };
        }
    }
}

/// Reports when the web view gains or loses keyboard focus: it has focus while its window is the key
/// window and the window's first responder is the web view or inside it. The first report is the
/// state at the first window update after the call; after that only changes are reported. Keep the
/// returned [`FocusWatch`] for as long as focus should be reported. Call on the main thread.
#[cfg(target_os = "macos")]
pub fn watch_focus(
    web_view: &objc2_web_kit::WKWebView,
    on_focus: impl Fn(Focus) + 'static,
) -> FocusWatch {
    use block2::RcBlock;
    use objc2::rc::Weak;
    use objc2_app_kit::{NSView, NSWindowDidResignKeyNotification, NSWindowDidUpdateNotification};
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    use std::cell::Cell;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::ptr::NonNull;
    use std::rc::Rc;

    let weak = Weak::new(web_view);
    let last: Rc<Cell<Option<Focus>>> = Rc::new(Cell::new(None));
    let on_focus: Rc<dyn Fn(Focus)> = Rc::new(on_focus);
    // Any window's update or loss of key status may move focus into or out of the web view; the
    // check itself is two property reads, and a report is made only when the answer changes.
    let handler = RcBlock::new(move |_: NonNull<NSNotification>| {
        let Some(view) = weak.load() else { return };
        let now = match view.window() {
            Some(window) if window.isKeyWindow() => window
                .firstResponder()
                .and_then(|responder| responder.downcast::<NSView>().ok())
                .is_some_and(|responder| responder.isDescendantOf(&view)),
            _ => false,
        };
        let now = if now { Focus::In } else { Focus::Out };
        if last.replace(Some(now)) != Some(now) {
            let _ = catch_unwind(AssertUnwindSafe(|| on_focus(now)));
        }
    });
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: observers on the default center for window notifications, which AppKit posts on the
    // main thread; the block runs where the notification is posted. Removed when `FocusWatch` drops.
    let observers = unsafe {
        [
            NSWindowDidUpdateNotification,
            NSWindowDidResignKeyNotification,
        ]
    }
    .into_iter()
    .map(|name| unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &handler)
    })
    .collect();
    FocusWatch { observers }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_windows_keys() {
        assert_eq!(key_of_virtual(0x4C), Key::Letter('l'));
        assert_eq!(key_of_virtual(0x31), Key::Digit(1));
        assert_eq!(key_of_virtual(0x69), Key::NumpadDigit(9));
        assert_eq!(key_of_virtual(0x75), Key::F(6));
        assert_eq!(key_of_virtual(0x09), Key::Tab);
        assert_eq!(key_of_virtual(0x22), Key::PageDown);
        assert_eq!(key_of_virtual(0xFF), Key::Other);
    }

    #[test]
    fn names_mac_keys() {
        assert_eq!(key_of_mac(37, "l"), Key::Letter('l'));
        assert_eq!(key_of_mac(37, "L"), Key::Letter('l'));
        assert_eq!(key_of_mac(18, "1"), Key::Digit(1));
        assert_eq!(key_of_mac(92, "9"), Key::NumpadDigit(9));
        assert_eq!(key_of_mac(97, ""), Key::F(6));
        assert_eq!(key_of_mac(53, "\u{1b}"), Key::Escape);
        assert_eq!(key_of_mac(121, ""), Key::PageDown);
        assert_eq!(key_of_mac(44, "/"), Key::Other);
    }
}
