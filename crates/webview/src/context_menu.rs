//! An item of the app's own in the web view's context menu, for selected text.
//!
//! A page that wants its own context menu entry has to replace the whole menu — and with it copy,
//! paste, spelling and the platform's other entries. The web view's own menu is the one to extend:
//! WebView2 raises `ContextMenuRequested` with the menu's items and what was right-clicked, and on
//! macOS a web view is an `NSView` that is handed its menu in `willOpenMenu:withEvent:` before it
//! shows. [`add_selection_item`] puts one item at the top of that menu when text is selected, and calls
//! the app with that text when the person picks it.
//!
//! On macOS the framework's web view class does not take a menu hook, so the method is added to that
//! class at run time (once; the web view's own implementation still runs first). Selected text is
//! recognised by the Copy entry the web view puts in its menu for a selection, and read from the page
//! (`getSelection()`) when the item is picked. Other platforms: not supported.

/// What the person picked the item for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// The text selected where the menu was opened, as the page has it.
    pub selection: String,
}

/// Adds an item labelled `label()` — read each time the menu opens, so it follows the app's language —
/// to the context menu the web view shows for selected text, and calls `on_pick` with that text when
/// the person picks it. Call once per web view.
#[cfg(windows)]
pub fn add_selection_item(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    label: impl Fn() -> String + 'static,
    on_pick: impl Fn(Picked) + 'static,
) -> windows_core::Result<()> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Environment9, ICoreWebView2_11, ICoreWebView2_2,
        COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
    };
    use webview2_com::{
        take_pwstr, ContextMenuRequestedEventHandler, CustomItemSelectedEventHandler,
    };
    use windows_core::{Interface, BOOL, HSTRING, PCWSTR, PWSTR};

    // SAFETY: COM calls on the web view the caller holds, on its thread; every string the runtime
    // hands over is freed by `take_pwstr`.
    let core = unsafe { controller.CoreWebView2()? };
    let environment: ICoreWebView2Environment9 =
        unsafe { core.cast::<ICoreWebView2_2>()?.Environment()? }.cast()?;
    let core: ICoreWebView2_11 = core.cast()?;
    let on_pick = Rc::new(on_pick);
    let handler = ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let target = unsafe { args.ContextMenuTarget()? };
        let mut selected = BOOL::default();
        unsafe { target.HasSelection(&mut selected)? };
        if !selected.as_bool() {
            return Ok(());
        }
        let mut text = PWSTR::null();
        unsafe { target.SelectionText(&mut text)? };
        let selection = take_pwstr(text);
        let title = HSTRING::from(label());
        let item = unsafe {
            environment.CreateContextMenuItem(
                PCWSTR(title.as_ptr()),
                None,
                COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
            )?
        };
        let on_pick = on_pick.clone();
        let picked = CustomItemSelectedEventHandler::create(Box::new(move |_, _| {
            let selection = selection.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| on_pick(Picked { selection })));
            Ok(())
        }));
        let mut token = 0i64;
        unsafe {
            item.add_CustomItemSelected(&picked, &mut token)?;
            args.MenuItems()?.InsertValueAtIndex(0, &item)?;
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { core.add_ContextMenuRequested(&handler, &mut token)? };
    Ok(())
}

/// Adds an item labelled `label()` — read each time the menu opens, so it follows the app's language —
/// to the context menu the web view shows for selected text, and calls `on_pick` with that text when
/// the person picks it. Call once per web view, on the main thread.
#[cfg(target_os = "macos")]
pub fn add_selection_item(
    webview: &objc2_web_kit::WKWebView,
    label: impl Fn() -> String + 'static,
    on_pick: impl Fn(Picked) + 'static,
) -> Result<(), Unsupported> {
    mac::add(webview, Box::new(label), Box::new(on_pick))
}

/// The context menu of this web view cannot be extended: its class already has a menu hook this
/// crate did not add.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported;

#[cfg(target_os = "macos")]
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the web view's class already has a context menu hook of its own")
    }
}

#[cfg(target_os = "macos")]
impl std::error::Error for Unsupported {}

/// Whether a menu the web view opened is the one for selected text: the web view puts its Copy entry
/// there (and only there, among the menus a page shows).
#[cfg(any(target_os = "macos", test))]
fn is_selection_menu<'a>(identifiers: impl IntoIterator<Item = &'a str>) -> bool {
    identifiers
        .into_iter()
        .any(|id| id == "WKMenuItemIdentifierCopy")
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{is_selection_menu, Picked, Unsupported};
    use block2::RcBlock;
    use objc2::rc::{Retained, Weak};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{
        define_class, msg_send, sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly,
    };
    use objc2_app_kit::{NSEvent, NSMenu, NSMenuItem, NSUserInterfaceItemIdentification};
    use objc2_foundation::{NSError, NSObject, NSString};
    use objc2_web_kit::WKWebView;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    struct Entry {
        webview: Weak<WKWebView>,
        label: Box<dyn Fn() -> String>,
        on_pick: Rc<dyn Fn(Picked)>,
        target: Retained<MenuTarget>,
    }

    thread_local! {
        // By the web view's address; the weak reference tells a live web view from a later one at the same address.
        static ENTRIES: RefCell<HashMap<usize, Entry>> = RefCell::new(HashMap::new());
        // The classes the menu hook was added to (a framework may use more than one web view class).
        static HOOKED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) struct TargetIvars {
        key: usize,
    }

    define_class!(
        // The menu item's target: an item keeps its target weakly, so the entry holds it.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "TauriKitContextMenuTarget"]
        #[ivars = TargetIvars]
        pub(super) struct MenuTarget;

        impl MenuTarget {
            #[unsafe(method(pick:))]
            fn pick(&self, _sender: Option<&AnyObject>) {
                picked(self.ivars().key);
            }
        }
    );

    pub(super) fn add(
        webview: &WKWebView,
        label: Box<dyn Fn() -> String>,
        on_pick: Box<dyn Fn(Picked)>,
    ) -> Result<(), Unsupported> {
        let mtm = MainThreadMarker::new().expect("add_selection_item is called on the main thread");
        hook(webview.class())?;
        let key = webview as *const WKWebView as usize;
        let target = MenuTarget::alloc(mtm).set_ivars(TargetIvars { key });
        // SAFETY: NSObject's designated initializer on a freshly allocated instance.
        let target: Retained<MenuTarget> = unsafe { msg_send![super(target), init] };
        ENTRIES.with(|entries| {
            entries.borrow_mut().insert(
                key,
                Entry {
                    webview: Weak::from(webview),
                    label,
                    on_pick: Rc::from(on_pick),
                    target,
                },
            )
        });
        Ok(())
    }

    // TODO(upstream: wry has no context menu hook for its macOS web view — with one, this method is no longer added at run time)
    /// Adds `willOpenMenu:withEvent:` to the web view's class, once. A class that already has its own
    /// (one this crate did not add) is left alone.
    fn hook(class: &AnyClass) -> Result<(), Unsupported> {
        let address = class as *const AnyClass as usize;
        if HOOKED.with(|hooked| hooked.borrow().contains(&address)) {
            return Ok(());
        }
        let selector = sel!(willOpenMenu:withEvent:);
        // SAFETY: the implementation has the method's signature (self, _cmd, NSMenu *, NSEvent *) -> void,
        // and the type encoding says so; adding a method the class does not define is what the runtime is for.
        let added = unsafe {
            objc2::ffi::class_addMethod(
                class as *const AnyClass as *mut AnyClass,
                selector,
                std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&WKWebView, Sel, &NSMenu, &NSEvent),
                    Imp,
                >(will_open_menu),
                c"v@:@@".as_ptr(),
            )
        };
        if !added.as_bool() {
            return Err(Unsupported);
        }
        HOOKED.with(|hooked| hooked.borrow_mut().push(address));
        Ok(())
    }

    unsafe extern "C-unwind" fn will_open_menu(
        this: &WKWebView,
        _cmd: Sel,
        menu: &NSMenu,
        event: &NSEvent,
    ) {
        // The web view's own menu work first (it fills the menu).
        // SAFETY: WKWebView implements willOpenMenu:withEvent:; dispatch starts at that class.
        unsafe { msg_send![super(this, WKWebView::class()), willOpenMenu: menu, withEvent: event] }
        let key = this as *const WKWebView as usize;
        let item = ENTRIES.with(|entries| {
            let entries = entries.borrow();
            let entry = entries.get(&key)?;
            let live = entry.webview.load()?;
            if !std::ptr::eq(&*live, this) {
                return None;
            }
            Some(((entry.label)(), entry.target.clone()))
        });
        let Some((label, target)) = item else { return };
        let items = menu.itemArray();
        let identifiers: Vec<String> = items
            .iter()
            .filter_map(|item| item.identifier().map(|id| id.to_string()))
            .collect();
        if !is_selection_menu(identifiers.iter().map(String::as_str)) {
            return;
        }
        let mtm = MainThreadMarker::from(this);
        // SAFETY: a new menu item with a title, an action the target implements and no key equivalent.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(&label),
                Some(sel!(pick:)),
                &NSString::new(),
            )
        };
        unsafe { item.setTarget(Some(&target)) };
        menu.insertItem_atIndex(&item, 0);
        menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), 1);
    }

    fn picked(key: usize) {
        let found = ENTRIES.with(|entries| {
            let entries = entries.borrow();
            let entry = entries.get(&key)?;
            Some((entry.webview.load()?, entry.on_pick.clone()))
        });
        let Some((webview, on_pick)) = found else {
            return;
        };
        let done = RcBlock::new(move |value: *mut AnyObject, _error: *mut NSError| {
            // SAFETY: the script answers a string (or nothing); anything else reads as no selection.
            let selection = unsafe { value.as_ref() }
                .and_then(|value| value.downcast_ref::<NSString>())
                .map(|text| text.to_string())
                .unwrap_or_default();
            if !selection.is_empty() {
                on_pick(Picked { selection });
            }
        });
        // SAFETY: evaluating a script in the page the menu was opened on; the block lives as long as the call needs.
        unsafe {
            webview.evaluateJavaScript_completionHandler(
                &NSString::from_str("String(window.getSelection())"),
                Some(&done),
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use super::is_selection_menu;

    #[test]
    fn the_menu_for_selected_text_is_the_one_with_copy() {
        assert!(is_selection_menu([
            "WKMenuItemIdentifierLookUp",
            "WKMenuItemIdentifierCopy"
        ]));
        assert!(!is_selection_menu([
            "WKMenuItemIdentifierReload",
            "WKMenuItemIdentifierGoBack"
        ]));
        assert!(!is_selection_menu([]));
    }
}
