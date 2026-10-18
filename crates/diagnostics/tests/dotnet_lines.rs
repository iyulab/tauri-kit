//! The .NET side (`TauriKit.Diagnostics` in `dotnet/`) writes events in this crate's shape. Its test writes
//! the scene below and keeps the result, with the random id, the time and the platform made fixed, in
//! `cases/dotnet-events.jsonl`; this test reads that file through the same registry, and writes the same
//! scene here to check both languages write the same lines.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri_kit_diagnostics::{
    pending_events, Events, InstallId, Recorded, Registry, Session, Value, Why,
};

fn cases() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cases")
}

fn registry() -> Registry {
    Registry::from_json(&std::fs::read_to_string(cases().join("registry.json")).unwrap()).unwrap()
}

fn normalized(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .map(|line| {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["session"] = "00000000000000000000000000000000".into();
            v["time"] = "2026-01-01T00:00:00Z".into();
            v["os"] = "windows".into();
            v["arch"] = "x86_64".into();
            v
        })
        .collect()
}

#[test]
fn every_line_the_dotnet_side_writes_verifies_against_the_same_registry() {
    let dir = tempfile::tempdir().unwrap();
    let read = pending_events(
        &registry(),
        &cases().join("dotnet-events.jsonl"),
        &dir.path().join("sent"),
    )
    .unwrap();
    let names: Vec<_> = read
        .iter()
        .flat_map(|p| p.items.iter().map(|e| e.name().to_string()))
        .collect();
    assert_eq!(
        names,
        [
            "export.finished",
            "search.used",
            "kit.events.dropped",
            "kit.events.dropped",
            "app.opened",
            "kit.events.dropped"
        ],
        "no line of the .NET file was passed over"
    );
}

#[test]
fn both_languages_write_the_same_lines_for_the_same_scene() {
    let dir = tempfile::tempdir().unwrap();
    let install = InstallId::load_or_create(&dir.path().join("id")).unwrap();
    let session = Session::builder(&install, "1.2.3")
        .start(dir.path().join("s.jsonl"), dir.path().join("s.running"))
        .unwrap();
    let registry = registry();
    let events = Events::open(&session, registry.clone(), dir.path().join("events.jsonl"))
        .unwrap()
        .max_per_event(1);

    let export = registry.event("export.finished").unwrap();
    let ms = |n| Value::Duration(Duration::from_millis(n));
    let r = |fields: &[(&str, Value)]| events.record(&export, fields).unwrap();
    assert_eq!(
        r(&[
            ("format", Value::Choice("csv".into())),
            ("rows", Value::Count(437)),
            ("took", ms(1250))
        ]),
        Recorded::Written
    );
    let search = registry.event("search.used").unwrap();
    let got = events
        .record(
            &search,
            &[
                ("filters", Value::Set(vec!["size".into(), "date".into()])),
                ("results", Value::Count(7)),
                ("empty", Value::Bool(false)),
            ],
        )
        .unwrap();
    assert_eq!(got, Recorded::Written);
    assert_eq!(
        r(&[
            (
                "format",
                Value::Choice("C:/Users/someone/salaries.xlsx".into())
            ),
            ("rows", Value::Count(1)),
            ("took", ms(0))
        ]),
        Recorded::Dropped(Why::BadValue)
    );
    assert_eq!(
        r(&[
            ("format", Value::Choice("csv".into())),
            ("rows", Value::Count(1)),
            ("took", ms(0)),
            ("filename", Value::Choice("salaries".into()))
        ]),
        Recorded::Dropped(Why::UnknownField)
    );
    let opened = registry.event("app.opened").unwrap();
    assert_eq!(events.record(&opened, &[]).unwrap(), Recorded::Written);
    assert_eq!(events.record(&opened, &[]).unwrap(), Recorded::Capped);

    let rust = normalized(&std::fs::read_to_string(events.file()).unwrap());
    let dotnet = normalized(&std::fs::read_to_string(cases().join("dotnet-events.jsonl")).unwrap());
    assert_eq!(rust, dotnet);
}
