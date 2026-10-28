//! Prints a small report to a PDF file through a hidden WebView2 window, the way an app prints from
//! a window it made for printing: `cargo run -p tauri-kit-webview --example print_to_pdf [<out.pdf>]`.
//! Windows only; it needs the WebView2 runtime and a desktop session.
//!
//! With `--close-early` the window is closed right after the print starts, and the run checks that
//! the outcome is `PdfError::Abandoned`, once: `... --example print_to_pdf -- --close-early`.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::mpsc;
    use tauri_kit_webview::pdf::{print_to_pdf, PdfSettings, Source};
    use webview2_com::{
        CreateCoreWebView2ControllerCompletedHandler,
        CreateCoreWebView2EnvironmentCompletedHandler,
        Microsoft::Web::WebView2::Win32::CreateCoreWebView2Environment,
    };
    use windows::Win32::Foundation::{E_POINTER, HWND};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, CW_USEDEFAULT, WNDCLASSW,
        WS_OVERLAPPEDWINDOW,
    };

    let close_early = std::env::args_os().any(|a| a == "--close-early");
    let out = std::env::args_os()
        .skip(1)
        .find(|a| a != "--close-early")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("print_to_pdf-example.pdf"));
    let _ = std::fs::remove_file(&out);

    // A report with what printing has to survive: a `#` colour (a data URL would end the document
    // there), a background, text outside ASCII, and enough rows for a second page.
    let rows: String = (1..=60)
        .map(|i| format!("<tr><td>{i}</td><td>Item {i} — Ünïcödé</td><td style=\"background:#e8f0fe\">{}</td></tr>", i * 7))
        .collect();
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><style>body{{font-family:sans-serif;color:#1a1a1a}}\
         h1{{background:#204080;color:#fff;padding:8px}}td{{border:1px solid #ccc;padding:4px}}</style>\
         <h1>Report</h1><table>{rows}</table>"
    );

    // SAFETY: plain Win32 and COM set-up calls on this thread, which owns the window and pumps it.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let class: Vec<u16> = "print_to_pdf_example\0".encode_utf16().collect();
        let instance = GetModuleHandleW(std::ptr::null());
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        });
        // Never shown: the report window is hidden, as an app's printing window is.
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            794,
            1123,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        let window = HWND(window);

        let (tx, rx) = mpsc::channel();
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(|handler| {
                CreateCoreWebView2Environment(&handler).map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |status, environment| {
                status?;
                let _ = tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        )?;
        let environment = rx.recv()??;

        let (tx, rx) = mpsc::channel();
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                environment
                    .CreateCoreWebView2Controller(window, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |status, controller| {
                status?;
                let _ = tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        )?;
        let controller = rx.recv()??;

        let started = std::time::Instant::now();
        let (tx, rx) = mpsc::channel();
        print_to_pdf(
            &controller,
            Source::Html(html),
            &PdfSettings::a4(),
            &out,
            move |outcome| {
                let _ = tx.send(outcome);
            },
        )?;
        if close_early {
            // The window an app closes while its report prints: the web view lets go of the work.
            controller.Close()?;
            let outcome = webview2_com::wait_with_pump(rx)?;
            // The callback is an `FnOnce`: whatever it reports is the one outcome.
            return match outcome {
                Err(tauri_kit_webview::pdf::PdfError::Abandoned) => {
                    println!("closed early: Abandoned, once");
                    Ok(())
                }
                other => Err(format!("closed early: expected Abandoned, got {other:?}").into()),
            };
        }
        webview2_com::wait_with_pump(rx)??;

        let bytes = std::fs::read(&out)?;
        if !bytes.starts_with(b"%PDF-") {
            return Err(format!("{} is not a PDF", out.display()).into());
        }
        println!(
            "wrote {} ({} bytes) in {} ms",
            out.display(),
            bytes.len(),
            started.elapsed().as_millis()
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("printing to PDF is implemented for WebView2 (Windows) only");
}
