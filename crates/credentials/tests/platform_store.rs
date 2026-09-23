//! Against the real platform store. Ignored by default: CI runners do not all have a usable
//! credential store (Linux needs a running Secret Service). Run on a desktop with
//! `cargo test -p tauri-kit-credentials -- --ignored`. It only ever touches a `.test` entry.

use tauri_kit_credentials::{build_kind, Credentials};

#[test]
#[ignore = "touches the real OS credential store"]
fn round_trip_through_the_platform_store() {
    let credentials = Credentials::new("tauri-kit.credentials.platform-check", build_kind!());
    assert!(credentials.service().ends_with(".test"));

    credentials.delete("probe").unwrap();
    credentials.set("probe", "value").unwrap();
    assert_eq!(credentials.get("probe").unwrap().as_deref(), Some("value"));
    credentials.delete("probe").unwrap();
    assert_eq!(credentials.get("probe").unwrap(), None);
}
