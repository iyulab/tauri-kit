//! Asks for the permission, shows one notice, and waits for a click on it.
//!
//! The system's notifications work only for a bundled app that lives where apps are installed — a bundle in a
//! temporary folder is refused without a question — so this example is run from a bundle in `~/Applications`
//! (macOS):
//!
//! ```text
//! cargo build -p tauri-kit-notification --example notify
//! mkdir -p Notify.app/Contents/MacOS
//! cp target/debug/examples/notify Notify.app/Contents/MacOS/notify
//! cat > Notify.app/Contents/Info.plist <<'PLIST'
//! <?xml version="1.0" encoding="UTF-8"?>
//! <plist version="1.0"><dict>
//!   <key>CFBundleIdentifier</key><string>example.notify</string>
//!   <key>CFBundleExecutable</key><string>notify</string>
//!   <key>CFBundleName</key><string>Notify</string>
//!   <key>CFBundlePackageType</key><string>APPL</string>
//! </dict></plist>
//! PLIST
//! codesign --force --sign - Notify.app
//! mkdir -p ~/Applications && cp -R Notify.app ~/Applications/
//! open -W --stdout "$PWD/out.txt" ~/Applications/Notify.app
//! ```
//!
//! Started from a terminal instead it prints `Unavailable` and ends.

use std::time::Duration;
use tauri_kit_notification::{
    on_activation, permission, request_permission, set_app_user_model_id, show, Notice,
};

/// The notices work through a running application, so on macOS the main thread runs one (an installed
/// Tauri app already does) and the calls are made from a worker.
fn work() {
    set_app_user_model_id("example.notify"); // Windows only; ignored elsewhere
    on_activation(|id| println!("activated: {id}"));
    println!("permission: {:?}", permission());
    println!("after asking: {:?}", request_permission());
    let notice = Notice {
        title: "Example".into(),
        body: "Click this notice.".into(),
        activation_id: Some("example-1".into()),
    };
    println!("show: {:?}", show(&notice));
    std::thread::sleep(Duration::from_secs(40));
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    std::thread::spawn(work);
    app.run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    work();
}
