//! A page printed to a PDF file, with no dialog.
//!
//! An app that writes reports — a document the person saves, attaches or archives — needs the page it
//! already draws as a PDF file, without the print dialog and without a second browser engine. The web
//! view can do it: WebView2's `PrintToPdf` with print settings (page size, orientation, margins,
//! backgrounds). [`print_to_pdf`] drives it on a window the app made, either on the document the
//! window shows or on HTML the app hands over ([`Source::Html`] — put in with `NavigateToString`, not
//! a data URL, which ends the document at its first `#`).
//!
//! Before printing it waits for the page to be ready — loaded and its fonts in — by asking the page
//! (a document put in as a string is not seen by a framework's page-load event, and the window's
//! first, empty document already counts as loaded), with a limit the app sets in
//! [`PdfSettings::ready_timeout`]. The app picks no settle delay of its own.
//!
//! The window is the app's: a hidden window made for printing must be made with the same browser
//! arguments as the app's other windows (WebView2 refuses a second window of one profile with
//! different ones), and only the app knows those. Nothing else should navigate that window while it
//! prints.
//!
//! WebView2 only, for now. [`PdfSettings`] is written in terms a macOS web view's paginated printing
//! takes as well, so a macOS counterpart can take the same settings.

use std::fmt;
use std::time::Duration;

/// Which way up the page is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

/// Space left blank around the page's content, in inches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Margins {
    /// The same margin on every side.
    pub fn uniform(inches: f64) -> Self {
        Margins {
            top: inches,
            right: inches,
            bottom: inches,
            left: inches,
        }
    }
}

/// How the page is laid out on paper.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfSettings {
    /// The paper's width and height in inches, held upright; [`orientation`](Self::orientation)
    /// turns it.
    pub paper: (f64, f64),
    pub orientation: Orientation,
    pub margins: Margins,
    /// Whether background colours and images are printed. A report that draws with backgrounds (a
    /// shaded header, a coloured cell) wants them; the web view's own default leaves them out.
    pub print_backgrounds: bool,
    /// How long to wait for the page to be ready before giving up with [`PdfError::NotReady`].
    pub ready_timeout: Duration,
}

impl PdfSettings {
    /// ISO A4 (210 × 297 mm), portrait, 0.4 in margins, backgrounds printed, 30 s to be ready.
    pub fn a4() -> Self {
        PdfSettings {
            paper: (8.27, 11.69),
            ..Self::letter()
        }
    }

    /// US Letter (8.5 × 11 in), portrait, 0.4 in margins, backgrounds printed, 30 s to be ready.
    pub fn letter() -> Self {
        PdfSettings {
            paper: (8.5, 11.0),
            orientation: Orientation::Portrait,
            margins: Margins::uniform(0.4),
            print_backgrounds: true,
            ready_timeout: Duration::from_secs(30),
        }
    }

    /// The paper's width and height as printed — turned for [`Orientation::Landscape`].
    pub fn page(&self) -> (f64, f64) {
        match self.orientation {
            Orientation::Portrait => self.paper,
            Orientation::Landscape => (self.paper.1, self.paper.0),
        }
    }

    /// Refuses settings that leave no room to print: a paper size that is not a positive number of
    /// inches, a margin that is negative or not a number, margins that meet across the page, and a
    /// ready timeout of zero.
    pub fn check(&self) -> Result<(), InvalidSettings> {
        let (width, height) = self.page();
        if !(width.is_finite() && width > 0.0 && height.is_finite() && height > 0.0) {
            return Err(InvalidSettings::Paper);
        }
        let Margins {
            top,
            right,
            bottom,
            left,
        } = self.margins;
        if [top, right, bottom, left]
            .iter()
            .any(|m| !(m.is_finite() && *m >= 0.0))
        {
            return Err(InvalidSettings::Margin);
        }
        if left + right >= width || top + bottom >= height {
            return Err(InvalidSettings::NoRoom);
        }
        if self.ready_timeout.is_zero() {
            return Err(InvalidSettings::ReadyTimeout);
        }
        Ok(())
    }
}

/// What [`PdfSettings::check`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidSettings {
    /// The paper's width or height is not a positive number of inches.
    Paper,
    /// A margin is negative or not a number.
    Margin,
    /// The margins meet or cross: nothing is left to print on.
    NoRoom,
    /// The ready timeout is zero.
    ReadyTimeout,
}

impl fmt::Display for InvalidSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            InvalidSettings::Paper => "the paper size must be a positive number of inches",
            InvalidSettings::Margin => "a margin must be zero or more inches",
            InvalidSettings::NoRoom => "the margins leave no room on the page",
            InvalidSettings::ReadyTimeout => "the ready timeout must be longer than zero",
        })
    }
}

impl std::error::Error for InvalidSettings {}

/// What to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The document the window shows now.
    Current,
    /// This HTML, put in the window as its document first. WebView2 takes at most 2 MB of it —
    /// [`PdfError::TooLarge`] beyond; put a larger report in a file and navigate to it instead.
    Html(String),
}

/// Why no PDF was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfError {
    /// The settings leave no room to print (see [`PdfSettings::check`]).
    Settings(InvalidSettings),
    /// The HTML is larger than WebView2 takes as a string (2 MB).
    TooLarge,
    /// The HTML did not load (the web view's error status, as a number).
    NotLoaded(i32),
    /// The page was not ready — loaded, with its fonts in — within the ready timeout.
    NotReady,
    /// The web view finished without writing the file.
    NotWritten,
    /// The web view refused or failed a call: a runtime too old for printing to PDF, a window
    /// already closed, a path it cannot write.
    WebView(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::Settings(why) => write!(f, "{why}"),
            PdfError::TooLarge => f.write_str("the HTML is larger than the web view takes (2 MB)"),
            PdfError::NotLoaded(status) => {
                write!(f, "the HTML did not load (web error status {status})")
            }
            PdfError::NotReady => f.write_str("the page was not ready within the ready timeout"),
            PdfError::NotWritten => f.write_str("the web view did not write the PDF"),
            PdfError::WebView(message) => write!(f, "the web view failed: {message}"),
        }
    }
}

impl std::error::Error for PdfError {}

/// WebView2's limit on HTML put in as a string, in bytes of its UTF-16 form.
#[cfg(any(windows, test))]
const HTML_LIMIT: usize = 2 * 1024 * 1024;

#[cfg(any(windows, test))]
fn html_fits(html: &str) -> bool {
    html.encode_utf16().count() * 2 <= HTML_LIMIT
}

/// The script that answers `"ready"` once the page has loaded and its fonts are in, or `"timeout"`
/// after `timeout`. Run with the DevTools protocol's `Runtime.evaluate` and `awaitPromise`, which
/// waits for the promise — a plain script run hands back a promise as an empty object.
#[cfg(any(windows, test))]
fn ready_script(timeout: Duration) -> String {
    let ms = timeout.as_millis().min(u32::MAX as u128);
    format!(
        "Promise.race([\
         new Promise(r => setTimeout(() => r('timeout'), {ms})),\
         (async () => {{\
         if (document.readyState !== 'complete') await new Promise(r => addEventListener('load', r, {{ once: true }}));\
         await document.fonts.ready;\
         document.documentElement.getBoundingClientRect();\
         return 'ready';\
         }})()\
         ])"
    )
}

/// The `Runtime.evaluate` parameters for `script`, as JSON.
#[cfg(any(windows, test))]
fn evaluate_params(script: &str) -> String {
    let mut json = String::from("{\"expression\":\"");
    for c in script.chars() {
        match c {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            c if (c as u32) < 0x20 => json.push_str(&format!("\\u{:04x}", c as u32)),
            c => json.push(c),
        }
    }
    json.push_str("\",\"awaitPromise\":true,\"returnByValue\":true}");
    json
}

/// Whether a `Runtime.evaluate` answer says the page is ready.
#[cfg(any(windows, test))]
fn says_ready(answer: &str) -> bool {
    let compact: String = answer.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains("\"value\":\"ready\"") && !compact.contains("\"exceptionDetails\"")
}

/// Prints `source` from the web view `controller` to the PDF file `out`, laid out by `settings`, and
/// calls `on_done` once with the outcome. Call it on the web view's thread (inside the framework's
/// `with_webview`); it starts the work and returns — the web view finishes it on that thread, so
/// nothing here waits.
///
/// An error returned here was found before anything started, and `on_done` is not called. Once this
/// returns `Ok`, `on_done` is called exactly once.
#[cfg(windows)]
pub fn print_to_pdf(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    source: Source,
    settings: &PdfSettings,
    out: &std::path::Path,
    on_done: impl FnOnce(Result<(), PdfError>) + 'static,
) -> Result<(), PdfError> {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2Environment6, ICoreWebView2PrintSettings, ICoreWebView2_2,
        ICoreWebView2_7, COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE,
        COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT, COREWEBVIEW2_WEB_ERROR_STATUS,
    };
    use webview2_com::{
        CallDevToolsProtocolMethodCompletedHandler, NavigationCompletedEventHandler,
        PrintToPdfCompletedHandler,
    };
    use windows_core::{Interface, BOOL, HSTRING};

    settings.check().map_err(PdfError::Settings)?;
    if let Source::Html(html) = &source {
        if !html_fits(html) {
            return Err(PdfError::TooLarge);
        }
    }
    let failed = |e: windows_core::Error| PdfError::WebView(e.message());

    // Everything that can be refused before starting is asked now, so a runtime too old for
    // printing to PDF is an error here rather than a callback.
    // SAFETY: COM calls on the web view the caller holds, on its thread.
    let (core, core7, print_settings) = unsafe {
        let core: ICoreWebView2 = controller.CoreWebView2().map_err(failed)?;
        let core7: ICoreWebView2_7 = core.cast().map_err(failed)?;
        let environment: ICoreWebView2Environment6 = core
            .cast::<ICoreWebView2_2>()
            .and_then(|core| core.Environment())
            .and_then(|environment| environment.cast())
            .map_err(failed)?;
        let print: ICoreWebView2PrintSettings =
            environment.CreatePrintSettings().map_err(failed)?;
        let (width, height) = settings.paper;
        let Margins {
            top,
            right,
            bottom,
            left,
        } = settings.margins;
        print
            .SetOrientation(match settings.orientation {
                Orientation::Portrait => COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT,
                Orientation::Landscape => COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE,
            })
            .and_then(|()| print.SetPageWidth(width))
            .and_then(|()| print.SetPageHeight(height))
            .and_then(|()| print.SetMarginTop(top))
            .and_then(|()| print.SetMarginRight(right))
            .and_then(|()| print.SetMarginBottom(bottom))
            .and_then(|()| print.SetMarginLeft(left))
            .and_then(|()| print.SetShouldPrintBackgrounds(settings.print_backgrounds))
            .map_err(failed)?;
        (core, core7, print)
    };

    // `on_done` travels through closures the web view may call more than once; this takes it once.
    type Done = Rc<RefCell<Option<Box<dyn FnOnce(Result<(), PdfError>)>>>>;
    let done: Done = Rc::new(RefCell::new(Some(Box::new(on_done))));
    let finish = |done: &Done, outcome: Result<(), PdfError>| {
        if let Some(on_done) = done.borrow_mut().take() {
            on_done(outcome);
        }
    };

    let path = HSTRING::from(out.as_os_str());
    let print = {
        let done = done.clone();
        move || {
            let handler = {
                let done = done.clone();
                PrintToPdfCompletedHandler::create(Box::new(move |status, written| {
                    finish(
                        &done,
                        match status {
                            Ok(()) if written => Ok(()),
                            Ok(()) => Err(PdfError::NotWritten),
                            Err(e) => Err(PdfError::WebView(e.message())),
                        },
                    );
                    Ok(())
                }))
            };
            // SAFETY: on the web view's thread, from its completion callback.
            if let Err(e) = unsafe { core7.PrintToPdf(&path, &print_settings, &handler) } {
                finish(&done, Err(PdfError::WebView(e.message())));
            }
        }
    };

    let when_ready = {
        let done = done.clone();
        let core = core.clone();
        let params = HSTRING::from(evaluate_params(&ready_script(settings.ready_timeout)));
        move || {
            let handler = {
                let done = done.clone();
                CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                    move |status, answer| {
                        match status {
                            Ok(()) if says_ready(&answer) => print(),
                            Ok(()) => finish(&done, Err(PdfError::NotReady)),
                            Err(e) => finish(&done, Err(PdfError::WebView(e.message()))),
                        }
                        Ok(())
                    },
                ))
            };
            // SAFETY: on the web view's thread.
            if let Err(e) = unsafe {
                core.CallDevToolsProtocolMethod(
                    &HSTRING::from("Runtime.evaluate"),
                    &params,
                    &handler,
                )
            } {
                finish(&done, Err(PdfError::WebView(e.message())));
            }
        }
    };

    match source {
        Source::Current => when_ready(),
        Source::Html(html) => {
            // The handler removes itself on the first completion: the window is the app's, and may
            // go on to show other documents.
            let token = Rc::new(Cell::new(0i64));
            let mut when_ready = Some(when_ready);
            let handler = {
                let done = done.clone();
                let token = token.clone();
                NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
                    let Some(when_ready) = when_ready.take() else {
                        return Ok(());
                    };
                    if let Some(sender) = sender {
                        // SAFETY: on the web view's thread, from its own event.
                        let _ = unsafe { sender.remove_NavigationCompleted(token.get()) };
                    }
                    let mut success = BOOL(0);
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS(0);
                    if let Some(args) = &args {
                        // SAFETY: the event's arguments, during the event.
                        unsafe {
                            let _ = args.IsSuccess(&mut success);
                            let _ = args.WebErrorStatus(&mut status);
                        }
                    }
                    if success.as_bool() {
                        when_ready();
                    } else {
                        finish(&done, Err(PdfError::NotLoaded(status.0)));
                    }
                    Ok(())
                }))
            };
            let mut registered = 0i64;
            // SAFETY: COM calls on the web view the caller holds, on its thread.
            unsafe {
                core.add_NavigationCompleted(&handler, &mut registered)
                    .map_err(failed)?;
                token.set(registered);
                if let Err(e) = core.NavigateToString(&HSTRING::from(html)) {
                    let _ = core.remove_NavigationCompleted(registered);
                    return Err(failed(e));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a4_and_letter_are_the_papers_they_name() {
        assert_eq!(PdfSettings::a4().paper, (8.27, 11.69));
        assert_eq!(PdfSettings::letter().paper, (8.5, 11.0));
        assert_eq!(PdfSettings::a4().check(), Ok(()));
    }

    #[test]
    fn landscape_turns_the_page() {
        let mut settings = PdfSettings::a4();
        settings.orientation = Orientation::Landscape;
        assert_eq!(settings.page(), (11.69, 8.27));
    }

    #[test]
    fn settings_that_leave_no_room_are_refused() {
        let with = |f: fn(&mut PdfSettings)| {
            let mut settings = PdfSettings::letter();
            f(&mut settings);
            settings.check()
        };
        assert_eq!(with(|s| s.paper = (0.0, 11.0)), Err(InvalidSettings::Paper));
        assert_eq!(
            with(|s| s.paper = (f64::NAN, 11.0)),
            Err(InvalidSettings::Paper)
        );
        assert_eq!(
            with(|s| s.margins.left = -0.1),
            Err(InvalidSettings::Margin)
        );
        assert_eq!(
            with(|s| s.margins.top = f64::INFINITY),
            Err(InvalidSettings::Margin)
        );
        assert_eq!(
            with(|s| s.margins = Margins::uniform(4.25)),
            Err(InvalidSettings::NoRoom)
        );
        assert_eq!(
            with(|s| s.ready_timeout = Duration::ZERO),
            Err(InvalidSettings::ReadyTimeout)
        );
        // Zero margins print to the edge; margins just short of meeting still leave room.
        assert_eq!(with(|s| s.margins = Margins::uniform(0.0)), Ok(()));
        assert_eq!(with(|s| s.margins = Margins::uniform(4.2)), Ok(()));
    }

    #[test]
    fn margins_are_checked_against_the_turned_page() {
        // 5 in left and right fit across Letter's 11 in when landscape, not across its 8.5 in upright.
        let mut settings = PdfSettings::letter();
        settings.margins = Margins {
            top: 0.5,
            right: 5.0,
            bottom: 0.5,
            left: 5.0,
        };
        assert_eq!(settings.check(), Err(InvalidSettings::NoRoom));
        settings.orientation = Orientation::Landscape;
        settings.margins.right = 5.0;
        settings.margins.left = 5.4;
        assert_eq!(settings.check(), Ok(()));
    }

    #[test]
    fn html_beyond_two_megabytes_of_utf16_does_not_fit() {
        assert!(html_fits(&"a".repeat(HTML_LIMIT / 2)));
        assert!(!html_fits(&"a".repeat(HTML_LIMIT / 2 + 1)));
        // Measured in UTF-16: a character outside the basic plane takes four bytes.
        assert!(!html_fits(&"\u{1F600}".repeat(HTML_LIMIT / 4 + 1)));
    }

    #[test]
    fn the_ready_script_waits_for_load_and_fonts_and_gives_up_on_time() {
        let script = ready_script(Duration::from_millis(1500));
        assert!(script.contains("setTimeout(() => r('timeout'), 1500)"));
        assert!(script.contains("document.fonts.ready"));
        assert!(script.contains("addEventListener('load'"));
        assert!(script.contains("return 'ready'"));
    }

    #[test]
    fn evaluate_params_are_json_with_the_script_escaped() {
        let params = evaluate_params("a \"b\" \\ c\nd\u{1}");
        assert_eq!(
            params,
            "{\"expression\":\"a \\\"b\\\" \\\\ c\\nd\\u0001\",\"awaitPromise\":true,\"returnByValue\":true}"
        );
    }

    #[test]
    fn only_a_ready_value_without_an_exception_says_ready() {
        assert!(says_ready(
            r#"{"result":{"type":"string","value":"ready"}}"#
        ));
        assert!(says_ready(
            "{ \"result\": { \"type\": \"string\", \"value\": \"ready\" } }"
        ));
        assert!(!says_ready(
            r#"{"result":{"type":"string","value":"timeout"}}"#
        ));
        assert!(!says_ready(
            r#"{"result":{"type":"object"},"exceptionDetails":{"text":"value: \"ready\""}}"#
        ));
    }
}
