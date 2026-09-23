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
//!   returns as soon as it exits.
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

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
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
    /// on a full pipe. Bytes that are not valid UTF-8 are replaced rather than dropping the line.
    Files { stdout: PathBuf, stderr: PathBuf },
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
pub struct Sidecar {
    child: Child,
    tree: tree::Tree,
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
        if let Output::Files { stdout, stderr } = output {
            if let Some(out) = child.stdout.take() {
                drain(out, stdout);
            }
            if let Some(err) = child.stderr.take() {
                drain(err, stderr);
            }
        }
        Ok(Self { child, tree })
    }

    /// The operating system's id for the sidecar process.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// The exit status, if the sidecar has exited. Does not wait.
    pub fn try_status(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Whether the sidecar is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
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
            if let Some(status) = self.child.try_wait()? {
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
            if self.child.try_wait()?.is_some() {
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

fn drain(stream: impl Read + Send + 'static, path: PathBuf) {
    thread::spawn(move || {
        let Ok(mut file) = File::options().create(true).append(true).open(&path) else {
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

#[cfg(windows)]
mod tree {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command};
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
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    /// The sidecar leads its own process group, so the group id is its pid.
    pub struct Tree;

    pub fn prepare(cmd: &mut Command) {
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        // SAFETY: prctl is async-signal-safe and touches only the calling (child) process.
        unsafe {
            cmd.pre_exec(|| {
                // If the app dies without cleaning up, the kernel stops the sidecar.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }

    pub fn adopt(_child: &Child) -> io::Result<Tree> {
        Ok(Tree)
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
                // ESRCH: the whole group is already gone.
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(err);
                }
            }
            Ok(())
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod tree {
    use std::io;
    use std::process::{Child, Command};

    pub struct Tree;

    pub fn prepare(_cmd: &mut Command) {}

    pub fn adopt(_child: &Child) -> io::Result<Tree> {
        Ok(Tree)
    }

    pub fn request_stop(_child: &Child) {}

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
        let port = free_loopback_port().unwrap();
        assert_ne!(port, 0);
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    }
}
