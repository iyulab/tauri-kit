//! A test binary is not a bundled app, so the system's notifications cannot be used from it: every entry
//! says so and none raises, asks or shows anything. (On macOS the framework raises an exception when it
//! is called without a bundle; reaching these assertions at all is part of what is checked.)

use tauri_kit_notification::{permission, request_permission, show, Error, Notice, Permission};

#[test]
fn a_run_without_a_bundle_reports_unavailable_everywhere() {
    assert_eq!(permission(), Permission::Unavailable);
    assert_eq!(request_permission(), Permission::Unavailable);
    let notice = Notice {
        title: "t".into(),
        body: "b".into(),
        activation_id: Some("id".into()),
    };
    assert_eq!(show(&notice), Err(Error::Unavailable));
}
