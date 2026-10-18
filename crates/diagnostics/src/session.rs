//! Sessions: when the app starts and ends, and which of its screens it spent time on — never
//! what a screen showed, what was typed into it, or who the person is.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::queue::append_line;
use crate::report::{plain_kind, utc};
use crate::stream::hold;
use crate::UNRECOGNIZED_KIND;

/// The most screens one session event carries; time on screens past it is left out.
pub const MAX_SCREENS: usize = 64;

/// A random identifier of this installation of the app: made on the device the first time it is
/// needed, from the operating system's random generator, and kept in a file.
///
/// It is never derived from the hardware, the account or anything else on the device, so it names
/// an installation and nothing more: deleting the file makes a new one. It is 32 lowercase
/// hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallId(String);

impl InstallId {
    /// The identifier kept in `file`, or a new one written there (crash-safely, with its folders)
    /// when `file` is missing or holds anything other than an identifier.
    pub fn load_or_create(file: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(file) {
            Ok(text) if is_id(text.trim()) => return Ok(InstallId(text.trim().to_string())),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let id = random_id()?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        tauri_kit_fs::write_atomic(file, id.as_bytes())?;
        Ok(InstallId(id))
    }

    /// The identifier, as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The screens of an app, by fixed names of its own.
///
/// A session records time only under these names: a name it is handed that is not one of them —
/// a title, a route with a record's identifier in it, anything built from what the person typed —
/// is recorded as [`UNRECOGNIZED_KIND`], whole. A name given here is kept only if it is a plain
/// identifier (an ASCII letter, then letters, digits, `_`, `.`, `:` or `-`, at most 100 characters).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screens(BTreeSet<String>);

impl Screens {
    /// The screens named `names`.
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Screens(
            names
                .into_iter()
                .map(|name| plain_kind(name.as_ref()))
                .filter(|name| name != UNRECOGNIZED_KIND)
                .collect(),
        )
    }

    /// The name time on `raw` is recorded under: `raw` itself if it is one of the screens,
    /// otherwise [`UNRECOGNIZED_KIND`].
    pub fn name(&self, raw: &str) -> String {
        if self.0.contains(raw) {
            raw.to_string()
        } else {
            UNRECOGNIZED_KIND.to_string()
        }
    }
}

/// Time on one screen during a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenTime {
    /// How many times the screen was opened.
    pub opened: u32,
    /// How long it was open in all, in whole seconds.
    pub seconds: u64,
}

/// What a [`SessionEvent`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// The app started.
    Start,
    /// The app ended as it should: its time and its time on each screen.
    End,
    /// A session that started did not end as it should — the app crashed, was ended by the system
    /// or lost power. Recorded when the next session starts, with [`Ended`] saying what is known of
    /// how.
    Unfinished,
}

/// What is known of how a session that did not end as it should ended — read from what the session
/// itself could still say before it went ([`Session::os_ending`], [`Session::updating`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Ended {
    /// Nothing was said: a crash, the app being ended from outside, lost power. These cannot be told
    /// apart without a crash dump.
    Unfinished,
    /// The operating system was ending the person's session (shutdown, restart, sign-out) while the
    /// app ran — not a crash.
    OsSessionEnd,
    /// The app was handing over to an installer — an update in progress.
    Updated,
}

/// One line of a session file, exactly as it is written and sent.
///
/// Every field comes from an allowlist, and only a [`Session`] makes one: the installation's
/// [`InstallId`], a random identifier of the session, the app's version, the platform and the
/// time; a start adds the interface's language (a language tag) and the app's own name for what
/// it was set up for (a plain identifier); an end adds how long the session lasted and the time on
/// each of the app's [`Screens`]; an unfinished session adds when it started. There is no field a
/// person's words, a file path or a record could travel in, and a line read back that breaks any of
/// these rules is not a session event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Line", into = "Line")]
pub struct SessionEvent {
    kind: SessionKind,
    head: Head,
    time: String,
    locale: Option<String>,
    track: Option<String>,
    started: Option<String>,
    seconds: Option<u64>,
    screens: BTreeMap<String, ScreenTime>,
    how: Option<Ended>,
    version_changed: bool,
}

impl SessionEvent {
    /// What the event says.
    pub fn kind(&self) -> SessionKind {
        self.kind
    }

    /// The installation's identifier.
    pub fn install(&self) -> &str {
        &self.head.install
    }

    /// The session's identifier: random, made when the session started.
    pub fn session(&self) -> &str {
        &self.head.session
    }

    /// The app's version.
    pub fn version(&self) -> &str {
        &self.head.version
    }

    /// The operating system and CPU architecture, as [`std::env::consts`] names them.
    pub fn platform(&self) -> (&str, &str) {
        (&self.head.os, &self.head.arch)
    }

    /// When the event happened, in UTC to the second (ISO 8601).
    pub fn time(&self) -> &str {
        &self.time
    }

    /// The interface's language, on a start.
    pub fn locale(&self) -> Option<&str> {
        self.locale.as_deref()
    }

    /// The app's own name for what it was set up for, on a start.
    pub fn track(&self) -> Option<&str> {
        self.track.as_deref()
    }

    /// When an unfinished session started.
    pub fn started(&self) -> Option<&str> {
        self.started.as_deref()
    }

    /// How long an ended session lasted, in whole seconds.
    pub fn seconds(&self) -> Option<u64> {
        self.seconds
    }

    /// For an unfinished session: what is known of how it ended. `None` on other kinds, and on a
    /// line written before this was recorded.
    pub fn how(&self) -> Option<Ended> {
        self.how
    }

    /// For an unfinished session: whether the session after it runs another version of the app —
    /// the app was replaced after it stopped (a crash, then the person installed again, say).
    pub fn version_changed(&self) -> bool {
        self.version_changed
    }

    /// The time on each screen, for an end.
    pub fn screens(&self) -> &BTreeMap<String, ScreenTime> {
        &self.screens
    }
}

/// Who and where a session is: the same on each of its events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    install: String,
    session: String,
    version: String,
    os: String,
    arch: String,
}

impl Head {
    fn is_valid(&self) -> bool {
        is_id(&self.install)
            && is_id(&self.session)
            && is_version(&self.version)
            && is_platform(&self.os)
            && is_platform(&self.arch)
    }
}

/// A session event as a line of the file: the fields written, checked when read back.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    event: SessionKind,
    install: String,
    session: String,
    version: String,
    os: String,
    arch: String,
    time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    track: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    screens: BTreeMap<String, ScreenTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    how: Option<Ended>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    version_changed: bool,
}

impl From<SessionEvent> for Line {
    fn from(e: SessionEvent) -> Self {
        Line {
            event: e.kind,
            install: e.head.install,
            session: e.head.session,
            version: e.head.version,
            os: e.head.os,
            arch: e.head.arch,
            time: e.time,
            locale: e.locale,
            track: e.track,
            started: e.started,
            seconds: e.seconds,
            screens: e.screens,
            how: e.how,
            version_changed: e.version_changed,
        }
    }
}

impl TryFrom<Line> for SessionEvent {
    type Error = String;

    fn try_from(l: Line) -> Result<Self, String> {
        let event = SessionEvent {
            kind: l.event,
            head: Head {
                install: l.install,
                session: l.session,
                version: l.version,
                os: l.os,
                arch: l.arch,
            },
            time: l.time,
            locale: l.locale,
            track: l.track,
            started: l.started,
            seconds: l.seconds,
            screens: l.screens,
            how: l.how,
            version_changed: l.version_changed,
        };
        if event.is_valid() {
            Ok(event)
        } else {
            Err("not a session event".into())
        }
    }
}

impl SessionEvent {
    /// Whether every field keeps to its rule, and the event has exactly the fields of its kind.
    fn is_valid(&self) -> bool {
        let fields = match self.kind {
            SessionKind::Start => {
                self.locale.as_deref().is_some_and(is_locale)
                    && self.track.as_deref().is_some_and(is_plain)
                    && self.started.is_none()
                    && self.seconds.is_none()
                    && self.screens.is_empty()
                    && self.how.is_none()
                    && !self.version_changed
            }
            SessionKind::End => {
                self.locale.is_none()
                    && self.track.is_none()
                    && self.started.is_none()
                    && self.seconds.is_some()
                    && self.screens.len() <= MAX_SCREENS
                    && self.screens.keys().all(|name| is_plain(name))
                    && self.how.is_none()
                    && !self.version_changed
            }
            SessionKind::Unfinished => {
                self.locale.is_none()
                    && self.track.is_none()
                    && self.started.as_deref().is_some_and(is_time)
                    && self.seconds.is_none()
                    && self.screens.is_empty()
            }
        };
        fields && self.head.is_valid() && is_time(&self.time)
    }
}

/// What a running session remembers in its marker file, so the next session can tell it did not
/// end.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Running {
    #[serde(flatten)]
    head: Head,
    started: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ending: Option<Ended>,
}

/// The session of one launch: written to a JSON Lines file the person can read — exactly what
/// would be sent — as it starts and ends.
///
/// Start one with [`Session::builder`] when the app starts, tell it which screen is open with
/// [`Session::screen`], and [`Session::end`] it when the app ends as it should. While it runs, a marker in a folder of
/// running sessions says so, beside a lock the session holds; a session that starts and finds the
/// marker of one whose lock nobody holds any more — its process is gone — records that one as
/// [`SessionKind::Unfinished`] first. Sessions of two instances of the app running at once each
/// keep their own marker, so neither is taken for unfinished while it runs. Make one per launch,
/// in one process of the app.
///
/// ```no_run
/// use std::path::Path;
/// use tauri_kit_diagnostics::{tidy, InstallId, Screens, Segment, Session, MAX_STREAM_BYTES};
///
/// let dir = Path::new("/path/to/logs");
/// tidy(&dir.join("sessions"), MAX_STREAM_BYTES)?;
/// let segment = Segment::open(dir.join("sessions"))?;
/// let install = InstallId::load_or_create(&dir.join("install-id"))?;
/// let session = Session::builder(&install, env!("CARGO_PKG_VERSION"))
///     .locale("ko-KR")
///     .track("school")
///     .screens(Screens::new(["home", "list", "report", "settings"]))
///     .start(segment.file(), &dir.join("running"))?;
///
/// session.screen(Some("home"));
/// session.screen(Some("report"));
/// // The window is hidden: no screen is open.
/// session.screen(None);
/// session.end()?;
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct Session {
    file: PathBuf,
    /// This session's marker: what it says, rewritten whole ([`Running`]).
    marker: PathBuf,
    /// The lock beside it, held while the session runs — released by the operating system when the
    /// process goes, however it goes. `None` once the session has ended.
    lock: Mutex<Option<(File, PathBuf)>>,
    head: Head,
    /// When it started, as the marker says.
    started_at: String,
    screens: Screens,
    started: Instant,
    clock: Mutex<Clock>,
}

/// Time on screens so far, and the screen open now.
#[derive(Debug, Default)]
struct Clock {
    open: Option<(String, Instant)>,
    totals: BTreeMap<String, (u32, std::time::Duration)>,
    ended: bool,
}

/// What a session starts with: see [`Session::builder`].
#[derive(Debug)]
pub struct SessionBuilder {
    head: Head,
    locale: String,
    track: String,
    screens: Screens,
}

impl Session {
    /// A session of the installation `install`, of the app at `version` (typically
    /// `env!("CARGO_PKG_VERSION")`).
    pub fn builder(install: &InstallId, version: &str) -> SessionBuilder {
        SessionBuilder {
            head: Head {
                install: install.0.clone(),
                session: String::new(),
                version: if is_version(version) {
                    version.to_string()
                } else {
                    UNRECOGNIZED_KIND.to_string()
                },
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
            },
            locale: "und".to_string(),
            track: "default".to_string(),
            screens: Screens::default(),
        }
    }

    /// The file the session's events are appended to.
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The session's random identifier.
    pub fn id(&self) -> &str {
        &self.head.session
    }

    /// The session id, version and platform an event of this session carries — never the installation.
    pub(crate) fn event_head(&self) -> (String, String, String, String) {
        let h = &self.head;
        (
            h.session.clone(),
            h.version.clone(),
            h.os.clone(),
            h.arch.clone(),
        )
    }

    /// Says which screen is open now — one of the app's [`Screens`], or `None` when none is (the
    /// window hidden or minimised). Time on the screen open before ends here. Nothing is written
    /// until the session ends.
    pub fn screen(&self, name: Option<&str>) {
        self.screen_at(name, Instant::now());
    }

    fn screen_at(&self, name: Option<&str>, now: Instant) {
        let mut clock = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.ended {
            return;
        }
        let name = name.map(|raw| self.screens.name(raw));
        if let Some((open, since)) = clock.open.take() {
            if name.as_deref() == Some(open.as_str()) {
                clock.open = Some((open, since));
                return;
            }
            let total = clock.totals.entry(open).or_default();
            total.1 += now.saturating_duration_since(since);
        }
        if let Some(name) = name {
            let full = clock.totals.len() >= MAX_SCREENS && !clock.totals.contains_key(&name);
            if !full {
                clock.totals.entry(name.clone()).or_default().0 += 1;
                clock.open = Some((name, now));
            }
        }
    }

    /// Says that the operating system is ending the person's session (shutdown, restart, sign-out)
    /// while the app runs. If the app then goes without [`Session::end`], the next session records
    /// it as [`Ended::OsSessionEnd`] — not a crash. Call it from the platform's notice that the session
    /// is ending (on Windows, `WM_QUERYENDSESSION` or `WM_ENDSESSION`), before anything that may not
    /// finish.
    pub fn os_ending(&self) -> io::Result<()> {
        self.say(Ended::OsSessionEnd)
    }

    /// Says that the app is handing over to an installer — an update. If the app then goes without
    /// [`Session::end`] (the installer ends it), the next session records it as [`Ended::Updated`].
    /// A different version alone is not taken as an update: a crash followed by the person
    /// installing again is still a crash ([`SessionEvent::version_changed`]).
    pub fn updating(&self) -> io::Result<()> {
        self.say(Ended::Updated)
    }

    /// Writes into the marker how the session is about to go, unless it has already ended.
    fn say(&self, ending: Ended) -> io::Result<()> {
        if self.clock.lock().unwrap_or_else(|e| e.into_inner()).ended {
            return Ok(());
        }
        let running = Running {
            head: self.head.clone(),
            started: self.started_at.clone(),
            ending: Some(ending),
        };
        tauri_kit_fs::write_atomic(
            &self.marker,
            &serde_json::to_vec(&running).map_err(io::Error::other)?,
        )
    }

    /// Ends the session as it should: writes how long it lasted and the time on each screen, and
    /// removes the marker. A session ends once; ending it again does nothing. Call it on every quit
    /// the app controls, before any step that waits — a quit that waits on something first and is
    /// cut short leaves the marker, and the session is recorded as unfinished.
    pub fn end(&self) -> io::Result<()> {
        self.end_at(Instant::now())
    }

    fn end_at(&self, now: Instant) -> io::Result<()> {
        {
            let clock = self.clock.lock().unwrap_or_else(|e| e.into_inner());
            if clock.ended {
                return Ok(());
            }
        }
        self.screen_at(None, now);
        let mut clock = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.ended = true;
        let screens = clock
            .totals
            .iter()
            .map(|(name, (opened, time))| {
                (
                    name.clone(),
                    ScreenTime {
                        opened: *opened,
                        seconds: time.as_secs(),
                    },
                )
            })
            .collect();
        drop(clock);
        let end = SessionEvent {
            kind: SessionKind::End,
            head: self.head.clone(),
            time: utc(std::time::SystemTime::now()),
            locale: None,
            track: None,
            started: None,
            seconds: Some(now.saturating_duration_since(self.started).as_secs()),
            screens,
            how: None,
            version_changed: false,
        };
        append_line(&self.file, &end)?;
        remove_if_there(&self.marker)?;
        // The marker is gone before the lock is let go, so no starting session finds a marker it
        // could lock while this one still says it runs.
        if let Some((lock, path)) = self.lock.lock().unwrap_or_else(|e| e.into_inner()).take() {
            drop(lock);
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

/// Removes `path`; a file that is not there is nothing to remove.
fn remove_if_there(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What a marker file says, if it is one.
fn read_running(path: &Path) -> Option<Running> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Running>(&bytes).ok())
        .filter(|r| r.head.is_valid() && is_time(&r.started))
}

/// The sessions in `running` that did not end and whose process is gone — each marker whose lock
/// can be taken — oldest first, with their markers removed. A lock left without its marker (a
/// session that ended but went before letting its lock go) is removed too.
fn sessions_gone(running: &Path) -> io::Result<Vec<Running>> {
    let mut gone = Vec::new();
    for entry in std::fs::read_dir(running)? {
        let path = entry?.path();
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| is_id(s))
        else {
            continue;
        };
        let marker = running.join(format!("{id}.json"));
        let is_lock = path.extension().is_some_and(|e| e == "lock");
        if is_lock && marker.exists() {
            continue; // seen with its marker
        }
        let Some((lock, lock_path)) = hold(running, id) else {
            continue; // still running
        };
        if let Some(before) = read_running(&marker) {
            gone.push(before);
        }
        remove_if_there(&marker)?;
        drop(lock);
        let _ = std::fs::remove_file(lock_path);
    }
    gone.sort_by(|a, b| a.started.cmp(&b.started));
    Ok(gone)
}

impl SessionBuilder {
    /// The interface's language, as a language tag (`ko-KR`, `en`). Anything else is recorded as
    /// `und` (undetermined). By default `und`.
    pub fn locale(mut self, locale: &str) -> Self {
        self.locale = if is_locale(locale) {
            locale.to_string()
        } else {
            "und".to_string()
        };
        self
    }

    /// The app's own name for what this installation is set up for — an edition, a profile. A name
    /// that is not a plain identifier is recorded as [`UNRECOGNIZED_KIND`]. By default `default`.
    pub fn track(mut self, track: &str) -> Self {
        self.track = plain_kind(track);
        self
    }

    /// The app's screens. Without them, time on any screen is recorded as [`UNRECOGNIZED_KIND`].
    pub fn screens(mut self, screens: Screens) -> Self {
        self.screens = screens;
        self
    }

    /// Starts the session: appends to `file` each session in the folder `running` that did not end
    /// and whose process is gone, then this session's start, and keeps its own marker and lock in
    /// `running` until [`Session::end`]. Give every process of the app that starts sessions the same
    /// `running`. A file where the folder goes is the single marker earlier versions kept: it is
    /// read as one more session that may not have ended, then replaced by the folder. Call
    /// [`tidy`](crate::tidy) on the stream `file` is a segment of first.
    pub fn start(
        self,
        file: impl Into<PathBuf>,
        running: impl Into<PathBuf>,
    ) -> io::Result<Session> {
        let (file, running) = (file.into(), running.into());
        let now = utc(std::time::SystemTime::now());
        let mut gone = Vec::new();
        if running.is_file() {
            gone.extend(read_running(&running));
            std::fs::remove_file(&running)?;
        }
        std::fs::create_dir_all(&running)?;
        gone.extend(sessions_gone(&running)?);
        for before in gone {
            let version_changed = before.head.version != self.head.version;
            let unfinished = SessionEvent {
                kind: SessionKind::Unfinished,
                head: before.head,
                time: now.clone(),
                locale: None,
                track: None,
                started: Some(before.started),
                seconds: None,
                screens: BTreeMap::new(),
                how: Some(before.ending.unwrap_or(Ended::Unfinished)),
                version_changed,
            };
            append_line(&file, &unfinished)?;
        }
        let mut head = self.head;
        head.session = random_id()?;
        let start = SessionEvent {
            kind: SessionKind::Start,
            head: head.clone(),
            time: now.clone(),
            locale: Some(self.locale),
            track: Some(self.track),
            started: None,
            seconds: None,
            screens: BTreeMap::new(),
            how: None,
            version_changed: false,
        };
        append_line(&file, &start)?;
        let marker = running.join(format!("{}.json", head.session));
        // The lock first: a session starting meanwhile that finds the marker finds it held.
        let lock = hold(&running, &head.session)
            .ok_or_else(|| io::Error::other("the new session's lock could not be taken"))?;
        let running = Running {
            head: head.clone(),
            started: now.clone(),
            ending: None,
        };
        tauri_kit_fs::write_atomic(
            &marker,
            &serde_json::to_vec(&running).map_err(io::Error::other)?,
        )?;
        Ok(Session {
            file,
            marker,
            lock: Mutex::new(Some(lock)),
            head,
            started_at: now,
            screens: self.screens,
            started: Instant::now(),
            clock: Mutex::default(),
        })
    }
}

/// 16 bytes from the operating system's random generator, as 32 lowercase hexadecimal digits.
pub(crate) fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// 32 lowercase hexadecimal digits.
pub(crate) fn is_id(raw: &str) -> bool {
    raw.len() == 32 && raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A plain identifier, as a report's kind is.
fn is_plain(raw: &str) -> bool {
    plain_kind(raw) == raw
}

/// A version: ASCII letters, digits, `.`, `-`, `+` and `_`, 1 to 64 of them.
pub(crate) fn is_version(raw: &str) -> bool {
    (1..=64).contains(&raw.len())
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

/// An operating system or architecture name: ASCII letters, digits and `_`, 1 to 32 of them.
pub(crate) fn is_platform(raw: &str) -> bool {
    (1..=32).contains(&raw.len()) && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A language tag: subtags of 1 to 8 ASCII letters or digits joined by `-`, the first letters
/// only, at most 35 characters.
fn is_locale(raw: &str) -> bool {
    raw.len() <= 35
        && raw.split('-').enumerate().all(|(i, part)| {
            (1..=8).contains(&part.len())
                && if i == 0 {
                    part.chars().all(|c| c.is_ascii_alphabetic())
                } else {
                    part.chars().all(|c| c.is_ascii_alphanumeric())
                }
        })
}

/// A time as [`utc`] writes it: `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn is_time(raw: &str) -> bool {
    let b = raw.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn lines(file: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
        (
            dir.path().join("sessions.jsonl"),
            dir.path().join("session.running"),
            dir.path().join("install-id"),
        )
    }

    fn screens() -> Screens {
        Screens::new(["home", "list", "report"])
    }

    #[test]
    fn an_install_id_is_random_kept_and_replaced_when_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a").join("id"), dir.path().join("b"));
        let first = InstallId::load_or_create(&a).unwrap();
        assert!(is_id(first.as_str()));
        assert_eq!(InstallId::load_or_create(&a).unwrap(), first, "kept");
        assert_ne!(InstallId::load_or_create(&b).unwrap(), first, "random");

        std::fs::write(&a, "C:\\Users\\someone\n").unwrap();
        let replaced = InstallId::load_or_create(&a).unwrap();
        assert!(is_id(replaced.as_str()));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), replaced.as_str());
    }

    #[test]
    fn screens_outside_the_list_are_recorded_whole_as_unrecognized() {
        let screens = Screens::new(["home", "a name, with words", "list"]);
        assert_eq!(screens.name("home"), "home");
        assert_eq!(screens.name("a name, with words"), UNRECOGNIZED_KIND);
        assert_eq!(screens.name("subject/3f2a"), UNRECOGNIZED_KIND);
        assert_eq!(screens.name("Home"), UNRECOGNIZED_KIND);
    }

    #[test]
    fn a_session_writes_its_start_and_its_end_with_time_on_each_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        let session = Session::builder(&install, "1.2.3")
            .locale("ko-KR")
            .track("school")
            .screens(screens())
            .start(&file, &marker)
            .unwrap();
        let own = marker.join(format!("{}.json", session.id()));
        assert!(own.is_file(), "running");
        assert!(marker.join(format!("{}.lock", session.id())).is_file());

        let t0 = Instant::now();
        session.screen_at(Some("home"), t0);
        session.screen_at(Some("home"), t0 + Duration::from_secs(5));
        session.screen_at(Some("report"), t0 + Duration::from_secs(10));
        session.screen_at(Some("subject/Kim"), t0 + Duration::from_secs(13));
        session.screen_at(None, t0 + Duration::from_secs(20));
        session.screen_at(Some("home"), t0 + Duration::from_secs(30));
        session.end_at(t0 + Duration::from_secs(34)).unwrap();
        session.end_at(t0 + Duration::from_secs(99)).unwrap();
        assert_eq!(std::fs::read_dir(&marker).unwrap().count(), 0, "ended");

        let written = lines(&file);
        assert_eq!(written.len(), 2);
        let start = &written[0];
        assert_eq!(start["event"], "start");
        assert_eq!(start["install"], install.as_str());
        assert_eq!(start["session"], session.id());
        assert_eq!(start["version"], "1.2.3");
        assert_eq!(start["locale"], "ko-KR");
        assert_eq!(start["track"], "school");
        let end = &written[1];
        assert_eq!(end["event"], "end");
        assert_eq!(end["session"], session.id());
        assert_eq!(
            end["screens"],
            serde_json::json!({
                "home": { "opened": 2, "seconds": 14 },
                "report": { "opened": 1, "seconds": 3 },
                "Unrecognized": { "opened": 1, "seconds": 7 },
            })
        );
    }

    #[test]
    fn a_session_that_did_not_end_is_recorded_when_the_next_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        let crashed = Session::builder(&install, "1.2.3")
            .start(&file, &marker)
            .unwrap();
        let crashed_id = crashed.id().to_string();
        drop(crashed);

        let next = Session::builder(&install, "1.2.4")
            .start(&file, &marker)
            .unwrap();
        let written = lines(&file);
        assert_eq!(written.len(), 3);
        assert_eq!(written[1]["event"], "unfinished");
        assert_eq!(written[1]["session"], crashed_id);
        assert_eq!(written[1]["version"], "1.2.3");
        assert_eq!(written[1]["started"], written[0]["time"]);
        assert_eq!(written[2]["event"], "start");
        assert_eq!(written[2]["session"], next.id());
        assert_ne!(next.id(), crashed_id);
    }

    #[test]
    fn two_instances_running_at_once_keep_their_own_markers() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        let start = || {
            Session::builder(&install, "1.2.3")
                .start(&file, &marker)
                .unwrap()
        };
        let first = start();
        let second = start();
        let events = |file: &Path| -> Vec<(String, String)> {
            lines(file)
                .iter()
                .map(|l| {
                    (
                        l["event"].as_str().unwrap().to_string(),
                        l["session"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        };
        assert!(
            events(&file).iter().all(|(e, _)| e == "start"),
            "a session still running is not taken for unfinished"
        );

        first.end().unwrap();
        let second_id = second.id().to_string();
        assert!(
            marker.join(format!("{second_id}.json")).is_file(),
            "the first ending leaves the second's marker"
        );
        drop(second); // the second goes without ending: its process is gone, its lock let go
        let third = start();

        let written = events(&file);
        let unfinished: Vec<_> = written.iter().filter(|(e, _)| e == "unfinished").collect();
        assert_eq!(unfinished, [&("unfinished".to_string(), second_id)]);
        third.end().unwrap();
    }

    #[test]
    fn the_single_marker_of_an_earlier_version_is_read_once_and_becomes_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        let old = r#"{"install":"00000000000000000000000000000001","session":"00000000000000000000000000000002","version":"1.2.2","os":"windows","arch":"x86_64","started":"2026-01-01T00:00:00Z"}"#;
        std::fs::write(&marker, old).unwrap();

        let next = Session::builder(&install, "1.2.3")
            .start(&file, &marker)
            .unwrap();
        assert!(marker.is_dir());
        let written = lines(&file);
        assert_eq!(written[0]["event"], "unfinished");
        assert_eq!(written[0]["session"], "00000000000000000000000000000002");
        assert_eq!(written[0]["version_changed"], true);
        assert_eq!(written[1]["session"], next.id());

        next.end().unwrap();
        Session::builder(&install, "1.2.3")
            .start(&file, &marker)
            .unwrap()
            .end()
            .unwrap();
        assert_eq!(
            lines(&file)
                .iter()
                .filter(|l| l["event"] == "unfinished")
                .count(),
            1,
            "read once"
        );
    }

    #[test]
    fn a_lock_left_without_its_marker_is_cleared_and_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        std::fs::create_dir_all(&marker).unwrap();
        let stray = marker.join("0123456789abcdef0123456789abcdef.lock");
        std::fs::write(&stray, "").unwrap();
        std::fs::write(marker.join("notes.txt"), "not ours").unwrap();

        let session = Session::builder(&install, "1.2.3")
            .start(&file, &marker)
            .unwrap();
        assert!(!stray.exists());
        assert!(
            marker.join("notes.txt").exists(),
            "only its own files are touched"
        );
        assert_eq!(lines(&file).len(), 1, "only the start");
        session.end().unwrap();
    }

    #[test]
    fn an_unfinished_session_says_how_it_ended_when_it_could_say() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        let start = || {
            Session::builder(&install, "1.2.3")
                .start(&file, &marker)
                .unwrap()
        };

        drop(start()); // said nothing: a crash
        let ending = start();
        ending.os_ending().unwrap();
        drop(ending); // the system ended the person's session
        let updating = start();
        updating.updating().unwrap();
        drop(updating); // the installer ended it
        let clean = start();
        clean.os_ending().unwrap();
        clean.end().unwrap(); // said it would go, then ended as it should
        clean.updating().unwrap(); // after the end: nothing more is written
        start();

        let how: Vec<_> = lines(&file)
            .iter()
            .filter(|l| l["event"] == "unfinished")
            .map(|l| l["how"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(how, ["unfinished", "os_session_end", "updated"]);
        assert!(lines(&file)
            .iter()
            .all(|l| l.get("version_changed").is_none()));
    }

    #[test]
    fn another_version_after_a_crash_is_still_a_crash_and_says_the_version_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        drop(
            Session::builder(&install, "1.2.3")
                .start(&file, &marker)
                .unwrap(),
        );
        Session::builder(&install, "1.2.4")
            .start(&file, &marker)
            .unwrap();

        let unfinished = &lines(&file)[1];
        assert_eq!(unfinished["how"], "unfinished");
        assert_eq!(unfinished["version_changed"], true);
        assert_eq!(unfinished["version"], "1.2.3");
    }

    #[test]
    fn how_and_version_changed_belong_to_an_unfinished_session_only() {
        let base = |extra: serde_json::Value| {
            let mut line = serde_json::json!({
                "install": "0123456789abcdef0123456789abcdef", "session": "fedcba9876543210fedcba9876543210",
                "version": "1.2.3", "os": "windows", "arch": "x86_64", "time": "2026-10-08T09:00:00Z",
            });
            line.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<SessionEvent>(line)
        };
        let started = "2026-10-08T08:00:00Z";
        assert!(
            base(serde_json::json!({"event": "unfinished", "started": started})).is_ok(),
            "a line from before"
        );
        assert!(base(
            serde_json::json!({"event": "unfinished", "started": started, "how": "updated"})
        )
        .is_ok());
        assert!(base(
            serde_json::json!({"event": "unfinished", "started": started, "how": "a crash at C:/x"})
        )
        .is_err());
        assert!(base(serde_json::json!({"event": "end", "seconds": 5, "how": "updated"})).is_err());
        assert!(base(serde_json::json!({"event": "start", "locale": "en", "track": "default", "version_changed": true})).is_err());
    }

    #[test]
    fn no_field_can_carry_words_a_path_or_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        let session = Session::builder(&install, "1.2.3 (C:\\Users\\Kim)")
            .locale("Kim Minji")
            .track("Kim's school, Seoul")
            .screens(Screens::new(["home"]))
            .start(&file, &marker)
            .unwrap();
        session.screen(Some("/subjects/Kim Minji"));
        session.end().unwrap();

        for line in lines(&file) {
            let text = line.to_string();
            assert!(!text.contains("Kim") && !text.contains("Users"), "{text}");
            let keys: BTreeSet<_> = line.as_object().unwrap().keys().cloned().collect();
            let allowed: BTreeSet<String> = [
                "event", "install", "session", "version", "os", "arch", "time", "locale", "track",
                "started", "seconds", "screens",
            ]
            .map(String::from)
            .into();
            assert!(keys.is_subset(&allowed), "{keys:?}");
        }
        let written = lines(&file);
        assert_eq!(written[0]["version"], UNRECOGNIZED_KIND);
        assert_eq!(written[0]["locale"], "und");
        assert_eq!(written[0]["track"], UNRECOGNIZED_KIND);
    }

    #[test]
    fn a_line_read_back_that_breaks_a_rule_is_not_a_session_event() {
        let good = r#"{"event":"start","install":"0123456789abcdef0123456789abcdef","session":"fedcba9876543210fedcba9876543210","version":"1.2.3","os":"windows","arch":"x86_64","time":"2026-10-08T09:00:00Z","locale":"ko-KR","track":"school"}"#;
        assert!(serde_json::from_str::<SessionEvent>(good).is_ok());
        for bad in [
            good.replace("school", "Kim's school"),
            good.replace(
                "\"track\":\"school\"",
                "\"track\":\"school\",\"note\":\"words\"",
            ),
            good.replace("0123456789abcdef0123456789abcdef", "my-pc-name"),
            good.replace("\"event\":\"start\"", "\"event\":\"end\""),
            good.replace("2026-10-08T09:00:00Z", "yesterday"),
        ] {
            assert!(serde_json::from_str::<SessionEvent>(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_session_keeps_at_most_its_most_screens() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker, id) = paths(&dir);
        let install = InstallId::load_or_create(&id).unwrap();
        let names: Vec<String> = (0..MAX_SCREENS + 5).map(|i| format!("s{i}")).collect();
        let session = Session::builder(&install, "1.2.3")
            .screens(Screens::new(&names))
            .start(&file, &marker)
            .unwrap();
        for name in &names {
            session.screen(Some(name));
        }
        session.end().unwrap();
        let end: SessionEvent = serde_json::from_value(lines(&file)[1].clone()).unwrap();
        assert_eq!(end.screens().len(), MAX_SCREENS);
    }
}
