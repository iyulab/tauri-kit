//! Run a bundled helper process — a sidecar — from a desktop app.
//!
//! Starting a child process is one line. Running one well is not, and the failures are quiet:
//!
//! - **A console window flashes up.** A GUI app on Windows has no console, so a console-subsystem
//!   child gets one of its own unless it is told not to.
//! - **Processes outlive the app.** Killing the child does not kill what *it* started, and when the
//!   app crashes nobody kills anything. [`Sidecar`] puts the child in a Windows job object that the
//!   OS tears down with the app, or in its own process group on Unix, and stops the whole tree.
//! - **A crash looks like a slow start.** Waiting for a sidecar to become ready by polling it alone
//!   keeps waiting after it has died. [`Sidecar::wait_ready`] checks the process between probes and
//!   returns as soon as it exits. A sidecar that announces readiness on stdout — often with the
//!   port it chose — is waited for with [`Sidecar::wait_line`], under the same rules.
//! - **A sidecar that dies stays dead.** Once running, nothing notices it ending, or restarts it
//!   within limits, or stops it in order. [`supervise::Supervisor`] does, from a function that
//!   starts it once.
//! - **Pipes fill up.** A child whose output nobody reads blocks once the pipe buffer is full.
//!   [`Output::Files`] drains both streams on background threads.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use std::{process::Command, time::Duration};
//! use tauri_kit_sidecar::{free_loopback_port, Output, Readiness, Sidecar};
//!
//! let port = free_loopback_port()?;
//! let mut cmd = Command::new("helper");
//! cmd.arg("--port").arg(port.to_string());
//! let mut sidecar = Sidecar::spawn(cmd, Output::Discard)?;
//!
//! match sidecar.wait_ready(Duration::from_secs(30), Duration::from_millis(250), || {
//!     std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
//! })? {
//!     Readiness::Ready => { /* use it */ }
//!     Readiness::Exited(status) => eprintln!("helper exited during startup: {status}"),
//!     Readiness::TimedOut => eprintln!("helper did not become ready"),
//! }
//! sidecar.shutdown(Duration::from_secs(2))?;
//! # Ok(())
//! # }
//! ```
//!
//! **Before an update installs, stop the sidecar yourself.** On Windows the installer replaces the
//! sidecar's program, which the running sidecar holds open, and it starts before the app has
//! exited — so the job object, which ends the sidecar only once the app is gone, is too late. With
//! Tauri's updater plugin that is one call in the hook it runs just before the installer:
//! `app.updater_builder().on_before_exit(move || { /* sidecar.shutdown(…) on the app's own state */ })`.
//! A [`supervise::Supervisor`] is stopped the same way, with [`supervise::Supervisor::stop`], which
//! also keeps it from being started again while the installer runs.

#[cfg(feature = "loopback")]
pub mod loopback;
#[cfg(feature = "pipe")]
pub mod pipe;
pub mod supervise;

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Keeps a child from getting a console window of its own on Windows. No-op elsewhere.
///
/// [`Sidecar::spawn`] applies this itself; it is public for the other processes an app starts.
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// A port on 127.0.0.1 that was free a moment ago, for a sidecar to listen on.
///
/// The port is released before this returns, so another process could take it before the sidecar
/// binds. Pair it with [`Sidecar::wait_ready`], which reports a sidecar that exits because it could
/// not bind, instead of waiting it out.
pub fn free_loopback_port() -> io::Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

/// Where the sidecar's stdout and stderr go.
#[derive(Debug, Clone)]
pub enum Output {
    /// Discarded.
    Discard,
    /// Appended line by line to these files, read on background threads so the child never blocks
    /// on a full pipe. Missing parent directories are created. Lines are written as the child
    /// printed them, line endings included; bytes that are not valid UTF-8 are replaced rather than
    /// dropping the line. If a file cannot be opened, its stream is still drained, into nothing —
    /// a log that cannot be written never stops the sidecar.
    Files { stdout: PathBuf, stderr: PathBuf },
    /// stdout is read line by line for [`Sidecar::wait_line`], and drained on a background thread
    /// for as long as the sidecar runs, so the child never blocks on a full pipe. stdout is also
    /// appended to a file, as with [`Output::Files`] (parent directories included), or not kept —
    /// every line, the ones `wait_line` takes included. stderr goes where its [`Sink`] says.
    Lines {
        stdout: Option<PathBuf>,
        stderr: Sink,
    },
}

/// Where a stream the kit does not read goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Sink {
    /// Discarded.
    #[default]
    Discard,
    /// Appended line by line to this file, as with [`Output::Files`] (parent directories included).
    File(PathBuf),
    /// Written where the app's own stream goes — the terminal the app was started from, while
    /// developing. An app built without a console (a released GUI app) has nowhere to write it,
    /// so there it is as good as discarded: use [`Sink::File`] for a log that outlives the session.
    Inherit,
}

impl From<PathBuf> for Sink {
    fn from(path: PathBuf) -> Self {
        Sink::File(path)
    }
}

impl From<Option<PathBuf>> for Sink {
    /// A file when there is a path, otherwise discarded.
    fn from(path: Option<PathBuf>) -> Self {
        path.map_or(Sink::Discard, Sink::File)
    }
}

/// How waiting for a readiness line ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineReadiness {
    /// The first stdout line the caller accepted, without its line ending.
    Line(String),
    /// The process exited before printing a line the caller accepted.
    Exited(ExitStatus),
    /// Neither happened before the deadline. The process is still running.
    TimedOut,
}

/// How waiting for a sidecar to become ready ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The probe succeeded.
    Ready,
    /// The process exited before the probe succeeded.
    Exited(ExitStatus),
    /// Neither happened before the deadline. The process is still running.
    TimedOut,
}

/// A running sidecar and everything it started.
///
/// Dropping it stops the whole process tree. On Windows the tree is also stopped by the OS when
/// the app itself exits or crashes, because the job object is closed with the app.
///
/// To watch a sidecar for its whole life while any thread may stop it, keep it in a
/// `Mutex<Option<Sidecar>>`: a watcher locks it only for [`Sidecar::try_status`], which does not
/// wait, and whoever stops it takes it out and calls [`Sidecar::shutdown`] outside the lock. The
/// waits ([`Sidecar::wait_ready`], [`Sidecar::wait_line`]) hold the sidecar for as long as they
/// wait, so they belong to startup.
pub struct Sidecar {
    child: Child,
    tree: tree::Tree,
    lines: Option<Lines>,
}

/// stdout lines on their way to [`Sidecar::wait_line`]. Sent only while someone may still wait for
/// one; afterwards the reader drains without keeping anything.
struct Lines {
    receiver: Receiver<String>,
    listening: Arc<AtomicBool>,
}

impl Sidecar {
    /// Starts `cmd` as a sidecar: no console window, output as `output` says, and the child placed
    /// where its whole tree can be stopped together.
    pub fn spawn(mut cmd: Command, output: Output) -> io::Result<Self> {
        hide_console(&mut cmd);
        cmd.stdin(Stdio::null());
        match &output {
            Output::Discard => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
            Output::Files { .. } => {
                cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
            Output::Lines { stderr, .. } => {
                cmd.stdout(Stdio::piped());
                cmd.stderr(match stderr {
                    Sink::Discard => Stdio::null(),
                    Sink::File(_) => Stdio::piped(),
                    Sink::Inherit => Stdio::inherit(),
                });
            }
        }
        tree::prepare(&mut cmd);
        let mut child = cmd.spawn()?;
        let tree = match tree::adopt(&child) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        let mut lines = None;
        match output {
            Output::Discard => {}
            Output::Files { stdout, stderr } => {
                if let Some(out) = child.stdout.take() {
                    drain(out, stdout);
                }
                if let Some(err) = child.stderr.take() {
                    drain(err, stderr);
                }
            }
            Output::Lines { stdout, stderr } => {
                if let Some(out) = child.stdout.take() {
                    lines = Some(read_lines(out, stdout));
                }
                if let (Some(err), Sink::File(path)) = (child.stderr.take(), stderr) {
                    drain(err, path);
                }
            }
        }
        Ok(Self { child, tree, lines })
    }

    /// The operating system's id for the sidecar process.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// The exit status, if the sidecar has exited. Does not wait.
    pub fn try_status(&mut self) -> io::Result<Option<ExitStatus>> {
        tree::exit_status(&mut self.child)
    }

    /// Whether the sidecar is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(tree::exit_status(&mut self.child), Ok(None))
    }

    /// Calls `probe` every `interval` until it returns true, the sidecar exits, or `deadline`
    /// passes — whichever comes first. The process is checked before every probe, so a sidecar
    /// that crashes during startup is reported at once instead of after the deadline.
    pub fn wait_ready(
        &mut self,
        deadline: Duration,
        interval: Duration,
        mut probe: impl FnMut() -> bool,
    ) -> io::Result<Readiness> {
        let until = Instant::now() + deadline;
        loop {
            if let Some(status) = tree::exit_status(&mut self.child)? {
                return Ok(Readiness::Exited(status));
            }
            if probe() {
                return Ok(Readiness::Ready);
            }
            let now = Instant::now();
            if now >= until {
                return Ok(Readiness::TimedOut);
            }
            thread::sleep(interval.min(until - now));
        }
    }

    /// Waits for the first stdout line `accept` returns true for, the sidecar exiting, or `deadline`
    /// passing — whichever comes first. The line is returned the moment it is printed; a sidecar
    /// that exits during startup is reported at once, as with [`Sidecar::wait_ready`].
    ///
    /// Lines `accept` turns down are dropped. Once a line is accepted, the rest of stdout is still
    /// drained but no longer kept. Requires [`Output::Lines`]; otherwise fails with
    /// [`io::ErrorKind::InvalidInput`].
    pub fn wait_line(
        &mut self,
        deadline: Duration,
        mut accept: impl FnMut(&str) -> bool,
    ) -> io::Result<LineReadiness> {
        let Some(lines) = &self.lines else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "wait_line needs Output::Lines",
            ));
        };
        let until = Instant::now() + deadline;
        loop {
            let now = Instant::now();
            if now >= until {
                return Ok(LineReadiness::TimedOut);
            }
            match lines
                .receiver
                .recv_timeout((until - now).min(Duration::from_millis(50)))
            {
                Ok(line) if accept(&line) => {
                    lines.listening.store(false, Ordering::Relaxed);
                    return Ok(LineReadiness::Line(line));
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {}
                // stdout closed: no line will come; only the process's end is left to report.
                Err(RecvTimeoutError::Disconnected) => {
                    thread::sleep(
                        Duration::from_millis(20)
                            .min(until.saturating_duration_since(Instant::now())),
                    );
                }
            }
            if let Some(status) = tree::exit_status(&mut self.child)? {
                // Lines printed just before exiting may still be queued.
                while let Ok(line) = lines.receiver.try_recv() {
                    if accept(&line) {
                        lines.listening.store(false, Ordering::Relaxed);
                        return Ok(LineReadiness::Line(line));
                    }
                }
                return Ok(LineReadiness::Exited(status));
            }
        }
    }

    /// Stops the sidecar and everything it started.
    ///
    /// Waits up to `grace` for the sidecar to exit on its own — an app that has its own way of
    /// asking the sidecar to quit (a shutdown request, closing its stdin) asks first, then calls
    /// this. On Unix the process group is sent `SIGTERM` at the start of the grace period. Whatever
    /// is still running afterwards is killed, the whole tree at once.
    pub fn shutdown(mut self, grace: Duration) -> io::Result<ExitStatus> {
        tree::request_stop(&self.child);
        let until = Instant::now() + grace;
        while Instant::now() < until {
            if tree::exit_status(&mut self.child)?.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // Even when the sidecar itself exited, what it started may not have.
        self.tree.kill(&mut self.child)?;
        self.child.wait()
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.tree.kill(&mut self.child);
        let _ = self.child.wait();
    }
}

/// How a [`run`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// The process exited in time; what it printed is all here.
    Exited(std::process::Output),
    /// The deadline passed first. The process and everything it started were stopped.
    TimedOut,
}

/// Runs `cmd` to the end, up to `deadline`, and hands back its exit status and everything it
/// printed — for a helper that does one job and exits, such as a converter or a renderer.
///
/// The same care as [`Sidecar`]: no console window, stdin closed, and the child placed where its
/// whole tree can be stopped. Both streams are read on background threads, so a chatty child never
/// blocks on a full pipe while it is waited for. When the deadline passes, the whole tree is
/// stopped — not only the child, whose own children would otherwise live on and keep its output
/// pipes open — and the run returns at once.
///
/// What it printed is kept in memory: for output that is a report, not a stream of data.
pub fn run(mut cmd: Command, deadline: Duration) -> io::Result<Ran> {
    hide_console(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tree::prepare(&mut cmd);
    let mut child = cmd.spawn()?;
    let tree = match tree::adopt(&child) {
        Ok(tree) => tree,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let stdout = child.stdout.take().map(collect);
    let stderr = child.stderr.take().map(collect);

    let until = Instant::now() + deadline;
    let status = loop {
        if let Some(status) = tree::exit_status(&mut child)? {
            break Some(status);
        }
        if Instant::now() >= until {
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Exited or not, what it started may still be running and holding the pipes open.
    tree.kill(&mut child)?;
    let status = match status {
        Some(_) => child.wait()?,
        None => {
            child.wait()?;
            return Ok(Ran::TimedOut);
        }
    };
    let take = |collected: Option<Receiver<Vec<u8>>>| {
        collected
            .and_then(|r| r.recv_timeout(Duration::from_secs(1)).ok())
            .unwrap_or_default()
    };
    Ok(Ran::Exited(std::process::Output {
        status,
        stdout: take(stdout),
        stderr: take(stderr),
    }))
}

/// Reads a stream to its end on a background thread; the bytes arrive once it closes.
fn collect(mut stream: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    receiver
}

/// The file a stream is appended to, its folder created first. `None` when it cannot be opened.
fn append_to(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(dir);
    }
    File::options().create(true).append(true).open(path).ok()
}

fn drain(stream: impl Read + Send + 'static, path: PathBuf) {
    thread::spawn(move || {
        let Some(mut file) = append_to(&path) else {
            // Nowhere to write: keep reading so the child never blocks on a full pipe.
            let _ = io::copy(&mut BufReader::new(stream), &mut io::sink());
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        while matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0) {
            let _ = file.write_all(String::from_utf8_lossy(&line).as_bytes());
            line.clear();
        }
    });
}

fn read_lines(stream: impl Read + Send + 'static, log: Option<PathBuf>) -> Lines {
    let (sender, receiver) = mpsc::channel();
    let listening = Arc::new(AtomicBool::new(true));
    let still = Arc::clone(&listening);
    thread::spawn(move || {
        let mut file = log.as_deref().and_then(append_to);
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        while matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0) {
            let text = String::from_utf8_lossy(&line);
            if let Some(file) = file.as_mut() {
                let _ = file.write_all(text.as_bytes());
            }
            if still.load(Ordering::Relaxed) {
                // A dropped receiver only means nobody waits any more; keep draining.
                let _ = sender.send(text.trim_end_matches(['\r', '\n']).to_owned());
            }
            line.clear();
        }
    });
    Lines {
        receiver,
        listening,
    }
}

#[cfg(windows)]
mod tree {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command, ExitStatus};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job object holding the sidecar and everything it starts. Closing the handle — including
    /// when the app exits or crashes — terminates them all.
    pub struct Tree(HANDLE);

    // The handle is an owned kernel object usable from any thread.
    unsafe impl Send for Tree {}
    unsafe impl Sync for Tree {}

    pub fn prepare(_cmd: &mut Command) {}

    /// Assigns the running child to a new job. Anything it starts afterwards joins the job too; a
    /// process it starts in the few microseconds before the assignment would not — closing that
    /// gap needs the child created suspended, which `std::process::Command` does not offer.
    pub fn adopt(child: &Child) -> io::Result<Tree> {
        // SAFETY: plain Win32 calls on handles owned here; every failure path closes the job.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let tree = Tree(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(tree)
        }
    }

    /// Windows has no signal to ask a process to exit; the app asks through its own channel.
    pub fn request_stop(_child: &Child) {}

    /// The child's handle stays open until it is waited on, so its id cannot be reused meanwhile.
    pub fn exit_status(child: &mut Child) -> io::Result<Option<ExitStatus>> {
        child.try_wait()
    }

    impl Tree {
        pub fn kill(&self, _child: &mut Child) -> io::Result<()> {
            // SAFETY: the job handle is valid for the lifetime of `self`.
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: closing the handle this value owns, once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(unix)]
mod tree {
    use std::io;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Command, ExitStatus};

    /// The sidecar leads its own process group, so the group id is its pid.
    pub struct Tree;

    pub fn prepare(cmd: &mut Command) {
        // No PR_SET_PDEATHSIG on Linux: it fires when the *thread* that spawned the child exits,
        // not the process, so a sidecar started from a worker thread would be killed the moment
        // that thread finished.
        cmd.process_group(0);
    }

    pub fn adopt(_child: &Child) -> io::Result<Tree> {
        Ok(Tree)
    }

    /// Whether the sidecar has exited, without reaping it.
    ///
    /// Reaping frees its pid — and with it the process-group id the tree is stopped through — for
    /// the system to hand to an unrelated process. Left unreaped, the id stays reserved until
    /// `Child::wait` after the group has been killed.
    pub fn exit_status(child: &mut Child) -> io::Result<Option<ExitStatus>> {
        // SAFETY: an all-zero siginfo_t is valid; waitid only writes into it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waiting on our own child, without consuming its exit (WNOWAIT).
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            // Already reaped by `Child` itself: it kept the status.
            if err.raw_os_error() == Some(libc::ECHILD) {
                return child.try_wait();
            }
            return Err(err);
        }
        // SAFETY: filled in by a successful waitid for a child that changed state.
        let (pid, status) = unsafe { (info.si_pid(), info.si_status()) };
        if pid == 0 {
            return Ok(None); // still running (WNOHANG)
        }
        // Re-encode as the wait status ExitStatus is built from.
        let raw = match info.si_code {
            libc::CLD_EXITED => (status & 0xff) << 8,
            libc::CLD_DUMPED => status | 0x80,
            _ => status,
        };
        Ok(Some(ExitStatus::from_raw(raw)))
    }

    /// Whether the sidecar has exited, or does within a moment.
    fn leader_exits(child: &mut Child) -> io::Result<bool> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if exit_status(child)?.is_some() {
                return Ok(true);
            }
            if std::time::Instant::now() >= until {
                return Ok(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub fn request_stop(child: &Child) {
        // SAFETY: signalling a process group we created; failure (already gone) is harmless.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGTERM) };
    }

    impl Tree {
        pub fn kill(&self, child: &mut Child) -> io::Result<()> {
            // SAFETY: as above.
            let rc = unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
            if rc != 0 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    // The whole group is already gone.
                    Some(libc::ESRCH) => {}
                    // macOS and the BSDs answer EPERM, not ESRCH, when no member of the group
                    // can be signalled — every one is a zombie, or already on its way out. That is
                    // what the group is once the sidecar has exited and is kept unreaped (see
                    // `exit_status`), and also for the moment just after a SIGTERM, when the
                    // sidecar is exiting but its exit is not visible yet. A member still running
                    // as us would have been signalled instead, so EPERM with a leader that has
                    // exited — or does within a moment — means there is nothing left to stop.
                    Some(libc::EPERM) if leader_exits(child)? => {}
                    _ => return Err(err),
                }
            }
            Ok(())
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod tree {
    use std::io;
    use std::process::{Child, Command, ExitStatus};

    pub struct Tree;

    pub fn prepare(_cmd: &mut Command) {}

    pub fn adopt(_child: &Child) -> io::Result<Tree> {
        Ok(Tree)
    }

    pub fn request_stop(_child: &Child) {}

    pub fn exit_status(child: &mut Child) -> io::Result<Option<ExitStatus>> {
        child.try_wait()
    }

    impl Tree {
        pub fn kill(&self, child: &mut Child) -> io::Result<()> {
            match child.kill() {
                Err(e) if e.kind() != io::ErrorKind::InvalidInput => Err(e),
                _ => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_loopback_port_is_nonzero_and_bindable() {
        // The port was free a moment ago, nothing more: a test running alongside may take it before
        // this binds (as the documentation says), so one of a few tries has to bind.
        let bound = (0..5).any(|_| {
            let port = free_loopback_port().unwrap();
            assert_ne!(port, 0);
            TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
        });
        assert!(bound, "no port it gave was bindable");
    }
}
