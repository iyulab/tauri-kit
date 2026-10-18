//! A stream kept in a folder — reports, session events, events: one segment per process that
//! writes it, so processes of the app (two instances of it, a shell and its backend) never write,
//! trim or delete each other's lines.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use crate::queue::read_offset;
use crate::session::{is_id, random_id};

/// How large a stream's folder may grow before [`tidy`] drops whole segments, as a default to pass
/// it: several hundred reports, which only a device failing on every launch for a long time would
/// write.
pub const MAX_STREAM_BYTES: usize = 1 << 20;

/// One process's part of a stream: a file in the stream's folder that this process alone appends
/// to — hand [`Segment::file`] to a [`Reporter`](crate::Reporter), [`Events`](crate::Events) or a
/// [`Session`](crate::Session) — beside a lock it holds while the `Segment` lives, which the
/// operating system lets go when the process goes, however it goes. Keep the `Segment` for as long
/// as anything writes its file. A sender reads every segment of the folder ([`segments`]) — live
/// ones too, since only whole lines are read — and [`tidy`] deletes only segments whose lock it can
/// take.
#[derive(Debug)]
pub struct Segment {
    file: PathBuf,
    _lock: File,
}

impl Segment {
    /// A new segment of the stream kept in `dir`, creating the folder when needed. Its file is
    /// made when it is first written.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Segment> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let id = random_id()?;
        let (lock, _) = hold(dir, &id)
            .ok_or_else(|| io::Error::other("the new segment's lock could not be taken"))?;
        Ok(Segment {
            file: dir.join(format!("{id}.jsonl")),
            _lock: lock,
        })
    }

    /// The file this process appends to.
    pub fn file(&self) -> &Path {
        &self.file
    }
}

/// One segment of a stream as a sender reads it: its lines, and the offset of what has been sent
/// — the pair [`pending`](crate::pending), [`unsent`](crate::unsent) and
/// [`mark_sent`](crate::mark_sent) take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentFiles {
    /// The segment's lines.
    pub file: PathBuf,
    /// How far into `file` has been sent.
    pub sent: PathBuf,
}

/// The segments of the stream kept in `dir`, oldest first — live ones too. A missing folder has
/// none.
pub fn segments(dir: &Path) -> io::Result<Vec<SegmentFiles>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Some(id) = segment_id(&path) else {
            continue;
        };
        let modified = entry.metadata()?.modified()?;
        found.push((modified, id.to_string(), path));
    }
    found.sort();
    Ok(found
        .into_iter()
        .map(|(_, id, file)| SegmentFiles {
            sent: dir.join(format!("{id}.sent")),
            file,
        })
        .collect())
}

/// Keeps the stream kept in `dir` from growing without end, without touching a segment a process
/// still writes. A segment whose lock can be taken — its process is gone — and whose whole lines
/// have all been sent is deleted; then, while the folder holds more than `max_bytes`, the oldest
/// such ended segments go whole, sent or not. A lock left without its segment is removed. Returns
/// how many bytes of segments were dropped.
pub fn tidy(dir: &Path, max_bytes: usize) -> io::Result<usize> {
    let mut dropped = 0;
    let mut kept = Vec::new();
    for segment in segments(dir)? {
        let Some(id) = segment_id(&segment.file).map(str::to_string) else {
            continue;
        };
        let bytes = std::fs::read(&segment.file)?;
        let Some(lock) = hold(dir, &id) else {
            kept.push((bytes.len(), None));
            continue; // still written
        };
        let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        if read_offset(&segment.sent).is_some_and(|sent| sent >= whole) {
            dropped += bytes.len();
            delete(&segment, lock)?;
        } else {
            kept.push((bytes.len(), Some((segment, lock))));
        }
    }
    let mut total: usize = kept.iter().map(|(len, _)| len).sum();
    for (len, ended) in kept {
        if total <= max_bytes {
            break;
        }
        if let Some((segment, lock)) = ended {
            total -= len;
            dropped += len;
            delete(&segment, lock)?;
        }
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "lock") {
            if let Some(id) = segment_id(&path).map(str::to_string) {
                if !dir.join(format!("{id}.jsonl")).exists() {
                    if let Some(lock) = hold(dir, &id) {
                        drop(lock.0);
                        let _ = std::fs::remove_file(lock.1);
                    }
                }
            }
        }
    }
    Ok(dropped)
}

/// Takes the single file a stream was kept in before — `file`, with `sent` recording how far into
/// it was sent — into the folder `dir` as one ended segment, so what was not sent yet still goes
/// and nothing goes twice. Returns whether there was a file to take. Call it before anything writes
/// to the folder.
pub fn adopt(dir: &Path, file: &Path, sent: &Path) -> io::Result<bool> {
    if !file.is_file() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    let id = random_id()?;
    let into = dir.join(format!("{id}.jsonl"));
    if std::fs::rename(file, &into).is_err() {
        std::fs::copy(file, &into)?;
        std::fs::remove_file(file)?;
    }
    if let Some(offset) = read_offset(sent) {
        std::fs::write(dir.join(format!("{id}.sent")), offset.to_string())?;
    }
    match std::fs::remove_file(sent) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    Ok(true)
}

/// The id a segment's file, sent offset or lock is named by.
fn segment_id(path: &Path) -> Option<&str> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| is_id(s))
}

/// Deletes an ended segment: its lines and offset, then the lock it was held by.
fn delete(segment: &SegmentFiles, lock: (File, PathBuf)) -> io::Result<()> {
    for path in [&segment.file, &segment.sent] {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    drop(lock.0);
    let _ = std::fs::remove_file(lock.1);
    Ok(())
}

/// The lock `<id>.lock` in `dir`, taken — or `None` while another holds it (what it guards is still
/// in use) or it cannot be opened. Held until the `File` is dropped or the process goes.
pub(crate) fn hold(dir: &Path, id: &str) -> Option<(File, PathBuf)> {
    let path = dir.join(format!("{id}.lock"));
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    match file.try_lock() {
        Ok(()) => Some((file, path)),
        Err(TryLockError::WouldBlock | TryLockError::Error(_)) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mark_sent, unsent, FrameRule, Layer, Report, Reporter};

    fn ui() -> Layer {
        Layer::new("ui", FrameRule::web_bundle())
    }

    fn report(reporter: &Reporter, kind: &str) {
        assert!(reporter
            .record(Report::new(&ui(), kind, "", "1.2.3"))
            .unwrap());
    }

    fn send_all(dir: &Path) -> Vec<String> {
        let mut kinds = Vec::new();
        for segment in segments(dir).unwrap() {
            for batch in unsent(&segment.file, &segment.sent).unwrap() {
                kinds.extend(batch.reports.iter().map(|r| r.kind.clone()));
                mark_sent(&segment.sent, batch.end).unwrap();
            }
        }
        kinds.sort();
        kinds
    }

    #[test]
    fn each_process_writes_its_own_segment_and_a_sender_reads_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("reports");
        let (a, b) = (
            Segment::open(&stream).unwrap(),
            Segment::open(&stream).unwrap(),
        );
        assert_ne!(a.file(), b.file());
        report(&Reporter::new(a.file()), "FromA");
        report(&Reporter::new(b.file()), "FromB");
        assert_eq!(send_all(&stream), ["FromA", "FromB"]);
        assert!(send_all(&stream).is_empty(), "nothing goes twice");
    }

    #[test]
    fn tidy_never_touches_a_segment_still_written_and_deletes_an_ended_one_once_sent() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("reports");
        let live = Segment::open(&stream).unwrap();
        let ended = Segment::open(&stream).unwrap();
        report(&Reporter::new(live.file()), "Live");
        report(&Reporter::new(ended.file()), "Ended");
        let ended_file = ended.file().to_path_buf();
        drop(ended); // its process is gone

        assert_eq!(
            tidy(&stream, MAX_STREAM_BYTES).unwrap(),
            0,
            "nothing sent yet"
        );
        assert!(ended_file.exists());
        send_all(&stream);
        assert!(tidy(&stream, MAX_STREAM_BYTES).unwrap() > 0);
        assert!(!ended_file.exists(), "ended and sent");
        assert!(live.file().exists(), "still written, though sent");
        report(&Reporter::new(live.file()), "Later");
        assert_eq!(send_all(&stream), ["Later"]);
    }

    #[test]
    fn past_its_limit_the_oldest_ended_segments_go_whole_and_live_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("reports");
        let mut ended = Vec::new();
        for i in 0..3 {
            let segment = Segment::open(&stream).unwrap();
            report(&Reporter::new(segment.file()), &format!("Ended{i}"));
            ended.push(segment.file().to_path_buf());
            std::thread::sleep(std::time::Duration::from_millis(20)); // a later modified time
        }
        let live = Segment::open(&stream).unwrap();
        report(&Reporter::new(live.file()), "Alive0"); // as long a line as each ended one
        let line = std::fs::metadata(live.file()).unwrap().len() as usize;

        tidy(&stream, line * 2).unwrap();
        assert!(!ended[0].exists() && !ended[1].exists(), "the oldest went");
        assert!(ended[2].exists());
        assert!(live.file().exists());
        tidy(&stream, 0).unwrap();
        assert!(!ended[2].exists());
        assert!(live.file().exists(), "a live segment never goes");
    }

    #[test]
    fn an_ended_segment_cut_off_mid_line_counts_as_sent_once_its_whole_lines_are() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("reports");
        let segment = Segment::open(&stream).unwrap();
        report(&Reporter::new(segment.file()), "Whole");
        let file = segment.file().to_path_buf();
        drop(segment);
        let mut bytes = std::fs::read(&file).unwrap();
        bytes.extend_from_slice(br#"{"layer":"ui","ki"#);
        std::fs::write(&file, bytes).unwrap();

        assert_eq!(send_all(&stream), ["Whole"]);
        tidy(&stream, MAX_STREAM_BYTES).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn the_single_file_of_before_is_taken_in_with_what_it_had_sent() {
        let dir = tempfile::tempdir().unwrap();
        let (old, old_sent) = (
            dir.path().join("reports.jsonl"),
            dir.path().join("reports.sent"),
        );
        let reporter = Reporter::new(&old);
        report(&reporter, "SentBefore");
        let first = unsent(&old, &old_sent).unwrap();
        mark_sent(&old_sent, first[0].end).unwrap();
        report(&reporter, "NotYet");

        let stream = dir.path().join("reports");
        assert!(adopt(&stream, &old, &old_sent).unwrap());
        assert!(!old.exists() && !old_sent.exists());
        assert!(!adopt(&stream, &old, &old_sent).unwrap(), "once");
        assert_eq!(send_all(&stream), ["NotYet"]);
        tidy(&stream, MAX_STREAM_BYTES).unwrap();
        assert!(
            segments(&stream).unwrap().is_empty(),
            "an adopted file is an ended segment"
        );
    }

    /// A segment lock another process holds — the .NET side's `Segment`, say — reads as live. Run by hand
    /// with the lock's path in the environment while that process holds it:
    /// `TAURI_KIT_HELD_LOCK=<dir>/<id>.lock cargo test -p tauri-kit-diagnostics -- --ignored a_lock_another_process_holds`
    #[test]
    #[ignore = "needs a lock held by another process"]
    fn a_lock_another_process_holds_reads_as_live() {
        let path = PathBuf::from(std::env::var("TAURI_KIT_HELD_LOCK").unwrap());
        let id = segment_id(&path).unwrap();
        assert!(hold(path.parent().unwrap(), id).is_none());
    }

    #[test]
    fn a_lock_left_without_its_segment_is_removed_and_other_files_stay() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("reports");
        std::fs::create_dir_all(&stream).unwrap();
        let stray = stream.join("0123456789abcdef0123456789abcdef.lock");
        std::fs::write(&stray, "").unwrap();
        std::fs::write(stream.join("notes.txt"), "not ours").unwrap();
        let live = Segment::open(&stream).unwrap();
        tidy(&stream, MAX_STREAM_BYTES).unwrap();
        assert!(!stray.exists());
        assert!(stream.join("notes.txt").exists());
        drop(live);
        assert!(segments(&dir.path().join("none")).unwrap().is_empty());
    }
}
