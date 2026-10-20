//! The pipe client against an in-process supervisor: a named pipe on Windows, a Unix socket elsewhere, speaking one JSON object
//! per line. Each connection is greeted with a pushed event; then it either answers requests or closes.
#![cfg(feature = "pipe")]

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use tauri_kit_sidecar::pipe::{PipeAddress, PipeClient, PipeError, PipeSignal};

#[derive(Clone, Copy)]
enum Mode {
    /// Greet, then answer each request (`silent` is never answered; anything else is echoed with its id).
    Answer,
    /// Greet, then close the connection.
    GreetAndClose,
}

fn handle<C: Read + Write>(mut connection: C, mode: Mode) {
    if connection
        .write_all(b"{\"event\":\"hello\"}\n")
        .and_then(|()| connection.flush())
        .is_err()
    {
        return;
    }
    if let Mode::GreetAndClose = mode {
        return;
    }
    let mut reader = BufReader::new(connection);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let request: Value = serde_json::from_str(line.trim_end()).unwrap();
        if request["cmd"] == "silent" {
            continue;
        }
        let answer =
            json!({ "id": request["id"], "cmd": request["cmd"], "echo": request["value"] });
        let out = reader.get_mut();
        if out
            .write_all(format!("{{\"event\":\"in-between\"}}\n{answer}\n").as_bytes())
            .is_err()
        {
            return;
        }
        let _ = out.flush();
    }
}

#[cfg(windows)]
fn serve(name: &str, mode: Mode) -> PipeAddress {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let address = PipeAddress::dotnet(&format!("{name}-{}", std::process::id()));
    let wide: Vec<u16> = address
        .path()
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0])
        .collect();
    let instance = move || {
        // SAFETY: a fresh pipe instance per connection; the handle is owned by the File it becomes.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        assert_ne!(
            handle, INVALID_HANDLE_VALUE,
            "the test pipe could not be made"
        );
        handle
    };
    // The first instance exists before the address is handed out, so a client never races the server's start. Each instance
    // travels as the File that owns its handle (a raw handle is not Send).
    let mut next = unsafe { std::fs::File::from_raw_handle(instance() as _) };
    std::thread::spawn(move || loop {
        use std::os::windows::io::AsRawHandle;
        let raw = next.as_raw_handle();
        let connected = unsafe { ConnectNamedPipe(raw as _, std::ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        let file = std::mem::replace(&mut next, unsafe {
            std::fs::File::from_raw_handle(instance() as _)
        });
        if connected {
            std::thread::spawn(move || handle_and_disconnect(file, mode));
        }
    });
    address
}

#[cfg(windows)]
fn handle_and_disconnect(file: std::fs::File, mode: Mode) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::DisconnectNamedPipe;
    let raw = file.as_raw_handle();
    handle(&file, mode);
    // SAFETY: the handle is still the File's; disconnecting tells the client the pipe closed.
    unsafe { DisconnectNamedPipe(raw as _) };
}

#[cfg(unix)]
fn serve(name: &str, mode: Mode) -> PipeAddress {
    let dir = std::env::temp_dir().join(format!("tk-pipe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let address = PipeAddress::at(dir.join("socket"));
    let listener = std::os::unix::net::UnixListener::bind(address.path()).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || handle(stream, mode));
        }
    });
    address
}

#[test]
fn a_request_gets_the_answer_with_its_id_and_the_events_around_it_are_skipped() {
    let client = PipeClient::new(serve("answer", Mode::Answer), Duration::from_secs(5));
    let mut fields = Map::new();
    fields.insert("value".into(), json!(41));
    let answer = client.request("echo", fields).unwrap();
    assert_eq!(answer["cmd"], "echo");
    assert_eq!(answer["echo"], 41);
    assert!(answer["id"].as_str().is_some_and(|id| !id.is_empty()));
    // a second request is a connection of its own and gets its own id
    let again = client.request("echo", Map::new()).unwrap();
    assert_ne!(again["id"], answer["id"]);
}

#[test]
fn a_request_nobody_answers_times_out() {
    let client = PipeClient::new(serve("silent", Mode::Answer), Duration::from_millis(400));
    let started = Instant::now();
    assert!(matches!(
        client.request("silent", Map::new()),
        Err(PipeError::TimedOut)
    ));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn nothing_listening_is_unreachable() {
    let client = PipeClient::new(
        PipeAddress::dotnet("tk-pipe-nobody-listens-here"),
        Duration::from_millis(300),
    );
    assert!(matches!(
        client.request("status", Map::new()),
        Err(PipeError::Unreachable(_))
    ));
}

#[test]
fn the_events_reader_reports_each_connection_its_events_and_each_loss_then_reconnects() {
    let client = PipeClient::new(serve("events", Mode::GreetAndClose), Duration::from_secs(5));
    let (seen, signals) = mpsc::channel();
    let handle = client.events(Duration::from_millis(50), move |signal| {
        seen.send(signal).is_ok()
    });
    let first: Vec<PipeSignal> = (0..4)
        .map(|_| signals.recv_timeout(Duration::from_secs(10)).unwrap())
        .collect();
    handle.stop();
    assert_eq!(first[0], PipeSignal::Connected);
    assert_eq!(first[1], PipeSignal::Event(json!({ "event": "hello" })));
    assert_eq!(first[2], PipeSignal::Disconnected);
    assert_eq!(
        first[3],
        PipeSignal::Connected,
        "it connects again after the supervisor closed the connection"
    );
}

#[test]
fn a_dotnet_name_maps_to_the_platforms_address() {
    let address = PipeAddress::dotnet("supervisor");
    let path = address.path().to_string_lossy().into_owned();
    if cfg!(windows) {
        assert_eq!(path, r"\\.\pipe\supervisor");
    } else {
        assert!(path.ends_with("/CoreFxPipe_supervisor"));
    }
}
