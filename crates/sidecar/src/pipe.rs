//! Talking to a supervisor over its local pipe (`pipe` feature): for a shell whose backend is started and kept by a separate
//! process — not by the shell — and that process answers on a named pipe (Windows) or a Unix domain socket, one JSON object per
//! line. A request carries an `id` and a command; the answer is the line that echoes that `id`; any line without an `id` is an
//! event the supervisor pushed.
//!
//! Each request gets a connection of its own (opened, written, read to its answer, closed), and events are read on one held
//! connection that is opened again after the supervisor goes away. So no handle is ever read on one thread while another
//! writes — which a synchronous Windows pipe handle would serialize — and nothing here needs overlapped I/O. A supervisor that
//! serves one client at a time is not supported: it must accept several connections (a .NET `NamedPipeServerStream` with more
//! than one instance does).
//!
//! The pipe's name, the command names, the extra fields and the event shapes are the caller's; this module knows only `id`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

/// Where a supervisor listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipeAddress(PathBuf);

impl PipeAddress {
    /// The address a .NET `NamedPipeServerStream` named `name` listens on: `\\.\pipe\<name>` on Windows, and on Unix the
    /// socket .NET makes for it, `<temp>/CoreFxPipe_<name>` (`<temp>` as .NET's `Path.GetTempPath()`: `TMPDIR`, else `/tmp`).
    pub fn dotnet(name: &str) -> PipeAddress {
        #[cfg(windows)]
        {
            PipeAddress(PathBuf::from(format!(r"\\.\pipe\{name}")))
        }
        #[cfg(not(windows))]
        {
            let temp = std::env::var_os("TMPDIR")
                .filter(|t| !t.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            PipeAddress(temp.join(format!("CoreFxPipe_{name}")))
        }
    }

    /// A pipe path (Windows `\\.\pipe\...`) or a Unix socket path, as given.
    pub fn at(path: impl Into<PathBuf>) -> PipeAddress {
        PipeAddress(path.into())
    }

    /// The path the address names.
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

/// Why a request did not come back with its answer.
#[derive(Debug)]
pub enum PipeError {
    /// Nothing listens there, or it did not accept the connection in time.
    Unreachable(String),
    /// The connection broke, or closed before the answer came.
    Broken(String),
    /// No answer with the request's `id` within the timeout.
    TimedOut,
    /// A line that is not a JSON object came back where the answer was expected.
    Malformed(String),
}

impl std::fmt::Display for PipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(why) => {
                write!(f, "the supervisor's pipe could not be reached: {why}")
            }
            Self::Broken(why) => write!(
                f,
                "the supervisor's pipe broke before the answer came: {why}"
            ),
            Self::TimedOut => f.write_str("the supervisor did not answer in time"),
            Self::Malformed(line) => write!(
                f,
                "the supervisor answered with a line that is not a JSON object: {line}"
            ),
        }
    }
}

impl std::error::Error for PipeError {}

/// What the events reader reports.
#[derive(Debug, Clone, PartialEq)]
pub enum PipeSignal {
    /// The held connection is open.
    Connected,
    /// It closed or could not be opened; the reader tries again after a pause.
    Disconnected,
    /// A line the supervisor pushed (an object without an `id`).
    Event(Value),
}

/// A client of one supervisor's pipe.
#[derive(Clone, Debug)]
pub struct PipeClient {
    address: PipeAddress,
    timeout: Duration,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl PipeClient {
    /// A client of the supervisor at `address`; a request waits at most `timeout` for its answer.
    pub fn new(address: PipeAddress, timeout: Duration) -> PipeClient {
        PipeClient { address, timeout }
    }

    /// Sends `{ "id": <fresh>, "cmd": cmd, ...fields }` on a connection of its own and returns the line that echoes the id.
    /// Events that arrive on that connection before the answer are skipped (the events reader sees them on its own).
    pub fn request(&self, cmd: &str, fields: Map<String, Value>) -> Result<Value, PipeError> {
        let id = format!(
            "r{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let mut line = fields;
        line.insert("id".into(), Value::String(id.clone()));
        line.insert("cmd".into(), Value::String(cmd.into()));
        let deadline = Instant::now() + self.timeout;
        let connection = connect(&self.address, deadline)?;
        let mut text = Value::Object(line).to_string();
        text.push('\n');
        // The exchange runs on a thread of its own and the answer comes back over a channel: a pipe handle cannot bound a read
        // on every platform, a channel wait can. On a timeout the thread is left to end when the supervisor closes the
        // connection or answers late (its answer then goes nowhere).
        let (answered, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = answered.send(exchange(connection, &text, &id));
        });
        match answer.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(_) => Err(PipeError::TimedOut),
        }
    }

    /// Reads the supervisor's pushed lines on one held connection, on a thread of its own, until `on_signal` answers false or
    /// the returned handle is stopped. After the connection closes (or cannot be opened) it reports `Disconnected` and tries
    /// again after `retry` (doubling up to 30 s, back to `retry` once connected). Lines with an `id` (answers meant for some
    /// request) and lines that are not JSON objects are skipped.
    pub fn events(
        &self,
        retry: Duration,
        mut on_signal: impl FnMut(PipeSignal) -> bool + Send + 'static,
    ) -> EventsHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let address = self.address.clone();
        let stopped = stop.clone();
        std::thread::spawn(move || {
            let mut pause = retry;
            while !stopped.load(Ordering::SeqCst) {
                let deadline = Instant::now() + Duration::from_secs(5);
                if let Ok(connection) = connect(&address, deadline) {
                    pause = retry;
                    if !on_signal(PipeSignal::Connected) {
                        return;
                    }
                    for line in BufReader::new(connection).lines() {
                        if stopped.load(Ordering::SeqCst) {
                            return;
                        }
                        let Ok(line) = line else { break };
                        match serde_json::from_str::<Value>(line.trim_end()) {
                            Ok(v @ Value::Object(_)) if v.get("id").is_none() => {
                                if !on_signal(PipeSignal::Event(v)) {
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if stopped.load(Ordering::SeqCst) || !on_signal(PipeSignal::Disconnected) {
                    return;
                }
                let until = Instant::now() + pause;
                while Instant::now() < until && !stopped.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                pause = (pause * 2).min(Duration::from_secs(30));
            }
        });
        EventsHandle { stop }
    }
}

/// Stops an events reader. The reader notices between lines and between connection attempts; one blocked on a quiet open
/// connection ends with its next line or when the supervisor closes it.
pub struct EventsHandle {
    stop: Arc<AtomicBool>,
}

impl EventsHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(windows)]
type Connection = std::fs::File;
#[cfg(unix)]
type Connection = std::os::unix::net::UnixStream;

/// Opens a connection, retrying while the supervisor's instances are all busy or between two of them (Windows) until `deadline`.
fn connect(address: &PipeAddress, deadline: Instant) -> Result<Connection, PipeError> {
    loop {
        #[cfg(windows)]
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(address.path());
        #[cfg(unix)]
        let opened = std::os::unix::net::UnixStream::connect(address.path());
        match opened {
            Ok(c) => return Ok(c),
            Err(e) if busy(&e) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => return Err(PipeError::Unreachable(e.to_string())),
        }
    }
}

/// Writes one request line and reads to the line that echoes `id`, skipping pushed events.
fn exchange(mut connection: Connection, text: &str, id: &str) -> Result<Value, PipeError> {
    connection
        .write_all(text.as_bytes())
        .and_then(|()| connection.flush())
        .map_err(|e| PipeError::Broken(e.to_string()))?;
    let mut reader = BufReader::new(connection);
    let mut answer = String::new();
    loop {
        answer.clear();
        match reader.read_line(&mut answer) {
            Ok(0) => return Err(PipeError::Broken("closed before the answer".into())),
            Ok(_) => {}
            Err(e) => return Err(PipeError::Broken(e.to_string())),
        }
        let value: Value = match serde_json::from_str(answer.trim_end()) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                return Err(PipeError::Malformed(
                    answer.trim_end().chars().take(200).collect(),
                ))
            }
        };
        if value.get("id").and_then(Value::as_str) == Some(id) {
            return Ok(value);
        }
    }
}

/// Windows: `ERROR_PIPE_BUSY` — every instance of the pipe is serving a client — or `ERROR_FILE_NOT_FOUND` — a supervisor that
/// just accepted a client has not made its next instance yet (or none listens: then the deadline ends the wait).
fn busy(e: &std::io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(231) | Some(2))
}
