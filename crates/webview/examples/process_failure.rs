//! Ends the web view's render process from outside, the way a crash or a security product does, and checks what
//! `failure` makes of it: `cargo run -p tauri-kit-webview --example process_failure`. Windows only; it needs the
//! WebView2 runtime and a desktop session (a window is shown for a moment).
//!
//! The run: load a page, subscribe with `failure::watch`, end every render process of that web view, wait for the
//! report (it must say the page is lost — `Process::page_lost`), call `failure::reload`, and wait until the page
//! loads again. Exit code 0 when all of that happened, 1 when a step did not, 2 when nothing was reported in 30 s.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::mpsc;
    use tauri_kit_webview::failure::{reload, watch, Failed};
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        CreateCoreWebView2Environment, ICoreWebView2Environment8,
        COREWEBVIEW2_PROCESS_KIND_RENDERER,
    };
    use webview2_com::{
        CreateCoreWebView2ControllerCompletedHandler,
        CreateCoreWebView2EnvironmentCompletedHandler, NavigationCompletedEventHandler,
    };
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, ShowWindow, CW_USEDEFAULT, SW_SHOW,
        WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };

    // A run that is never told anything must not hang the person who started it.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(30));
        eprintln!("nothing was reported within 30 s");
        std::process::exit(2);
    });

    // SAFETY: plain Win32 and COM set-up calls on this thread, which owns the window and pumps it.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let class: Vec<u16> = "process_failure_example\0".encode_utf16().collect();
        let instance = GetModuleHandleW(std::ptr::null());
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        });
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            640,
            480,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        ShowWindow(window, SW_SHOW);
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
        let for_controller = environment.clone();
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                for_controller
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
        controller.SetBounds(RECT {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        })?;
        let core = controller.CoreWebView2()?;

        // The page loads: this channel hears every completed navigation.
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let handler = NavigationCompletedEventHandler::create(Box::new(move |_, _| {
            let _ = loaded_tx.send(());
            Ok(())
        }));
        let mut token = 0i64;
        core.add_NavigationCompleted(&handler, &mut token)?;
        core.NavigateToString(&HSTRING::from(
            "<!doctype html><meta charset=\"utf-8\"><title>process_failure</title><h1>alive</h1>",
        ))?;
        webview2_com::wait_with_pump(loaded_rx)?;
        println!("page loaded");
        // `wait_with_pump` took the receiver; hear the next navigations on a fresh channel.
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let handler = NavigationCompletedEventHandler::create(Box::new(move |_, _| {
            let _ = loaded_tx.send(());
            Ok(())
        }));
        core.add_NavigationCompleted(&handler, &mut token)?;

        let (failed_tx, failed_rx) = mpsc::channel::<Failed>();
        watch(&controller, move |failed| {
            let _ = failed_tx.send(failed);
        })?;

        // End every render process of this web view, from outside.
        let infos = environment
            .cast::<ICoreWebView2Environment8>()?
            .GetProcessInfos()?;
        let mut count = 0u32;
        infos.Count(&mut count)?;
        let mut ended = 0;
        for index in 0..count {
            let info = infos.GetValueAtIndex(index)?;
            let mut kind = Default::default();
            info.Kind(&mut kind)?;
            if kind != COREWEBVIEW2_PROCESS_KIND_RENDERER {
                continue;
            }
            let mut pid = 0i32;
            info.ProcessId(&mut pid)?;
            let process = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
            if !process.is_null() {
                TerminateProcess(process, 1);
                CloseHandle(process);
                ended += 1;
            }
        }
        println!("ended {ended} render process(es)");
        if ended == 0 {
            return Err("no render process was found to end".into());
        }

        let failed = webview2_com::wait_with_pump(failed_rx)?;
        println!("reported: {failed:?}");
        if !failed.process.page_lost() {
            return Err(format!("expected the page to be reported lost, got {failed:?}").into());
        }

        reload(&controller)?;
        webview2_com::wait_with_pump(loaded_rx)?;
        println!("page loaded again after the reload");
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("web view process failures are reported by WebView2 (Windows) only");
}
