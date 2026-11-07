//! The route as an app sees it: without answers, the app's own dialog; in a test run, the file's.

use tauri_kit_dialog::{answer_or, Answer, Ask, Kind};

#[test]
fn the_route_answers_from_the_file_only_in_a_test_run_of_a_test_build() {
    let ask = Ask {
        kind: Kind::Save,
        title: "Save",
        default_path: None,
        filters: &[],
    };
    let own = || Answer::Paths(vec!["/own".into()]);

    // One test touches the environment, so it is not raced by another.
    std::env::remove_var(tauri_kit_dialog::ANSWERS);
    assert_eq!(answer_or(&ask, own), Ok(Answer::Paths(vec!["/own".into()])));

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("answers.jsonl");
    std::fs::write(&file, "{\"kind\":\"cancel\"}\n").unwrap();
    std::env::set_var(tauri_kit_dialog::ANSWERS, &file);
    let answered = answer_or(&ask, own);
    std::env::remove_var(tauri_kit_dialog::ANSWERS);
    if cfg!(feature = "test-answers") {
        assert_eq!(answered, Ok(Answer::Cancelled));
    } else {
        assert_eq!(answered, Ok(Answer::Paths(vec!["/own".into()])));
    }
}
