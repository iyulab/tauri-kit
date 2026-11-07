//! The answers file of a test run, and the log of what was asked beside it.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Answer, Ask, Error, Kind};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Line {
    Save { path: PathBuf },
    Open { paths: Vec<PathBuf> },
    Folder { path: PathBuf },
    Cancel,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Asked<'a> {
    kind: &'static str,
    title: &'a str,
    default_path: Option<&'a Path>,
    filters: Vec<AskedFilter<'a>>,
}

#[derive(Serialize)]
struct AskedFilter<'a> {
    name: &'a str,
    extensions: &'a [&'a str],
}

/// The log of what was asked: the answers file's path with `.asked` added.
pub(crate) fn asked_log(answers: &Path) -> PathBuf {
    let mut name = OsString::from(answers.as_os_str());
    name.push(".asked");
    PathBuf::from(name)
}

/// The answer for `ask` from the answers file: the line after the ones the dialogs asked before it
/// took. The ask is logged whether or not it is answered.
pub(crate) fn take(answers: &Path, ask: &Ask<'_>) -> Result<Answer, Error> {
    let log = asked_log(answers);
    let taken = lines(&log)?.len();
    let record = Asked {
        kind: ask.kind.name(),
        title: ask.title,
        default_path: ask.default_path,
        filters: ask
            .filters
            .iter()
            .map(|f| AskedFilter {
                name: f.name,
                extensions: f.extensions,
            })
            .collect(),
    };
    let record = serde_json::to_string(&record).map_err(|e| Error::Queue(e.to_string()))?;
    tauri_kit_fs::append_line(&log, &record)
        .map_err(|e| Error::Queue(format!("{}: {e}", log.display())))?;

    let Some(text) = lines(answers)?.into_iter().nth(taken) else {
        return Err(Error::Unanswered {
            kind: ask.kind,
            title: ask.title.to_owned(),
        });
    };
    let mismatch = || Error::Mismatch {
        kind: ask.kind,
        answer: text.clone(),
    };
    let line: Line = serde_json::from_str(&text).map_err(|_| mismatch())?;
    match (ask.kind, line) {
        (_, Line::Cancel) => Ok(Answer::Cancelled),
        (Kind::Save, Line::Save { path }) | (Kind::Folder, Line::Folder { path }) => {
            Ok(Answer::Paths(vec![path]))
        }
        (Kind::Open, Line::Open { paths }) if paths.len() == 1 => Ok(Answer::Paths(paths)),
        (Kind::OpenMany, Line::Open { paths }) if !paths.is_empty() => Ok(Answer::Paths(paths)),
        _ => Err(mismatch()),
    }
}

/// The file's non-empty lines; none when it does not exist.
fn lines(path: &Path) -> Result<Vec<String>, Error> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::Queue(format!("{}: {e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Filter;

    fn ask(kind: Kind) -> Ask<'static> {
        Ask {
            kind,
            title: "Export",
            default_path: Some(Path::new("report.csv")),
            filters: &[Filter {
                name: "CSV",
                extensions: &["csv"],
            }],
        }
    }

    fn file(lines: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers.jsonl");
        fs::write(
            &path,
            lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn each_dialog_takes_the_next_answer_and_is_logged() {
        let (_dir, answers) = file(&[
            r#"{"kind":"save","path":"/out/report.csv"}"#,
            r#"{"kind":"cancel"}"#,
            r#"{"kind":"open","paths":["/a.txt","/b.txt"]}"#,
        ]);
        assert_eq!(
            take(&answers, &ask(Kind::Save)),
            Ok(Answer::Paths(vec![PathBuf::from("/out/report.csv")]))
        );
        assert_eq!(take(&answers, &ask(Kind::Folder)), Ok(Answer::Cancelled));
        assert_eq!(
            take(&answers, &ask(Kind::OpenMany)),
            Ok(Answer::Paths(vec!["/a.txt".into(), "/b.txt".into()]))
        );
        let asked = fs::read_to_string(asked_log(&answers)).unwrap();
        let first: serde_json::Value = serde_json::from_str(asked.lines().next().unwrap()).unwrap();
        assert_eq!(
            first,
            serde_json::json!({
                "kind": "save", "title": "Export", "defaultPath": "report.csv",
                "filters": [{ "name": "CSV", "extensions": ["csv"] }]
            })
        );
        assert_eq!(asked.lines().count(), 3);
    }

    #[test]
    fn answers_appended_while_the_app_runs_are_taken_in_turn() {
        let (_dir, answers) = file(&[r#"{"kind":"folder","path":"/vault"}"#]);
        assert!(take(&answers, &ask(Kind::Folder)).is_ok());
        assert_eq!(
            take(&answers, &ask(Kind::Folder)),
            Err(Error::Unanswered {
                kind: Kind::Folder,
                title: "Export".into()
            })
        );
        tauri_kit_fs::append_line(&answers, r#"{"kind":"folder","path":"/other"}"#).unwrap();
        // The unanswered dialog was logged, so it used up its turn: the next one takes the next line.
        tauri_kit_fs::append_line(&answers, r#"{"kind":"folder","path":"/third"}"#).unwrap();
        assert_eq!(
            take(&answers, &ask(Kind::Folder)),
            Ok(Answer::Paths(vec!["/third".into()]))
        );
    }

    #[test]
    fn an_answer_of_another_kind_is_refused() {
        for (kind, line) in [
            (Kind::Open, r#"{"kind":"save","path":"/x"}"#),
            (Kind::Open, r#"{"kind":"open","paths":["/a","/b"]}"#),
            (Kind::OpenMany, r#"{"kind":"open","paths":[]}"#),
            (Kind::Save, r#"{"kind":"folder","path":"/x"}"#),
            (Kind::Save, r#"{"kind":"save"}"#),
            (Kind::Save, "not json"),
        ] {
            let (_dir, answers) = file(&[line]);
            assert_eq!(
                take(&answers, &ask(kind)),
                Err(Error::Mismatch {
                    kind,
                    answer: line.into()
                }),
                "{line}"
            );
        }
    }

    #[test]
    fn no_answers_file_leaves_every_dialog_unanswered() {
        let dir = tempfile::tempdir().unwrap();
        let answers = dir.path().join("none.jsonl");
        assert!(matches!(
            take(&answers, &ask(Kind::Save)),
            Err(Error::Unanswered { .. })
        ));
    }
}
