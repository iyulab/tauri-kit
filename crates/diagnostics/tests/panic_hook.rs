//! The hook is process-wide, so this file holds the one test that installs it.

use std::fs;
use std::sync::Arc;

use tauri_kit_diagnostics::{FrameRule, Layer, PanicHook, Reporter};

#[test]
fn a_panic_leaves_its_message_in_the_log_and_only_its_place_in_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("logs").join("shell-panic.log");
    let reports = dir.path().join("reports.jsonl");
    PanicHook::new()
        .log(&log)
        .report(
            Arc::new(Reporter::new(&reports)),
            Layer::new("shell", FrameRule::rust_source()),
            "1.2.3",
        )
        .install();

    let caught = std::thread::Builder::new()
        .name("worker".into())
        .spawn(|| panic!("could not open C:\\Users\\someone\\secret.txt"))
        .unwrap()
        .join();
    let _ = std::panic::take_hook(); // back to the default for whatever runs after

    assert!(caught.is_err());
    let text = fs::read_to_string(&log).unwrap();
    assert!(text.starts_with("=== panic "), "{text}");
    assert!(text.contains("thread 'worker'"), "{text}");
    assert!(
        text.contains("could not open C:\\Users\\someone\\secret.txt"),
        "the log keeps the message: {text}"
    );
    assert!(
        text.contains("tests/panic_hook.rs:"),
        "the log names the location: {text}"
    );

    let report = fs::read_to_string(&reports).unwrap();
    assert!(report.contains("\"Panic\""), "{report}");
    assert!(
        !report.contains("secret"),
        "a report never carries the message: {report}"
    );
}
