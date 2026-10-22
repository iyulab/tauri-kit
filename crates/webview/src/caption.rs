//! Windows 11's Snap Layouts on a maximize button the page draws.
//!
//! A window without the system caption draws its own title bar in the page, minimize · maximize · close included. Windows 11
//! shows the Snap Layouts flyout over a maximize button only when the window under the cursor answers the non-client hit test
//! with `HTMAXBUTTON` — and in a WebView2 window that window is the web view's content window, which belongs to the browser
//! process: it cannot be subclassed, and no CSS `app-region` value makes it answer as a maximize button.
//!
//! [`MaximizeButton`] places a small input-sink child window of the app's own window over the page's button (the arrangement
//! Windows Terminal uses over its content, and the framework's own resize borders use over the web view): it answers `HTMAXBUTTON`, so the flyout appears; a click on it maximizes or restores
//! the window; and it reports [`ButtonEvent`]s, because the page under it sees no mouse at all and still has to draw hover and
//! pressed. The page measures its button and passes the rectangle in client pixels ([`ClientRect`]), again whenever the layout,
//! the window size or the scale changes — this module does no scaling of its own. Keyboard and screen readers stay with the
//! page's own button; the sink only takes the mouse.
//!
//! The top resize band is left to the window: over its top edge the sink passes the hit test on, so the window still resizes
//! from there. On macOS and Linux there is no such flyout and [`MaximizeButton`] does nothing.

/// A rectangle in the window's client area, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// What the mouse does over the button, for the page to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonEvent {
    /// The pointer came over the button.
    Enter,
    /// The pointer left it (a press in progress is dropped).
    Leave,
    /// The primary button went down on it.
    Down,
    /// The primary button came up on it after a press — the window has been maximized or restored.
    Up,
}

/// The answer over the sink: the maximize button, or — inside the window's top resize band while it is not maximized — passed
/// on, so the window's own resize handling answers there.
#[cfg(any(windows, test))]
fn hit(client_y: i32, resize_band: i32, maximized: bool) -> Hit {
    if !maximized && client_y < resize_band {
        Hit::PassOn
    } else {
        Hit::MaximizeButton
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    MaximizeButton,
    PassOn,
}

/// What a release does: toggle only when the press began on the button.
#[cfg(any(windows, test))]
fn on_release(pressed: bool, maximized: bool) -> Option<Toggle> {
    pressed.then_some(if maximized {
        Toggle::Restore
    } else {
        Toggle::Maximize
    })
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Maximize,
    Restore,
}

#[cfg(windows)]
pub use imp::MaximizeButton;

#[cfg(not(windows))]
pub use other::MaximizeButton;

#[cfg(not(windows))]
mod other {
    use super::{ButtonEvent, ClientRect};
    use std::io;

    /// On macOS and Linux there is no Snap Layouts flyout: nothing is placed.
    pub struct MaximizeButton;

    impl MaximizeButton {
        /// Does nothing here; see the Windows documentation.
        pub fn attach(
            _window: isize,
            _name: &str,
            _on_event: impl Fn(ButtonEvent) + Send + Sync + 'static,
        ) -> io::Result<Self> {
            Ok(Self)
        }

        /// Does nothing here.
        pub fn set_rect(&self, _rect: Option<ClientRect>) {}
    }
}

#[cfg(windows)]
mod imp {
    use super::{hit, on_release, ButtonEvent, ClientRect, Hit, Toggle};
    use std::collections::HashMap;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    };
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, IsZoomed, RegisterClassExW, SendMessageW,
        SetWindowPos, ShowWindow, HTCAPTION, HTMAXBUTTON, HTTRANSPARENT, HWND_TOP,
        SM_CXPADDEDBORDER, SM_CYSIZEFRAME, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_SHOWWINDOW, SW_MAXIMIZE, SW_RESTORE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK,
        WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONDOWN,
        WM_NCRBUTTONUP, WM_SIZE, WM_WINDOWPOSCHANGED, WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS,
    };

    const CLASS: &str = "TauriKitMaximizeButtonSink";
    const SUBCLASS_ID: usize = 0x7a6b_6d78; // the parent's subclass that keeps the sink on top

    struct Sink {
        parent: isize,
        on_event: Box<dyn Fn(ButtonEvent) + Send + Sync>,
        tracking: AtomicBool,
        pressed: AtomicBool,
    }

    /// The sinks by their window, read by the window procedure.
    fn sinks() -> &'static Mutex<HashMap<isize, Arc<Sink>>> {
        static SINKS: OnceLock<Mutex<HashMap<isize, Arc<Sink>>>> = OnceLock::new();
        SINKS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn sink_of(hwnd: HWND) -> Option<Arc<Sink>> {
        sinks().lock().ok()?.get(&(hwnd as isize)).cloned()
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn register_class() -> io::Result<()> {
        static REGISTERED: OnceLock<bool> = OnceLock::new();
        let ok = *REGISTERED.get_or_init(|| {
            let name = wide(CLASS);
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(sink_proc),
                hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
                lpszClassName: name.as_ptr(),
                // Everything else zero: no cursor, no background brush — the sink never paints.
                ..unsafe { std::mem::zeroed() }
            };
            unsafe { RegisterClassExW(&class) != 0 }
        });
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(
                "the maximize button's window class could not be registered",
            ))
        }
    }

    /// An input-sink window over the page's maximize button (module documentation).
    pub struct MaximizeButton {
        hwnd: isize,
        parent: isize,
    }

    impl MaximizeButton {
        /// Places the sink in `window` (the top-level window's handle, as an integer), hidden until [`set_rect`](Self::set_rect)
        /// gives it a place. `name` is the window name assistive technology may read when the pointer is over it — the page's
        /// own label for the button. Call on the window's own thread (the thread that runs its message loop).
        pub fn attach(
            window: isize,
            name: &str,
            on_event: impl Fn(ButtonEvent) + Send + Sync + 'static,
        ) -> io::Result<Self> {
            register_class()?;
            let class = wide(CLASS);
            let title = wide(name);
            // A plain child that never paints (no background brush, no WM_PAINT of its own): the web view's pixels show through.
            // Not layered — a layered child needs the app's manifest to declare Windows 8 or later, which a framework's default
            // manifest may not, and creation then fails without an error code.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_CHILD | WS_CLIPSIBLINGS,
                    0,
                    0,
                    0,
                    0,
                    window as HWND,
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                )
            };
            if hwnd.is_null() {
                return Err(io::Error::last_os_error());
            }
            let sink = Arc::new(Sink {
                parent: window,
                on_event: Box::new(on_event),
                tracking: AtomicBool::new(false),
                pressed: AtomicBool::new(false),
            });
            sinks()
                .lock()
                .map_err(|_| io::Error::other("the maximize button registry is poisoned"))?
                .insert(hwnd as isize, sink);
            unsafe {
                SetWindowSubclass(
                    window as HWND,
                    Some(parent_proc),
                    SUBCLASS_ID,
                    hwnd as usize,
                );
            }
            Ok(Self {
                hwnd: hwnd as isize,
                parent: window,
            })
        }

        /// Moves the sink over the button (`Some`) or takes it away (`None` — the button is hidden, or the window has its
        /// system caption again). Any thread.
        pub fn set_rect(&self, rect: Option<ClientRect>) {
            let hwnd = self.hwnd as HWND;
            unsafe {
                match rect {
                    Some(r) if r.width > 0 && r.height > 0 => {
                        SetWindowPos(
                            hwnd,
                            HWND_TOP,
                            r.x,
                            r.y,
                            r.width,
                            r.height,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                    }
                    _ => {
                        SetWindowPos(
                            hwnd,
                            HWND_TOP,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE,
                        );
                    }
                }
            }
        }
    }

    impl Drop for MaximizeButton {
        fn drop(&mut self) {
            unsafe {
                RemoveWindowSubclass(self.parent as HWND, Some(parent_proc), SUBCLASS_ID);
                DestroyWindow(self.hwnd as HWND);
            }
            if let Ok(mut map) = sinks().lock() {
                map.remove(&self.hwnd);
            }
        }
    }

    /// The window's top resize band at its current scale, in physical pixels.
    fn resize_band(parent: HWND) -> i32 {
        unsafe {
            let dpi = GetDpiForWindow(parent);
            GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
    }

    fn screen_point(lparam: LPARAM) -> POINT {
        POINT {
            x: (lparam & 0xFFFF) as i16 as i32,
            y: ((lparam >> 16) & 0xFFFF) as i16 as i32,
        }
    }

    unsafe extern "system" fn sink_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let Some(sink) = sink_of(hwnd) else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        let parent = sink.parent as HWND;
        match msg {
            WM_NCHITTEST => {
                let mut p = screen_point(lparam);
                ScreenToClient(parent, &mut p);
                match hit(p.y, resize_band(parent), IsZoomed(parent) != 0) {
                    Hit::MaximizeButton => HTMAXBUTTON as LRESULT,
                    Hit::PassOn => HTTRANSPARENT as LRESULT,
                }
            }
            WM_NCMOUSEMOVE => {
                if !sink.tracking.swap(true, Ordering::SeqCst) {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut track);
                    (sink.on_event)(ButtonEvent::Enter);
                }
                0
            }
            WM_NCMOUSELEAVE => {
                sink.tracking.store(false, Ordering::SeqCst);
                sink.pressed.store(false, Ordering::SeqCst);
                (sink.on_event)(ButtonEvent::Leave);
                0
            }
            // Handled here, not by the default procedure, which would act on the system's own caption metrics.
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
                sink.pressed.store(true, Ordering::SeqCst);
                (sink.on_event)(ButtonEvent::Down);
                0
            }
            WM_NCLBUTTONUP => {
                let pressed = sink.pressed.swap(false, Ordering::SeqCst);
                if let Some(toggle) = on_release(pressed, IsZoomed(parent) != 0) {
                    ShowWindow(
                        parent,
                        if toggle == Toggle::Restore {
                            SW_RESTORE
                        } else {
                            SW_MAXIMIZE
                        },
                    );
                    (sink.on_event)(ButtonEvent::Up);
                }
                0
            }
            // The secondary button is the window's: its system menu, as over the rest of the title bar.
            WM_NCRBUTTONDOWN | WM_NCRBUTTONUP => {
                SendMessageW(parent, msg, HTCAPTION as WPARAM, lparam)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Keeps the sink above its siblings — the web view and the framework's own resize child are raised on resize too.
    unsafe extern "system" fn parent_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        sink: usize,
    ) -> LRESULT {
        let result = DefSubclassProc(hwnd, msg, wparam, lparam);
        match msg {
            WM_SIZE | WM_WINDOWPOSCHANGED => {
                SetWindowPos(
                    sink as HWND,
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                );
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(parent_proc), id);
            }
            _ => {}
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_answers_as_the_maximize_button() {
        assert_eq!(hit(20, 8, false), Hit::MaximizeButton);
        assert_eq!(hit(0, 8, true), Hit::MaximizeButton);
    }

    #[test]
    fn the_top_resize_band_is_passed_on_unless_the_window_is_maximized() {
        assert_eq!(hit(3, 8, false), Hit::PassOn);
        assert_eq!(hit(7, 8, false), Hit::PassOn);
        assert_eq!(hit(8, 8, false), Hit::MaximizeButton);
        assert_eq!(hit(3, 8, true), Hit::MaximizeButton);
    }

    #[test]
    fn a_release_toggles_only_after_a_press_on_the_button() {
        assert_eq!(on_release(true, false), Some(Toggle::Maximize));
        assert_eq!(on_release(true, true), Some(Toggle::Restore));
        assert_eq!(on_release(false, false), None);
    }
}
