//! Picking an element of a page, as a browser's "select an element" tool does.
//!
//! An app that acts on a part of a page — "change this", "what is this" — lets the person point at
//! it. Doing that with a script in the page (a click handler, a box drawn into the page) lets the page
//! see the picking, stop it, or answer with an element of its choosing; a page the app does not trust
//! must not have that power. Here **nothing is put into the page**:
//!
//! - *Windows*: the browser's own inspect mode (DevTools `Overlay.setInspectMode`). The browser draws
//!   the highlight, the click never reaches the page, and the element arrives as a DevTools event.
//! - *macOS*: a transparent view over the web view takes the pointer and the keys; the element under
//!   the pointer is found in a script world of this module's own, which the page cannot see, and the
//!   highlight is drawn by the view.
//!
//! [`start`] turns picking on. The person moves over the page (the element under the pointer is
//! highlighted), clicks to pick, or presses Esc to stop. A pick hands over the element's markup and
//! visible text ([`Event::Picked`]) and leaves a border on it, which follows it as the page scrolls or
//! resizes, while the page takes clicks again; [`clear`] removes the border, and stops a pick still
//! under way ([`Event::Cancelled`]). Elements in open shadow trees and same-origin frames are picked
//! as themselves; a cross-origin frame is picked as the frame.
//!
//! Call everything on the web view's thread (the main thread on macOS). One pick at a time per web
//! view: [`start`] on a web view that is picking stops the earlier one first.

/// The colour of the highlight: a light fill and a border while picking, the border alone after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

/// How much of a picked element to hand over, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub html: usize,
    pub text: usize,
}

/// A picked element, read in a world the page cannot reach — a page that rewrites `outerHTML` or
/// `innerText` on its elements does not change what arrives here.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// The element's markup, cut to [`Limits::html`].
    pub html: String,
    /// Its visible text, trimmed and cut to [`Limits::text`].
    pub text: String,
    /// Its tag name, lower case.
    pub tag: String,
    /// A selector that finds it again in its document (or shadow root): its id, or a `data-testid`,
    /// `aria-label`, `name` or `title` that only it has, and positional steps from the nearest such
    /// ancestor only when it has none of those.
    pub selector: String,
    /// Where it is, in CSS pixels of the web view's top document: `[left, top, width, height]`.
    pub rect: [f64; 4],
    /// A few computed styles, as the browser reports them.
    pub styles: Styles,
}

/// Computed styles of a picked element.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Styles {
    pub color: String,
    pub background: String,
    pub font_size: String,
}

/// How a pick ended. Reported once per [`start`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Picked(Element),
    /// Esc, [`clear`], a new [`start`], or a click where there is no element.
    Cancelled,
}

fn cut(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// A function, called with the element as `this` and the two limits, that describes it as a JSON
/// string — run in a world the page cannot reach.
const DESCRIBE: &str = r#"function (htmlLimit, textLimit) {
  const el = this, doc = el.ownerDocument, view = doc.defaultView;
  const esc = (s) => (view.CSS && view.CSS.escape) ? view.CSS.escape(s) : String(s).replace(/[^\w-]/g, (c) => '\\' + c);
  const root = el.getRootNode();
  const unique = (sel) => { try { return root.querySelectorAll(sel).length === 1; } catch { return false; } };
  const anchor = (e) => {
    if (e.id && unique('#' + esc(e.id))) return '#' + esc(e.id);
    for (const a of ['data-testid', 'aria-label', 'name', 'title']) {
      const v = e.getAttribute(a);
      if (v) { const s = e.localName + '[' + a + '="' + v.replace(/["\\]/g, '\\$&') + '"]'; if (unique(s)) return s; }
    }
    return null;
  };
  let selector = anchor(el);
  if (!selector) {
    const parts = [];
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
      const a = e === el ? null : anchor(e);
      if (a) { parts.unshift(a); break; }
      let n = 1;
      for (let s = e.previousElementSibling; s; s = s.previousElementSibling) if (s.localName === e.localName) n++;
      parts.unshift(e.localName + ':nth-of-type(' + n + ')');
    }
    selector = parts.join(' > ');
  }
  const b = el.getBoundingClientRect();
  let x = b.left, y = b.top, w = view;
  for (let i = 0; w && w.frameElement && i < 16; i++) { const f = w.frameElement.getBoundingClientRect(); x += f.left + w.frameElement.clientLeft; y += f.top + w.frameElement.clientTop; w = w.parent; }
  const style = view.getComputedStyle(el);
  return JSON.stringify({
    html: String(el.outerHTML || '').slice(0, htmlLimit),
    text: String(el.innerText || el.textContent || '').trim().slice(0, textLimit),
    tag: el.localName, selector, rect: [x, y, b.width, b.height],
    styles: { color: style.color, background: style.backgroundColor, fontSize: style.fontSize },
  });
}"#;

/// The element a [`DESCRIBE`] answer names, cut to `limits` — `None` when it is not one.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn element_of(found: &serde_json::Value, limits: Limits) -> Option<Element> {
    let text = |v: &serde_json::Value| v.as_str().unwrap_or("").to_owned();
    let (x, y, w, h) = rect_of(found)?;
    Some(Element {
        html: cut(found["html"].as_str()?, limits.html),
        text: cut(found["text"].as_str().unwrap_or("").trim(), limits.text),
        tag: text(&found["tag"]),
        selector: text(&found["selector"]),
        rect: [x, y, w, h],
        styles: Styles {
            color: text(&found["styles"]["color"]),
            background: text(&found["styles"]["background"]),
            font_size: text(&found["styles"]["fontSize"]),
        },
    })
}

#[cfg(target_os = "macos")]
pub use mac::{clear, start};
#[cfg(windows)]
pub use windows_pick::{clear, start};

/// The script (a function body) that finds the element at a point of the page, in CSS pixels of the
/// top document's viewport: through open shadow roots and into same-origin frames. Returns a JSON
/// string — `null`, or `{ rect: [x, y, width, height], html?, text? }`. With `full`, the element is
/// also remembered (for [`REMEASURE`]) and its markup and text are read.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn locate_script(x: f64, y: f64, full: Option<Limits>) -> String {
    let tail = match full {
        Some(limits) => format!(
            "globalThis.pickedElement = el;\nreturn ({DESCRIBE}).call(el, {}, {});",
            limits.html, limits.text
        ),
        None => "return JSON.stringify({ rect });".to_owned(),
    };
    format!(
        r#"let doc = document, x = {x}, y = {y}, ox = 0, oy = 0, el = doc.elementFromPoint(x, y);
for (let i = 0; el && i < 16; i++) {{
  const root = el.shadowRoot;
  if (root) {{ const inner = root.elementFromPoint(x, y); if (inner && inner !== el) {{ el = inner; continue; }} }}
  if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {{
    let inner = null;
    try {{ inner = el.contentDocument; }} catch {{}}
    if (inner) {{
      const r = el.getBoundingClientRect(), dx = r.left + el.clientLeft, dy = r.top + el.clientTop;
      const got = inner.elementFromPoint(x - dx, y - dy);
      if (got) {{ ox += dx; oy += dy; x -= dx; y -= dy; el = got; doc = inner; continue; }}
    }}
  }}
  break;
}}
if (!el) return JSON.stringify(null);
const b = el.getBoundingClientRect();
const rect = [b.left + ox, b.top + oy, b.width, b.height];
{tail}"#
    )
}

/// The script that measures the remembered element again, in the top document's coordinates —
/// `null` when it has left the page.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const REMEASURE: &str = r#"const el = globalThis.pickedElement;
if (!el || !el.isConnected) return JSON.stringify(null);
const b = el.getBoundingClientRect();
let x = b.left, y = b.top, w = el.ownerDocument.defaultView;
for (let i = 0; w && w.frameElement && i < 16; i++) { const f = w.frameElement.getBoundingClientRect(); x += f.left + w.frameElement.clientLeft; y += f.top + w.frameElement.clientTop; w = w.parent; }
return JSON.stringify({ rect: [x, y, b.width, b.height] });"#;

/// A rectangle of the page, in CSS pixels: left, top, width, height.
type Rect = (f64, f64, f64, f64);

fn rect_of(found: &serde_json::Value) -> Option<Rect> {
    let r = found.get("rect")?.as_array()?;
    Some((
        r.first()?.as_f64()?,
        r.get(1)?.as_f64()?,
        r.get(2)?.as_f64()?,
        r.get(3)?.as_f64()?,
    ))
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{element_of, locate_script, rect_of, Event, Limits, Look, Rect, REMEASURE};
    use block2::RcBlock;
    use objc2::rc::{Allocated, Retained, Weak};
    use objc2::runtime::AnyObject;
    use objc2::{class, define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSEvent, NSResponder, NSView};
    use objc2_foundation::{NSError, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer};
    use objc2_web_kit::{WKContentWorld, WKWebView};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::ptr::NonNull;
    use std::rc::Rc;

    /// The name of this module's script world — the page's scripts never run in it.
    const WORLD: &str = "tauri-kit-pick";
    /// `kVK_Escape`.
    const ESCAPE: u16 = 53;
    /// `NSTrackingMouseMoved | NSTrackingActiveAlways | NSTrackingInVisibleRect`.
    const TRACKING: usize = 0x02 | 0x80 | 0x200;
    /// `NSViewWidthSizable | NSViewHeightSizable`.
    const SIZABLE: usize = 2 | 16;
    /// How often the border is moved to where the picked element is now.
    const FOLLOW_SECONDS: f64 = 0.25;

    enum Pointer {
        Hover(f64, f64),
        Pick(f64, f64),
        Cancel,
    }

    pub struct CoverIvars {
        on_pointer: Box<dyn Fn(Pointer)>,
        /// Taking clicks (picking) or showing the border only (picked).
        picking: Cell<bool>,
        /// The highlight, in the cover's (flipped) coordinates.
        mark: Cell<Option<NSRect>>,
        /// The web view's page zoom: CSS pixels × zoom = points.
        zoom: f64,
        look: Look,
    }

    define_class!(
        #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = CoverIvars]
        #[name = "TauriKitPickCover"]
        struct Cover;

        unsafe impl NSObjectProtocol for Cover {}

        impl Cover {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                true
            }

            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }

            /// The first click picks even when the window is not in front.
            #[unsafe(method(acceptsFirstMouse:))]
            fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
                true
            }

            /// Once picked, clicks go through to the page.
            #[unsafe(method(hitTest:))]
            fn hit_test(&self, point: NSPoint) -> *mut NSView {
                if self.ivars().picking.get() {
                    unsafe { msg_send![super(self), hitTest: point] }
                } else {
                    std::ptr::null_mut()
                }
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, event: &NSEvent) {
                let (x, y) = self.page_point(event);
                (self.ivars().on_pointer)(Pointer::Pick(x, y));
            }

            #[unsafe(method(mouseMoved:))]
            fn mouse_moved(&self, event: &NSEvent) {
                if self.ivars().picking.get() {
                    let (x, y) = self.page_point(event);
                    (self.ivars().on_pointer)(Pointer::Hover(x, y));
                }
            }

            /// While picking, keys do not reach the page; Esc stops.
            #[unsafe(method(keyDown:))]
            fn key_down(&self, event: &NSEvent) {
                if event.keyCode() == ESCAPE {
                    (self.ivars().on_pointer)(Pointer::Cancel);
                }
            }

            #[unsafe(method(cancelOperation:))]
            fn cancel_operation(&self, _sender: Option<&AnyObject>) {
                (self.ivars().on_pointer)(Pointer::Cancel);
            }

            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                let Some(rect) = self.ivars().mark.get() else { return };
                let look = self.ivars().look;
                let (r, g, b) = (look.red as f64 / 255.0, look.green as f64 / 255.0, look.blue as f64 / 255.0);
                unsafe {
                    if self.ivars().picking.get() {
                        let fill: Retained<AnyObject> = msg_send![class!(NSColor), colorWithSRGBRed: r, green: g, blue: b, alpha: 0.08f64];
                        let _: () = msg_send![&*fill, set];
                        let _: () = msg_send![class!(NSBezierPath), fillRect: rect];
                    }
                    let line: Retained<AnyObject> = msg_send![class!(NSColor), colorWithSRGBRed: r, green: g, blue: b, alpha: 1.0f64];
                    let _: () = msg_send![&*line, set];
                    let inner = NSRect::new(
                        NSPoint::new(rect.origin.x + 1.0, rect.origin.y + 1.0),
                        NSSize::new((rect.size.width - 2.0).max(0.0), (rect.size.height - 2.0).max(0.0)),
                    );
                    let path: Retained<AnyObject> = msg_send![class!(NSBezierPath), bezierPathWithRect: inner];
                    let _: () = msg_send![&*path, setLineWidth: 2.0f64];
                    let _: () = msg_send![&*path, stroke];
                }
            }
        }
    );

    impl Cover {
        fn new(frame: NSRect, ivars: CoverIvars, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ivars);
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            // SAFETY: a tracking area owned by the new view, and its resizing mask.
            unsafe {
                let area: Allocated<AnyObject> = msg_send![class!(NSTrackingArea), alloc];
                let area: Retained<AnyObject> = msg_send![area, initWithRect: NSRect::ZERO, options: TRACKING, owner: &*this, userInfo: Option::<&AnyObject>::None];
                let _: () = msg_send![&*this, addTrackingArea: &*area];
                let _: () = msg_send![&*this, setAutoresizingMask: SIZABLE];
            }
            this
        }

        fn page_point(&self, event: &NSEvent) -> (f64, f64) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let zoom = self.ivars().zoom;
            (p.x / zoom, p.y / zoom)
        }

        fn points(&self, rect: Rect) -> NSRect {
            let zoom = self.ivars().zoom;
            NSRect::new(
                NSPoint::new(rect.0 * zoom, rect.1 * zoom),
                NSSize::new(rect.2 * zoom, rect.3 * zoom),
            )
        }

        fn set_mark(&self, rect: Option<Rect>) {
            let mark = rect.map(|r| self.points(r));
            if self.ivars().mark.get() != mark {
                self.ivars().mark.set(mark);
                self.setNeedsDisplay(true);
            }
        }
    }

    /// One pick on one web view.
    struct Pick {
        cover: Retained<Cover>,
        on_event: Rc<dyn Fn(Event)>,
        /// A locate is running; the newest hover point waits for it.
        busy: Cell<bool>,
        waiting: Cell<Option<(f64, f64)>>,
        follow: RefCell<Option<Retained<NSTimer>>>,
    }

    thread_local! {
        /// Web view (by address) → its pick. Main thread only.
        static PICKS: RefCell<HashMap<usize, Rc<Pick>>> = RefCell::new(HashMap::new());
    }

    fn key(web_view: &WKWebView) -> usize {
        web_view as *const WKWebView as usize
    }

    fn pick_of(key: usize) -> Option<Rc<Pick>> {
        PICKS.with(|picks| picks.borrow().get(&key).cloned())
    }

    /// Runs `body` in this module's script world and hands its JSON answer to `then`.
    fn run(web_view: &WKWebView, body: &str, then: impl Fn(serde_json::Value) + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let block = RcBlock::new(move |result: *mut AnyObject, _error: *mut NSError| {
            // SAFETY: WebKit hands the function's result — here always a string — for this call.
            let value = unsafe { result.as_ref() }
                .and_then(|object| object.downcast_ref::<NSString>())
                .and_then(|text| serde_json::from_str(&text.to_string()).ok())
                .unwrap_or(serde_json::Value::Null);
            then(value);
        });
        // SAFETY: the caller's live web view, on the main thread; the world is this module's own.
        unsafe {
            let world = WKContentWorld::worldWithName(&NSString::from_str(WORLD), mtm);
            web_view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(body),
                None,
                None,
                &world,
                Some(&block),
            );
        }
    }

    /// Turns picking on over `web_view`; `on_event` is told once how it ended.
    pub fn start(
        web_view: &WKWebView,
        look: Look,
        limits: Limits,
        on_event: impl Fn(Event) + 'static,
    ) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        clear(web_view);
        let id = key(web_view);
        let weak = Weak::new(web_view);
        let on_pointer = move |pointer: Pointer| {
            let (Some(view), Some(pick)) = (weak.load(), pick_of(id)) else {
                return;
            };
            match pointer {
                Pointer::Hover(x, y) => hover(&view, &pick, x, y),
                Pointer::Pick(x, y) => picked(&view, id, x, y, limits),
                Pointer::Cancel => clear(&view),
            }
        };
        // SAFETY: `pageZoom` (macOS 11+), the zoom the page is drawn at.
        let zoom: f64 = unsafe { msg_send![web_view, pageZoom] };
        let ivars = CoverIvars {
            on_pointer: Box::new(on_pointer),
            picking: Cell::new(true),
            mark: Cell::new(None),
            zoom: if zoom > 0.0 { zoom } else { 1.0 },
            look,
        };
        let cover = Cover::new(web_view.bounds(), ivars, mtm);
        web_view.addSubview(&cover);
        if let Some(window) = web_view.window() {
            window.makeFirstResponder(Some(&cover));
        }
        let pick = Rc::new(Pick {
            cover,
            on_event: Rc::new(on_event),
            busy: Cell::new(false),
            waiting: Cell::new(None),
            follow: RefCell::new(None),
        });
        PICKS.with(|picks| picks.borrow_mut().insert(id, pick));
    }

    fn hover(view: &WKWebView, pick: &Rc<Pick>, x: f64, y: f64) {
        if pick.busy.replace(true) {
            pick.waiting.set(Some((x, y)));
            return;
        }
        let (id, weak) = (key(view), Weak::new(view));
        run(view, &locate_script(x, y, None), move |found| {
            let Some(pick) = pick_of(id) else { return };
            if pick.cover.ivars().picking.get() {
                pick.cover.set_mark(rect_of(&found));
            }
            pick.busy.set(false);
            if let (Some((x, y)), Some(view)) = (pick.waiting.take(), weak.load()) {
                hover(&view, &pick, x, y);
            }
        });
    }

    fn picked(view: &WKWebView, id: usize, x: f64, y: f64, limits: Limits) {
        let weak = Weak::new(view);
        run(view, &locate_script(x, y, Some(limits)), move |found| {
            let (Some(view), Some(pick)) = (weak.load(), pick_of(id)) else {
                return;
            };
            if !pick.cover.ivars().picking.get() {
                return;
            }
            let Some(rect) = rect_of(&found) else {
                clear(&view);
                return;
            };
            pick.cover.ivars().picking.set(false);
            pick.cover.set_mark(Some(rect));
            if let Some(window) = view.window() {
                window.makeFirstResponder(Some(&view));
            }
            follow(&view, &pick);
            match element_of(&found, limits) {
                Some(element) => (pick.on_event)(Event::Picked(element)),
                None => clear(&view),
            }
        });
    }

    /// Moves the border to where the picked element is now, until the pick is cleared.
    fn follow(view: &WKWebView, pick: &Rc<Pick>) {
        let (id, weak) = (key(view), Weak::new(view));
        let tick = RcBlock::new(move |timer: NonNull<NSTimer>| {
            let (Some(view), Some(_)) = (weak.load(), pick_of(id)) else {
                // SAFETY: the timer this block belongs to.
                unsafe { timer.as_ref().invalidate() };
                return;
            };
            run(&view, REMEASURE, move |found| {
                if let Some(pick) = pick_of(id) {
                    if let Some(rect) = rect_of(&found) {
                        pick.cover.set_mark(Some(rect));
                    }
                }
            });
        });
        // SAFETY: a repeating timer on the main run loop; the block runs on the main thread.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(FOLLOW_SECONDS, true, &tick)
        };
        *pick.follow.borrow_mut() = Some(timer);
    }

    /// Removes the border, and stops a pick still under way ([`Event::Cancelled`]).
    pub fn clear(web_view: &WKWebView) {
        let Some(pick) = PICKS.with(|picks| picks.borrow_mut().remove(&key(web_view))) else {
            return;
        };
        if let Some(timer) = pick.follow.borrow_mut().take() {
            timer.invalidate();
        }
        let was_picking = pick.cover.ivars().picking.get();
        pick.cover.removeFromSuperview();
        if was_picking {
            if let Some(window) = web_view.window() {
                window.makeFirstResponder(Some(web_view));
            }
            (pick.on_event)(Event::Cancelled);
        }
    }
}

#[cfg(windows)]
mod windows_pick {
    use super::{element_of, Event, Limits, Look, DESCRIBE};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2Controller, ICoreWebView2DevToolsProtocolEventReceiver,
        COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    };
    use webview2_com::{
        take_pwstr, AcceleratorKeyPressedEventHandler, CallDevToolsProtocolMethodCompletedHandler,
        DevToolsProtocolEventReceivedEventHandler,
    };
    use windows_core::{Interface, HSTRING, PWSTR};

    const ESCAPE: u32 = 0x1B;

    struct Pick {
        core: ICoreWebView2,
        controller: ICoreWebView2Controller,
        receiver: ICoreWebView2DevToolsProtocolEventReceiver,
        inspect_token: i64,
        key_token: i64,
        on_event: Rc<dyn Fn(Event)>,
        picking: Cell<bool>,
        look: Look,
    }

    thread_local! {
        /// Web view (by controller address) → its pick. The web view's thread only.
        static PICKS: RefCell<HashMap<usize, Rc<Pick>>> = RefCell::new(HashMap::new());
    }

    fn key(controller: &ICoreWebView2Controller) -> usize {
        controller.as_raw() as usize
    }

    fn pick_of(key: usize) -> Option<Rc<Pick>> {
        PICKS.with(|picks| picks.borrow().get(&key).cloned())
    }

    fn highlight(look: Look, fill: bool) -> serde_json::Value {
        let colour =
            |a: f64| serde_json::json!({ "r": look.red, "g": look.green, "b": look.blue, "a": a });
        let mut config = serde_json::json!({ "borderColor": colour(1.0), "showInfo": false, "showAccessibilityInfo": false });
        if fill {
            config["contentColor"] = colour(0.08);
        }
        config
    }

    /// Calls a DevTools method; `then` gets its answer (`Err` with the reason when it is refused).
    fn call(
        core: &ICoreWebView2,
        method: &str,
        params: serde_json::Value,
        then: impl FnOnce(Result<serde_json::Value, String>) + 'static,
    ) {
        let then = RefCell::new(Some(then));
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                if let Some(then) = then.borrow_mut().take() {
                    then(match result {
                        Ok(()) => serde_json::from_str(&json).map_err(|e| e.to_string()),
                        Err(e) => Err(if json.is_empty() { e.message() } else { json }),
                    });
                }
                Ok(())
            }));
        // SAFETY: a COM call on the caller's web view, on its thread; the handler runs once.
        let _ = unsafe {
            core.CallDevToolsProtocolMethod(
                &HSTRING::from(method),
                &HSTRING::from(params.to_string()),
                &handler,
            )
        };
    }

    /// Turns picking on in the web view; `on_event` is told once how it ended.
    pub fn start(
        controller: &ICoreWebView2Controller,
        look: Look,
        limits: Limits,
        on_event: impl Fn(Event) + 'static,
    ) -> windows_core::Result<()> {
        clear(controller);
        let id = key(controller);
        // SAFETY (every block below): COM calls on the caller's web view, on its thread; event
        // arguments live for their callback, and strings handed over are freed by `take_pwstr`.
        let core = unsafe { controller.CoreWebView2()? };
        let receiver = unsafe {
            core.GetDevToolsProtocolEventReceiver(&HSTRING::from("Overlay.inspectNodeRequested"))?
        };
        let inspected =
            DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut json = PWSTR::null();
                let json = unsafe {
                    args.ParameterObjectAsJson(&mut json)?;
                    take_pwstr(json)
                };
                let node = serde_json::from_str::<serde_json::Value>(&json)
                    .ok()
                    .and_then(|v| v["backendNodeId"].as_i64());
                if let (Some(node), Some(pick)) = (node, pick_of(id)) {
                    if pick.picking.replace(false) {
                        read(pick, node, limits);
                    }
                }
                Ok(())
            }));
        let mut inspect_token = 0i64;
        unsafe { receiver.add_DevToolsProtocolEventReceived(&inspected, &mut inspect_token)? };
        let escape = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let (mut kind, mut key) = (Default::default(), 0u32);
            unsafe {
                args.KeyEventKind(&mut kind)?;
                args.VirtualKey(&mut key)?;
            }
            if kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && key == ESCAPE {
                if let Some(pick) = pick_of(id) {
                    if pick.picking.get() {
                        unsafe { args.SetHandled(true)? };
                        let controller = pick.controller.clone();
                        clear(&controller);
                    }
                }
            }
            Ok(())
        }));
        let mut key_token = 0i64;
        unsafe { controller.add_AcceleratorKeyPressed(&escape, &mut key_token)? };
        let pick = Rc::new(Pick {
            core: core.clone(),
            controller: controller.clone(),
            receiver,
            inspect_token,
            key_token,
            on_event: Rc::new(on_event),
            picking: Cell::new(true),
            look,
        });
        PICKS.with(|picks| picks.borrow_mut().insert(id, pick));
        let inspect = serde_json::json!({ "mode": "searchForNode", "highlightConfig": highlight(look, true) });
        call(
            &core,
            "Overlay.hideHighlight",
            serde_json::json!({}),
            |_| {},
        );
        call(&core, "DOM.enable", serde_json::json!({}), |_| {});
        call(&core, "Overlay.enable", serde_json::json!({}), |_| {});
        call(&core, "Overlay.setInspectMode", inspect, |_| {});
        // SAFETY: as above.
        let _ = unsafe {
            controller.MoveFocus(webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
        };
        Ok(())
    }

    /// Describes the picked node in a world of this module's own (the page cannot change what it
    /// answers) and leaves a border on it. A node that world does not reach (in a frame) is read in
    /// its frame's own.
    fn read(pick: Rc<Pick>, node: i64, limits: Limits) {
        let core = pick.core.clone();
        let none =
            serde_json::json!({ "mode": "none", "highlightConfig": highlight(pick.look, true) });
        call(&core, "Overlay.setInspectMode", none, |_| {});
        describe(&core, node, limits, move |found| {
            let border = serde_json::json!({ "backendNodeId": node, "highlightConfig": highlight(pick.look, false) });
            call(&pick.core, "Overlay.highlightNode", border, |_| {});
            match found.and_then(|found| element_of(&found, limits)) {
                Some(element) => (pick.on_event)(Event::Picked(element)),
                None => (pick.on_event)(Event::Cancelled),
            }
        });
    }

    fn describe(
        core: &ICoreWebView2,
        node: i64,
        limits: Limits,
        then: impl FnOnce(Option<serde_json::Value>) + 'static,
    ) {
        let core2 = core.clone();
        call(
            core,
            "Page.getFrameTree",
            serde_json::json!({}),
            move |tree| {
                let frame = tree
                    .ok()
                    .and_then(|t| t["frameTree"]["frame"]["id"].as_str().map(str::to_owned));
                let world = serde_json::json!({ "frameId": frame, "worldName": "tauri-kit-pick" });
                let core3 = core2.clone();
                call(&core2, "Page.createIsolatedWorld", world, move |world| {
                    let context = world.ok().and_then(|w| w["executionContextId"].as_i64());
                    let resolve = {
                        let core = core3.clone();
                        move |params: serde_json::Value, then: Box<dyn FnOnce(Option<String>)>| {
                            call(&core, "DOM.resolveNode", params, move |object| {
                                then(object.ok().and_then(|o| {
                                    o["object"]["objectId"].as_str().map(str::to_owned)
                                }))
                            });
                        }
                    };
                    let run = {
                        let core = core3.clone();
                        move |object: Option<String>| {
                            let Some(object) = object else {
                                return then(None);
                            };
                            let params = serde_json::json!({
                                "objectId": object,
                                "functionDeclaration": DESCRIBE,
                                "arguments": [{ "value": limits.html }, { "value": limits.text }],
                                "returnByValue": true,
                            });
                            let release = core.clone();
                            call(&core, "Runtime.callFunctionOn", params, move |answer| {
                                call(
                                    &release,
                                    "Runtime.releaseObject",
                                    serde_json::json!({ "objectId": object }),
                                    |_| {},
                                );
                                then(answer.ok().and_then(|a| {
                                    a["result"]["value"]
                                        .as_str()
                                        .and_then(|json| serde_json::from_str(json).ok())
                                }));
                            });
                        }
                    };
                    match context {
                        Some(context) => {
                            let fallback = resolve.clone();
                            resolve(
                                serde_json::json!({ "backendNodeId": node, "executionContextId": context }),
                                Box::new(move |object| match object {
                                    Some(object) => run(Some(object)),
                                    None => fallback(
                                        serde_json::json!({ "backendNodeId": node }),
                                        Box::new(run),
                                    ),
                                }),
                            );
                        }
                        None => {
                            resolve(serde_json::json!({ "backendNodeId": node }), Box::new(run))
                        }
                    }
                });
            },
        );
    }

    /// Removes the border, and stops a pick still under way ([`Event::Cancelled`]).
    pub fn clear(controller: &ICoreWebView2Controller) {
        let Some(pick) = PICKS.with(|picks| picks.borrow_mut().remove(&key(controller))) else {
            return;
        };
        // SAFETY: COM calls on the caller's web view, on its thread.
        unsafe {
            let _ = pick
                .receiver
                .remove_DevToolsProtocolEventReceived(pick.inspect_token);
            let _ = pick.controller.remove_AcceleratorKeyPressed(pick.key_token);
        }
        call(
            &pick.core,
            "Overlay.hideHighlight",
            serde_json::json!({}),
            |_| {},
        );
        if pick.picking.get() {
            let none = serde_json::json!({ "mode": "none", "highlightConfig": highlight(pick.look, true) });
            call(&pick.core, "Overlay.setInspectMode", none, |_| {});
            (pick.on_event)(Event::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_element_and_cuts_it_by_characters() {
        let found = serde_json::json!({ "html": "<p id=\"a\">가나다</p>", "text": " 가나다 ", "tag": "p", "selector": "#a",
            "rect": [1.0, 2.0, 3.0, 4.0], "styles": { "color": "rgb(0, 0, 0)", "background": "rgba(0, 0, 0, 0)", "fontSize": "16px" } });
        let element = element_of(&found, Limits { html: 5, text: 2 }).unwrap();
        assert_eq!(
            (element.html.as_str(), element.text.as_str()),
            ("<p id", "가나")
        );
        assert_eq!(
            (
                element.tag.as_str(),
                element.selector.as_str(),
                element.rect
            ),
            ("p", "#a", [1.0, 2.0, 3.0, 4.0])
        );
        assert_eq!(element.styles.font_size, "16px");
        assert_eq!(
            element_of(&serde_json::Value::Null, Limits { html: 5, text: 2 }),
            None
        );
    }

    #[test]
    fn cuts_by_characters() {
        assert_eq!(cut("가나다라", 2), "가나");
        assert_eq!(cut("ab", 5), "ab");
    }

    #[test]
    fn reads_a_rect_or_none() {
        let found = serde_json::json!({ "rect": [1.0, 2.5, 30.0, 40.0] });
        assert_eq!(rect_of(&found), Some((1.0, 2.5, 30.0, 40.0)));
        assert_eq!(rect_of(&serde_json::Value::Null), None);
        assert_eq!(rect_of(&serde_json::json!({ "rect": [1.0] })), None);
    }

    #[test]
    fn the_locate_script_reads_the_element_only_when_asked() {
        let hover = locate_script(10.0, 20.0, None);
        assert!(hover.contains("elementFromPoint(x, y)") && !hover.contains("outerHTML"));
        assert!(!hover.contains("pickedElement"));
        let full = locate_script(
            10.0,
            20.0,
            Some(Limits {
                html: 4000,
                text: 300,
            }),
        );
        assert!(full.contains(".call(el, 4000, 300)") && full.contains("getComputedStyle"));
        assert!(full.contains("globalThis.pickedElement = el"));
    }
}
