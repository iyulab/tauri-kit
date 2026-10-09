//! Finding a loopback backend that lives on its own, from the file it publishes.

use std::fmt;
use std::path::Path;

use super::{Endpoint, Token};

/// Why a discovery file gave no backend to call.
#[derive(Debug)]
pub enum DiscoveryError {
    /// There is no file: no backend has published one, or the last one stopped cleanly.
    Missing,
    /// The file could not be read or is not a discovery record.
    Unreadable(String),
    /// The process that published it is gone: it did not stop cleanly.
    Stale,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("no backend has published where it is"),
            Self::Unreadable(why) => write!(f, "the discovery file could not be read: {why}"),
            Self::Stale => {
                f.write_str("the backend that published the discovery file is not running")
            }
        }
    }
}

impl std::error::Error for DiscoveryError {}

/// The record the .NET side writes (`DiscoveryFile.Publish` in `TauriKit.Sidecar.Loopback`).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    port: u16,
    token: String,
    pid: u32,
    /// Milliseconds since the Unix epoch.
    started_at: i64,
}

/// The backend the discovery file at `path` names — when the file is there, is a record, and the
/// process that wrote it still runs. Read it again after a 401: every start of the backend issues a
/// new token.
///
/// A process id is reused once its process is gone. On Windows the process's start time must match
/// the record's too; elsewhere only the id is checked, and a reused one shows up as a refused
/// connection instead.
pub fn discover(path: &Path) -> Result<Endpoint, DiscoveryError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(DiscoveryError::Missing),
        Err(e) => return Err(DiscoveryError::Unreadable(e.to_string())),
    };
    let record: Record =
        serde_json::from_str(&text).map_err(|e| DiscoveryError::Unreadable(e.to_string()))?;
    let token = Token::from_secret(record.token)
        .ok_or_else(|| DiscoveryError::Unreadable("the token is too short".into()))?;
    if record.port == 0 {
        return Err(DiscoveryError::Unreadable("the port is 0".into()));
    }
    if !running(record.pid, record.started_at) {
        return Err(DiscoveryError::Stale);
    }
    Ok(Endpoint::new(record.port, token))
}

/// Whether process `pid`, started at `started_at` (Unix milliseconds), still runs. A process that
/// is there but not ours to inspect cannot be shown dead, so it counts as running.
#[cfg(windows)]
fn running(pid: u32, started_at: i64) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the handle is checked before use and closed once; the out-parameters are locals.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == STILL_ACTIVE as u32;
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        let timed =
            GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        CloseHandle(process);
        if !alive {
            return false;
        }
        if !timed {
            return true;
        }
        // FILETIME counts 100 ns since 1601-01-01; the record, milliseconds since 1970-01-01.
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        let millis = (ticks / 10_000) as i64 - 11_644_473_600_000;
        (millis - started_at).abs() < 2_000
    }
}

#[cfg(unix)]
fn running(pid: u32, _started_at: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(windows, unix)))]
fn running(_pid: u32, _started_at: i64) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("backend.json");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn folder(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tauri-kit-discover-{name}-{}", std::process::id()))
    }

    fn started_at_of_this_process() -> i64 {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::FILETIME;
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            // SAFETY: the pseudo-handle needs no closing; the out-parameters are locals.
            unsafe {
                GetProcessTimes(
                    GetCurrentProcess(),
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user,
                )
            };
            let ticks =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            (ticks / 10_000) as i64 - 11_644_473_600_000
        }
        #[cfg(not(windows))]
        0
    }

    #[test]
    fn a_published_backend_is_found() {
        let token = "a".repeat(43);
        let dir = folder("found");
        let path = write(
            &dir,
            &format!(
                r#"{{"port":5100,"token":"{token}","pid":{},"startedAt":{}}}"#,
                std::process::id(),
                started_at_of_this_process()
            ),
        );
        let endpoint = discover(&path).unwrap();
        assert_eq!(endpoint.port(), 5100);
        assert_eq!(endpoint.base(), "http://127.0.0.1:5100");
        assert_eq!(endpoint.authorization(), format!("Bearer {token}"));
        assert!(!format!("{endpoint:?}").contains(&token));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_file_and_a_damaged_file_are_told_apart() {
        let dir = folder("damaged");
        assert!(matches!(
            discover(&dir.join("backend.json")),
            Err(DiscoveryError::Missing)
        ));
        let path = write(&dir, "{ not json");
        assert!(matches!(
            discover(&path),
            Err(DiscoveryError::Unreadable(_))
        ));
        let short = write(
            &dir,
            &format!(
                r#"{{"port":5100,"token":"short","pid":{},"startedAt":0}}"#,
                std::process::id()
            ),
        );
        assert!(matches!(
            discover(&short),
            Err(DiscoveryError::Unreadable(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_reused_process_id_is_stale() {
        let dir = folder("reused");
        // This process's id with a start time it never had: a later process that reused the id.
        let path = write(
            &dir,
            &format!(
                r#"{{"port":5100,"token":"{}","pid":{},"startedAt":978307200000}}"#,
                "a".repeat(43),
                std::process::id()
            ),
        );
        assert!(matches!(discover(&path), Err(DiscoveryError::Stale)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dead_process_is_stale() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        drop(child);
        let dir = folder("dead");
        let path = write(
            &dir,
            &format!(
                r#"{{"port":5100,"token":"{}","pid":{pid},"startedAt":0}}"#,
                "a".repeat(43)
            ),
        );
        assert!(matches!(discover(&path), Err(DiscoveryError::Stale)));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
