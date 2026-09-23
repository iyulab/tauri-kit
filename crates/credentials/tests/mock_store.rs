//! Behaviour against keyring-core's in-memory store, so it runs the same on every platform and
//! never touches a real credential store.

use std::sync::Once;
use tauri_kit_credentials::{build_kind, BuildKind, Credentials};

fn use_mock_store() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap()));
}

#[test]
fn an_integration_test_is_a_test_build_even_though_the_library_is_not() {
    // The library under test is compiled without `cfg(test)` here; the macro is expanded in this
    // file, which is. This is why the kind of build is a macro and not something the library asks.
    assert_eq!(build_kind!(), BuildKind::Test);
}

#[test]
fn set_get_delete_round_trip() {
    use_mock_store();
    let credentials = Credentials::new("com.example.roundtrip", build_kind!());
    assert_eq!(credentials.get("api-key").unwrap(), None);
    assert!(!credentials.contains("api-key").unwrap());

    credentials.set("api-key", "first").unwrap();
    credentials.set("api-key", "second").unwrap();
    assert_eq!(
        credentials.get("api-key").unwrap().as_deref(),
        Some("second")
    );
    assert!(credentials.contains("api-key").unwrap());

    credentials.delete("api-key").unwrap();
    assert_eq!(credentials.get("api-key").unwrap(), None);
}

#[test]
fn deleting_what_is_not_there_is_not_an_error() {
    use_mock_store();
    let credentials = Credentials::new("com.example.absent", build_kind!());
    credentials.delete("never-set").unwrap();
    credentials.delete("never-set").unwrap();
}

#[test]
fn a_test_run_does_not_reach_the_installed_apps_secret() {
    use_mock_store();
    let installed = Credentials::new("com.example.isolation", BuildKind::Release);
    installed.set("token", "the user's real token").unwrap();

    let under_test = Credentials::new("com.example.isolation", build_kind!());
    assert_eq!(under_test.get("token").unwrap(), None);
    under_test.set("token", "a test literal").unwrap();
    under_test.delete("token").unwrap();

    assert_eq!(
        installed.get("token").unwrap().as_deref(),
        Some("the user's real token")
    );
}

#[test]
fn accounts_under_one_service_are_separate() {
    use_mock_store();
    let credentials = Credentials::new("com.example.accounts", build_kind!());
    credentials.set("a", "one").unwrap();
    credentials.set("b", "two").unwrap();
    credentials.delete("a").unwrap();
    assert_eq!(credentials.get("b").unwrap().as_deref(), Some("two"));
}
