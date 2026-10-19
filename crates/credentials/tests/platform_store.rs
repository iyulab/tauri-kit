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

#[test]
#[ignore = "touches the real OS credential store"]
fn a_session_longer_than_one_windows_entry_round_trips() {
    // About what a signed-in OIDC session takes (two JWTs and a refresh token): past 1,280 UTF-16 units.
    let credentials = Credentials::new("tauri-kit.credentials.platform-check", build_kind!());
    let secret = "eyJhbGciOiJSUzI1NiIsImtpZCI6IjEifQ.".repeat(120);
    credentials.delete("long").unwrap();
    credentials.set("long", &secret).unwrap();
    assert_eq!(
        credentials.get("long").unwrap().as_deref(),
        Some(secret.as_str())
    );
    credentials.delete("long").unwrap();
    assert_eq!(credentials.get("long").unwrap(), None);
}
