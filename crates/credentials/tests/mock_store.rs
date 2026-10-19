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

// Long secrets (see the crate docs): kept in parts where one entry is too small. The in-memory store
// has no limit, so these lower it to what a few characters fill.

#[test]
fn a_secret_that_fits_stays_one_entry() {
    use_mock_store();
    let credentials = Credentials::new("com.example.parts.fits", build_kind!()).with_part_units(40);
    credentials.set("token", "short").unwrap();
    let raw = keyring_core::Entry::new(credentials.service(), "token").unwrap();
    assert_eq!(raw.get_password().unwrap(), "short");
    assert!(matches!(
        keyring_core::Entry::new(credentials.service(), "token/1")
            .unwrap()
            .get_password(),
        Err(keyring_core::Error::NoEntry)
    ));
}

#[test]
fn a_long_secret_is_kept_in_parts_and_read_whole() {
    use_mock_store();
    let credentials = Credentials::new("com.example.parts.long", build_kind!()).with_part_units(40);
    // Non-BMP characters (two UTF-16 units each) must not be cut in half.
    let secret: String = "eyJhbGciOiJSUzI1NiJ9.😀payload😀.signature—비밀".repeat(5);
    credentials.set("session", &secret).unwrap();
    assert_eq!(
        credentials.get("session").unwrap().as_deref(),
        Some(secret.as_str())
    );

    let head = keyring_core::Entry::new(credentials.service(), "session")
        .unwrap()
        .get_password()
        .unwrap();
    assert!(head.starts_with("\0tauri-kit-credentials/parts="));
    for i in 1.. {
        match keyring_core::Entry::new(credentials.service(), &format!("session/{i}"))
            .unwrap()
            .get_password()
        {
            Ok(part) => assert!(part.encode_utf16().count() <= 40),
            Err(keyring_core::Error::NoEntry) => break,
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_shorter_secret_or_a_delete_leaves_no_parts_behind() {
    use_mock_store();
    let credentials =
        Credentials::new("com.example.parts.shrink", build_kind!()).with_part_units(40);
    let part = |i: usize| {
        keyring_core::Entry::new(credentials.service(), &format!("s/{i}"))
            .unwrap()
            .get_password()
    };
    credentials.set("s", &"x".repeat(200)).unwrap();
    assert!(part(4).is_ok());
    credentials.set("s", &"y".repeat(45)).unwrap();
    assert_eq!(
        credentials.get("s").unwrap().as_deref(),
        Some("y".repeat(45).as_str())
    );
    assert!(part(1).is_ok());
    assert!(matches!(part(2), Err(keyring_core::Error::NoEntry)));
    credentials.set("s", "z").unwrap();
    assert!(matches!(part(1), Err(keyring_core::Error::NoEntry)));
    credentials.set("s", &"w".repeat(200)).unwrap();
    credentials.delete("s").unwrap();
    assert_eq!(credentials.get("s").unwrap(), None);
    assert!(matches!(part(1), Err(keyring_core::Error::NoEntry)));
}

#[test]
fn a_missing_part_is_an_error_not_a_cut_secret() {
    use_mock_store();
    let credentials =
        Credentials::new("com.example.parts.missing", build_kind!()).with_part_units(40);
    credentials.set("s", &"x".repeat(200)).unwrap();
    keyring_core::Entry::new(credentials.service(), "s/2")
        .unwrap()
        .delete_credential()
        .unwrap();
    assert!(credentials.get("s").is_err());
    credentials.delete("s").unwrap(); // and it can still be cleared
    assert_eq!(credentials.get("s").unwrap(), None);
}

#[test]
fn a_secret_may_not_begin_with_nul() {
    use_mock_store();
    let credentials = Credentials::new("com.example.parts.nul", build_kind!());
    assert!(credentials.set("s", "\0x").is_err());
    assert_eq!(credentials.get("s").unwrap(), None);
}

#[test]
fn parts_are_laid_out_the_same_in_both_languages() {
    // TauriKit.Credentials' `A_long_secret_is_laid_out_in_parts_exactly_as_the_crate_lays_it_out`
    // pins the same strings against the real store.
    use_mock_store();
    let credentials =
        Credentials::new("com.example.parts.layout", build_kind!()).with_part_units(40);
    credentials.set("s", &"x".repeat(100)).unwrap();
    let raw = |a: &str| {
        keyring_core::Entry::new(credentials.service(), a)
            .unwrap()
            .get_password()
            .unwrap()
    };
    assert_eq!(
        raw("s"),
        format!("\0tauri-kit-credentials/parts=4\0{}", "x".repeat(8))
    );
    assert_eq!(raw("s/1"), "x".repeat(40));
    assert_eq!(raw("s/3"), "x".repeat(12));
}
