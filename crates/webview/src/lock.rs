//! Keeping a window on its own origins.
//!
//! A window that shows content the app does not trust — a document someone else wrote, a page a
//! model produced, a site the person typed — can be steered away by that content: a link, a script
//! setting `location`, a redirect, a frame, a popup. [`NavigationLock`] says which origins the window
//! may show; on Windows, `lock_navigation` holds the web view to it.
//!
//! **What it is not.** The lock keeps the *window* on its origins. It does not stop a page from
//! sending data elsewhere: images, `fetch`, beacons, WebSocket, WebRTC and DNS prefetches all leave
//! without navigating. Pair it with a Content-Security-Policy (`default-src`, `connect-src`,
//! `img-src`, `form-action`, `frame-src`) for that.
//!
//! The decision itself ([`NavigationLock::decide`]) is plain code on every platform — on macOS and
//! Linux it goes into the window builder's navigation handler, without the parts only WebView2
//! offers (the block before the request leaves, the user-initiated flag, frames, external schemes).

use std::fmt;

/// The schemes whose URLs are documents with an origin. Every other scheme (`about:`, `mailto:`,
/// an app's own scheme) is not a place a document comes from, and is allowed only when the lock
/// names it in [`NavigationLock::other_schemes`].
const DOCUMENT_SCHEMES: [&str; 6] = ["http", "https", "file", "blob", "filesystem", "data"];

/// One origin a locked window may show: scheme, host and port, compared exactly — or, for `file:`,
/// the one document (its host and path; the query and fragment do not count).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    scheme: String,
    host: String,
    /// `None` is the scheme's default port.
    port: Option<u16>,
    /// The document's path, for `file:` only.
    path: Option<String>,
}

/// Why a URL has no origin a lock can compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginError {
    /// Not a URL this parser reads — or one it refuses rather than guess at: user info, a backslash,
    /// whitespace or a control character, a host that is not ASCII (pass its punycode form), a port
    /// out of range, a dot segment.
    Unreadable,
    /// A URL whose origin is opaque: `data:`, or a `blob:` made by a document without an origin.
    Opaque,
    /// A scheme that is not a document (`about:`, `mailto:`, …).
    NotADocument,
    /// An allow-list entry with more than an origin in it — a path, a query, a fragment. An entry
    /// is never narrowed to a path; one that seems to be would allow the whole origin.
    NotAnOrigin,
}

impl fmt::Display for OriginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OriginError::Unreadable => "not a URL this lock reads",
            OriginError::Opaque => "the URL has an opaque origin",
            OriginError::NotADocument => "the scheme is not a document",
            OriginError::NotAnOrigin => {
                "an allow-list entry is an origin, without a path, query or fragment"
            }
        })
    }
}

impl std::error::Error for OriginError {}

impl Origin {
    /// Reads one allow-list entry: `https://host[:port]` (a trailing `/` is fine), or
    /// `file:///path/to/document`.
    pub fn parse(entry: &str) -> Result<Origin, OriginError> {
        let (origin, rest) = read(entry, 0)?;
        let narrowed = match origin.path {
            Some(_) => rest.contains(['?', '#']),
            None => !(rest.is_empty() || rest == "/"),
        };
        if narrowed {
            return Err(OriginError::NotAnOrigin);
        }
        Ok(origin)
    }

    /// The origin a URL belongs to. A `blob:` or `filesystem:` URL belongs to the origin inside it.
    pub fn of(url: &str) -> Result<Origin, OriginError> {
        read(url, 0).map(|(origin, _)| origin)
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.host)?;
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        if let Some(path) = &self.path {
            f.write_str(path)?;
        }
        Ok(())
    }
}

/// The scheme of a URL, lowercased — `None` when there is none to read.
fn scheme_of(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first_is_letter = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    (first_is_letter && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
        .then(|| scheme.to_ascii_lowercase())
}

/// Reads the origin at the start of `url` and what follows it (path, query, fragment). `depth`
/// stops a `blob:` inside a `blob:`.
fn read(url: &str, depth: u8) -> Result<(Origin, &str), OriginError> {
    // Chromium hands over canonical URLs; anything else is refused rather than read generously.
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) || url.contains('\\') {
        return Err(OriginError::Unreadable);
    }
    let scheme = scheme_of(url).ok_or(OriginError::Unreadable)?;
    let rest = &url[scheme.len() + 1..];
    match scheme.as_str() {
        "http" | "https" => {
            let rest = rest.strip_prefix("//").ok_or(OriginError::Unreadable)?;
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let (host, port) = authority(&rest[..end])?;
            let default = if scheme == "http" { 80 } else { 443 };
            let port = port.filter(|&p| p != default);
            Ok((
                Origin {
                    scheme,
                    host,
                    port,
                    path: None,
                },
                &rest[end..],
            ))
        }
        "file" => {
            let rest = rest.strip_prefix("//").ok_or(OriginError::Unreadable)?;
            let slash = rest.find('/').ok_or(OriginError::Unreadable)?;
            let host = &rest[..slash];
            if host.contains([':', '@', '?', '#']) || !host.is_ascii() {
                return Err(OriginError::Unreadable);
            }
            let after = &rest[slash..];
            let end = after.find(['?', '#']).unwrap_or(after.len());
            let path = &after[..end];
            if path
                .split('/')
                .any(|segment| segment == "." || segment == "..")
            {
                return Err(OriginError::Unreadable);
            }
            let origin = Origin {
                scheme,
                host: host.to_ascii_lowercase(),
                port: None,
                path: Some(path.to_ascii_lowercase()),
            };
            Ok((origin, &after[end..]))
        }
        "blob" | "filesystem" if depth == 0 => {
            if rest.starts_with("null") {
                return Err(OriginError::Opaque);
            }
            match read(rest, 1) {
                // A blob made by a `file:` document has its uuid where the path would be — it is
                // never that document.
                Ok((origin, _)) if origin.path.is_some() => Err(OriginError::Opaque),
                Ok((origin, _)) => Ok((origin, "")),
                Err(OriginError::NotADocument) => Err(OriginError::Unreadable),
                Err(e) => Err(e),
            }
        }
        "blob" | "filesystem" => Err(OriginError::Unreadable),
        "data" => Err(OriginError::Opaque),
        _ => Err(OriginError::NotADocument),
    }
}

/// Host and port of an `http(s)` authority. User info is refused: `https://allowed@elsewhere`.
fn authority(authority: &str) -> Result<(String, Option<u16>), OriginError> {
    if authority.contains('@') || !authority.is_ascii() {
        return Err(OriginError::Unreadable);
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let close = v6.find(']').ok_or(OriginError::Unreadable)?;
            let after = &v6[close + 1..];
            let port = match after {
                "" => None,
                _ => Some(after.strip_prefix(':').ok_or(OriginError::Unreadable)?),
            };
            (&authority[..close + 2], port)
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let valid =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':');
    if host.is_empty() || !host.chars().all(valid) {
        return Err(OriginError::Unreadable);
    }
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            Some(p.parse::<u16>().map_err(|_| OriginError::Unreadable)?)
        }
        Some(_) => return Err(OriginError::Unreadable),
    };
    Ok((host.to_ascii_lowercase(), port))
}

/// Which origins a window may show.
#[derive(Debug, Clone, Default)]
pub struct NavigationLock {
    /// The origins the window and its frames may show.
    pub allowed: Vec<Origin>,
    /// Schemes that are not documents and that the window may still go to — for example `about`
    /// (a page leaving to `about:blank`) or `mailto` (which opens another program and leaves the
    /// window where it is). Empty by default: every one is refused. Lowercase names, no colon.
    /// Document schemes (`http`, `https`, `file`, `blob`, `filesystem`, `data`) are never read
    /// from here — they go by [`allowed`](Self::allowed).
    pub other_schemes: Vec<String>,
}

/// What a lock says about one URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Refuse(Reason),
}

/// Why a lock refused a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// A document from an origin the lock does not name.
    OutsideList,
    /// A scheme that is not a document, and that the lock does not name.
    OtherScheme,
    /// A document with an opaque origin (`data:`, a `blob:` without an origin).
    OpaqueOrigin,
    /// A URL the lock cannot read — refused rather than guessed at.
    Unreadable,
}

impl NavigationLock {
    /// A lock that allows these origins and no other scheme.
    pub fn new(allowed: impl IntoIterator<Item = Origin>) -> Self {
        NavigationLock {
            allowed: allowed.into_iter().collect(),
            other_schemes: Vec::new(),
        }
    }

    /// Also allow these schemes that are not documents (see [`other_schemes`](Self::other_schemes)).
    pub fn with_other_schemes<S: Into<String>>(
        mut self,
        schemes: impl IntoIterator<Item = S>,
    ) -> Self {
        self.other_schemes
            .extend(schemes.into_iter().map(|s| s.into().to_ascii_lowercase()));
        self
    }

    /// Whether the window may go to `url`.
    pub fn decide(&self, url: &str) -> Decision {
        let Some(scheme) = scheme_of(url) else {
            return Decision::Refuse(Reason::Unreadable);
        };
        if !DOCUMENT_SCHEMES.contains(&scheme.as_str()) {
            return if self.other_schemes.contains(&scheme) {
                Decision::Allow
            } else {
                Decision::Refuse(Reason::OtherScheme)
            };
        }
        match Origin::of(url) {
            Ok(origin) if self.allowed.contains(&origin) => Decision::Allow,
            Ok(_) => Decision::Refuse(Reason::OutsideList),
            Err(OriginError::Opaque) => Decision::Refuse(Reason::OpaqueOrigin),
            Err(_) => Decision::Refuse(Reason::Unreadable),
        }
    }
}

/// `url` when it is an `http(s)` address with a host, read as strictly as the lock reads — the
/// check to make before handing a URL that came from a page to the system browser, so a string
/// from the page cannot start a file or another program.
pub fn web_address(url: &str) -> Option<&str> {
    matches!(scheme_of(url).as_deref(), Some("http" | "https"))
        .then(|| Origin::of(url).ok())
        .flatten()
        .map(|_| url)
}

/// What a page in a locked window tried — handed to the caller of `lock_navigation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub uri: String,
    pub kind: Kind,
    /// What the lock says about [`uri`](Self::uri). For [`Kind::NewWindow`] the window is never
    /// created either way; this says whether the target is one of the window's own origins.
    pub decision: Decision,
    /// Whether the person started it (a click, a key) rather than a script or a timer — `None`
    /// where the platform does not say. **Open nothing outside the window on an attempt that is not
    /// `Some(true)`**: a page that can open the system browser at will can carry data out in the
    /// address of every tab it opens.
    pub user_initiated: Option<bool>,
}

/// Which way the page tried to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// The window itself, refused — a link, a script, a redirect, a form.
    Navigation,
    /// A frame inside the window, refused.
    Frame,
    /// A new window (`window.open`, `target="_blank"`) — never created; reported every time.
    NewWindow,
    /// A scheme handed to another program (`mailto:`, an app's protocol), refused.
    ExternalScheme,
    /// The window is showing a document the lock refuses although no navigation to it was let
    /// through — a navigation handler elsewhere undid the refusal, or the window was built on that
    /// address. The lock stopped the page's loading; what to show instead is the caller's.
    Breach,
}

/// What a lock could hold on this computer's web view runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enforced {
    /// Frame documents from refused origins are stopped before their request leaves. Without it
    /// (runtimes before the request-source filter) they are still cancelled, after the request.
    pub frame_requests: bool,
    /// Schemes handed to other programs are stopped. Without it (older runtimes) a page can still
    /// open `mailto:` or an app's protocol.
    pub external_schemes: bool,
}

/// Holds the web view of a window to `lock`, for as long as the web view lives. Call it once per web
/// view, on the thread the web view lives on — for a Tauri window, inside `with_webview`.
///
/// What it holds:
/// - **The window and its frames** — a navigation outside the lock is cancelled, redirects included.
/// - **The request** — the document request of a refused navigation gets an empty `403` before it
///   leaves. Cancelling a navigation alone is not enough: the runtime may already have sent the
///   request, and anything the page put in the address goes with it.
/// - **New windows** — none is ever created. Each request is reported, with whether the person
///   started it; opening it somewhere (a tab of the app's, the system browser) is the caller's.
/// - **External schemes** — refused unless named in [`NavigationLock::other_schemes`].
/// - **The document shown** — if the window ends up on a refused document anyway, loading stops and
///   [`Kind::Breach`] is reported.
///
/// `on_attempt` is told of every refusal and every new-window request, after the lock has acted. It
/// runs inside the web view's event, which waits for it: forward to an event or a channel, and never
/// open a dialog or anything modal from it. A panic in it is caught.
///
/// How to use it:
/// - **Lock before the content loads.** Build the window on an allowed address (or on
///   `about:blank` with `about` in [`NavigationLock::other_schemes`]), lock it, then navigate to the
///   content. A window built straight on the content runs its scripts before the lock is in place.
/// - **Leave new windows to the lock.** Do not also set a new-window handler on the window builder
///   (Tauri's `on_new_window`): one that allows a window overrides the lock.
/// - **Keep any other navigation handler in agreement.** A handler on the window builder (Tauri's
///   `on_navigation`) that allows what the lock refuses can undo the cancel, depending on order; give
///   it `lock.decide(url) == Decision::Allow`, or none. [`Kind::Breach`] reports it if it happens.
/// - **The app's own scheme** (a custom protocol the framework serves) must be on the list to be
///   shown; a refused one is still cancelled, but its request is answered by the framework, not
///   blocked.
#[cfg(windows)]
pub fn lock_navigation(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    lock: NavigationLock,
    on_attempt: impl Fn(Attempt) + 'static,
) -> windows_core::Result<Enforced> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_18, ICoreWebView2_2, ICoreWebView2_22,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
        COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT,
    };
    use webview2_com::{
        take_pwstr, CoTaskMemPWSTR, LaunchingExternalUriSchemeEventHandler,
        NavigationStartingEventHandler, NewWindowRequestedEventHandler, SourceChangedEventHandler,
        WebResourceRequestedEventHandler,
    };
    use windows_core::{Interface, BOOL, PWSTR};

    let lock = Rc::new(lock);
    let on_attempt: Rc<dyn Fn(Attempt)> = Rc::new(on_attempt);
    let report = move |attempt: Attempt| {
        let _ = catch_unwind(AssertUnwindSafe(|| on_attempt(attempt)));
    };
    let report = Rc::new(report);

    // SAFETY (every block below): COM calls on the web view the caller holds, on its thread; the
    // event arguments live for the duration of each callback, and strings the runtime hands over
    // are freed by `take_pwstr`. Handlers are held by COM reference and released with the web view.
    let core = unsafe { controller.CoreWebView2()? };
    let mut token = 0i64;

    // The window, and its frames: cancel, then report.
    let navigation = |kind: Kind| {
        let (lock, report) = (lock.clone(), report.clone());
        move |args: Option<
            webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2NavigationStartingEventArgs,
        >| {
            let Some(args) = args else { return Ok(()) };
            let mut uri = PWSTR::null();
            let mut by_person = BOOL::default();
            let uri = unsafe {
                args.Uri(&mut uri)?;
                args.IsUserInitiated(&mut by_person)?;
                take_pwstr(uri)
            };
            let decision = lock.decide(&uri);
            if decision != Decision::Allow {
                unsafe { args.SetCancel(true)? };
                report(Attempt {
                    uri,
                    kind,
                    decision,
                    user_initiated: Some(by_person.as_bool()),
                });
            }
            Ok(())
        }
    };
    let window = navigation(Kind::Navigation);
    let handler = NavigationStartingEventHandler::create(Box::new(move |_, args| window(args)));
    unsafe { core.add_NavigationStarting(&handler, &mut token)? };
    let frame = navigation(Kind::Frame);
    // The frame event takes the same handler type as the window's.
    let handler = NavigationStartingEventHandler::create(Box::new(move |_, args| frame(args)));
    unsafe { core.add_FrameNavigationStarting(&handler, &mut token)? };

    // The request: an empty 403 for a refused document, before it leaves. Reported from the
    // navigation events above, not here.
    let environment = unsafe { core.cast::<ICoreWebView2_2>()?.Environment()? };
    let every = CoTaskMemPWSTR::from("*");
    let frame_requests = match core.cast::<ICoreWebView2_22>() {
        Ok(core) => unsafe {
            core.AddWebResourceRequestedFilterWithRequestSourceKinds(
                *every.as_ref().as_pcwstr(),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT,
            )
            .is_ok()
        },
        Err(_) => false,
    };
    if !frame_requests {
        unsafe {
            core.AddWebResourceRequestedFilter(
                *every.as_ref().as_pcwstr(),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
            )?
        };
    }
    let requests = lock.clone();
    let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut uri = PWSTR::null();
        let uri = unsafe {
            args.Request()?.Uri(&mut uri)?;
            take_pwstr(uri)
        };
        if requests.decide(&uri) == Decision::Allow {
            return Ok(());
        }
        let (reason, headers) = (CoTaskMemPWSTR::from("Blocked"), CoTaskMemPWSTR::from(""));
        unsafe {
            let response = environment.CreateWebResourceResponse(
                None::<&windows::Win32::System::Com::IStream>,
                403,
                *reason.as_ref().as_pcwstr(),
                *headers.as_ref().as_pcwstr(),
            )?;
            args.SetResponse(&response)
        }
    }));
    unsafe { core.add_WebResourceRequested(&handler, &mut token)? };

    // New windows: never created, always reported.
    let (windows_lock, windows_report) = (lock.clone(), report.clone());
    let handler = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut uri = PWSTR::null();
        let mut by_person = BOOL::default();
        let uri = unsafe {
            args.SetHandled(true)?;
            args.Uri(&mut uri)?;
            args.IsUserInitiated(&mut by_person)?;
            take_pwstr(uri)
        };
        let decision = windows_lock.decide(&uri);
        windows_report(Attempt {
            uri,
            kind: Kind::NewWindow,
            decision,
            user_initiated: Some(by_person.as_bool()),
        });
        Ok(())
    }));
    unsafe { core.add_NewWindowRequested(&handler, &mut token)? };

    // Schemes handed to other programs.
    let external_schemes = match core.cast::<ICoreWebView2_18>() {
        Ok(core) => {
            let (lock, report) = (lock.clone(), report.clone());
            let handler =
                LaunchingExternalUriSchemeEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut uri = PWSTR::null();
                    let mut by_person = BOOL::default();
                    let uri = unsafe {
                        args.Uri(&mut uri)?;
                        args.IsUserInitiated(&mut by_person)?;
                        take_pwstr(uri)
                    };
                    let decision = lock.decide(&uri);
                    if decision != Decision::Allow {
                        unsafe { args.SetCancel(true)? };
                        report(Attempt {
                            uri,
                            kind: Kind::ExternalScheme,
                            decision,
                            user_initiated: Some(by_person.as_bool()),
                        });
                    }
                    Ok(())
                }));
            unsafe { core.add_LaunchingExternalUriScheme(&handler, &mut token)? };
            true
        }
        Err(_) => false,
    };

    // The document shown: whatever let it through, stop it and say so.
    let (shown_lock, shown_report) = (lock, report);
    let handler = SourceChangedEventHandler::create(Box::new(move |sender, _| {
        let Some(sender) = sender else { return Ok(()) };
        let mut uri = PWSTR::null();
        let uri = unsafe {
            sender.Source(&mut uri)?;
            take_pwstr(uri)
        };
        let decision = shown_lock.decide(&uri);
        if decision != Decision::Allow {
            unsafe { sender.Stop()? };
            shown_report(Attempt {
                uri,
                kind: Kind::Breach,
                decision,
                user_initiated: None,
            });
        }
        Ok(())
    }));
    unsafe { core.add_SourceChanged(&handler, &mut token)? };

    Ok(Enforced {
        frame_requests,
        external_schemes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(entries: &[&str]) -> NavigationLock {
        NavigationLock::new(entries.iter().map(|e| Origin::parse(e).unwrap()))
    }

    const REFUSED: Decision = Decision::Refuse(Reason::OutsideList);

    #[test]
    fn the_exact_host_is_allowed_and_a_longer_one_is_not() {
        let lock = lock(&["http://app.localhost"]);
        assert_eq!(
            lock.decide("http://app.localhost/notes?id=2#top"),
            Decision::Allow
        );
        assert_eq!(lock.decide("http://app.localhost.example.com/"), REFUSED);
        assert_eq!(lock.decide("http://evil-app.localhost/"), REFUSED);
        assert_eq!(
            lock.decide("https://app.localhost/"),
            REFUSED,
            "the scheme is part of the origin"
        );
    }

    #[test]
    fn default_ports_and_case_are_the_same_origin() {
        let lock = lock(&["https://Docs.Example:443/"]);
        assert_eq!(lock.decide("https://docs.example/a"), Decision::Allow);
        assert_eq!(lock.decide("HTTPS://DOCS.EXAMPLE:443/a"), Decision::Allow);
        assert_eq!(lock.decide("https://docs.example:8443/a"), REFUSED);
        let local = lock_with_port();
        assert_eq!(local.decide("http://127.0.0.1:4100/x"), Decision::Allow);
        assert_eq!(local.decide("http://127.0.0.1:4101/x"), REFUSED);
        assert_eq!(local.decide("http://127.0.0.1/x"), REFUSED);
    }

    fn lock_with_port() -> NavigationLock {
        lock(&["http://127.0.0.1:4100"])
    }

    #[test]
    fn user_info_and_backslashes_are_refused_not_read() {
        let lock = lock(&["https://allowed.example"]);
        assert_eq!(
            lock.decide("https://allowed.example@elsewhere.example/"),
            Decision::Refuse(Reason::Unreadable)
        );
        assert_eq!(
            lock.decide("https://elsewhere.example\\@allowed.example/"),
            Decision::Refuse(Reason::Unreadable)
        );
        assert_eq!(
            lock.decide("https://allowed.example /"),
            Decision::Refuse(Reason::Unreadable)
        );
        assert_eq!(
            lock.decide("not a url"),
            Decision::Refuse(Reason::Unreadable)
        );
        assert_eq!(
            lock.decide("https:allowed.example"),
            Decision::Refuse(Reason::Unreadable)
        );
    }

    #[test]
    fn an_entry_is_an_origin_and_never_narrowed_to_a_path() {
        assert_eq!(
            Origin::parse("https://cdn.example/app/"),
            Err(OriginError::NotAnOrigin)
        );
        assert_eq!(
            Origin::parse("https://cdn.example/?q"),
            Err(OriginError::NotAnOrigin)
        );
        assert_eq!(
            Origin::parse("https://bücher.example"),
            Err(OriginError::Unreadable)
        );
        assert_eq!(
            Origin::parse("https://xn--bcher-kva.example"),
            Ok(Origin::of("https://xn--bcher-kva.example/").unwrap())
        );
        assert_eq!(
            Origin::parse("mailto:someone@example.com"),
            Err(OriginError::NotADocument)
        );
        assert_eq!(
            Origin::parse("https://example:99999"),
            Err(OriginError::Unreadable)
        );
    }

    #[test]
    fn ipv6_hosts_keep_their_brackets() {
        let lock = lock(&["http://[::1]:8080"]);
        assert_eq!(lock.decide("http://[::1]:8080/a"), Decision::Allow);
        assert_eq!(lock.decide("http://[::1]:8081/a"), REFUSED);
        assert_eq!(Origin::parse("http://[::1"), Err(OriginError::Unreadable));
    }

    #[test]
    fn blobs_belong_to_the_origin_that_made_them() {
        let lock = lock(&["https://allowed.example", "file:///c:/notes/index.html"]);
        assert_eq!(
            lock.decide("blob:https://allowed.example/0f3c"),
            Decision::Allow
        );
        assert_eq!(lock.decide("blob:https://elsewhere.example/0f3c"), REFUSED);
        assert_eq!(
            lock.decide("blob:null/0f3c"),
            Decision::Refuse(Reason::OpaqueOrigin)
        );
        assert_eq!(
            lock.decide("blob:file:///0f3c"),
            Decision::Refuse(Reason::OpaqueOrigin)
        );
        assert_eq!(
            lock.decide("blob:blob:https://allowed.example/0f3c"),
            Decision::Refuse(Reason::Unreadable)
        );
        assert_eq!(
            lock.decide("filesystem:https://allowed.example/temporary/a"),
            Decision::Allow
        );
        assert_eq!(
            lock.decide("filesystem:https://elsewhere.example/temporary/a"),
            REFUSED
        );
        assert_eq!(
            lock.decide("data:text/html,<p>hi"),
            Decision::Refuse(Reason::OpaqueOrigin)
        );
    }

    #[test]
    fn a_file_entry_is_one_document() {
        let lock = lock(&["file:///C:/Notes/index.html"]);
        assert_eq!(
            lock.decide("file:///c:/notes/index.html?view=2#a"),
            Decision::Allow
        );
        assert_eq!(lock.decide("file:///c:/notes/other.html"), REFUSED);
        assert_eq!(
            lock.decide("file:///c:/notes/sub/../index.html"),
            Decision::Refuse(Reason::Unreadable)
        );
        assert_eq!(lock.decide("file://server/share/index.html"), REFUSED);
        assert_eq!(
            Origin::parse("file:///c:/notes/index.html#a"),
            Err(OriginError::NotAnOrigin)
        );
    }

    #[test]
    fn other_schemes_are_refused_unless_named() {
        let strict = lock(&["https://allowed.example"]);
        assert_eq!(
            strict.decide("about:blank"),
            Decision::Refuse(Reason::OtherScheme)
        );
        assert_eq!(
            strict.decide("mailto:team@example.com"),
            Decision::Refuse(Reason::OtherScheme)
        );
        assert_eq!(
            strict.decide("javascript:alert(1)"),
            Decision::Refuse(Reason::OtherScheme)
        );
        assert_eq!(
            strict.decide("ms-settings:"),
            Decision::Refuse(Reason::OtherScheme)
        );
        let named = strict.with_other_schemes(["About", "mailto"]);
        assert_eq!(named.decide("about:blank"), Decision::Allow);
        assert_eq!(named.decide("mailto:team@example.com"), Decision::Allow);
        assert_eq!(
            named.decide("ms-settings:"),
            Decision::Refuse(Reason::OtherScheme)
        );
    }

    #[test]
    fn naming_a_document_scheme_does_not_allow_its_documents() {
        let lock = lock(&["https://allowed.example"]).with_other_schemes([
            "file",
            "data",
            "https",
            "filesystem",
        ]);
        assert_eq!(lock.decide("file:///c:/secret.txt"), REFUSED);
        assert_eq!(
            lock.decide("data:text/html,x"),
            Decision::Refuse(Reason::OpaqueOrigin)
        );
        assert_eq!(lock.decide("https://elsewhere.example/"), REFUSED);
        assert_eq!(
            lock.decide("filesystem:https://elsewhere.example/temporary/a"),
            REFUSED
        );
    }

    #[test]
    fn web_address_takes_only_http_with_a_host() {
        assert_eq!(
            web_address("https://example.com/a?b"),
            Some("https://example.com/a?b")
        );
        assert_eq!(
            web_address("http://example.com"),
            Some("http://example.com")
        );
        assert_eq!(web_address("file:///c:/windows/system32/calc.exe"), None);
        assert_eq!(web_address("javascript:alert(1)"), None);
        assert_eq!(web_address("ms-msdt:/id"), None);
        assert_eq!(web_address(" https://example.com"), None);
        assert_eq!(web_address("https.exe"), None);
        assert_eq!(web_address("https://user@example.com"), None);
    }
}
