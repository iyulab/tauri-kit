//! The loopback arrangement against a real child process: this test binary, started again as a
//! small HTTP server that checks the token it was handed.
#![cfg(feature = "loopback-client")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tauri_kit_sidecar::loopback::{
    Client, ClientOptions, Endpoint, Loopback, LoopbackOptions, StartError, Token,
};

const TOKEN_ENV: &str = "LOOPBACK_TEST_TOKEN";
const PREFIX: &str = "loopback-test ready port=";

/// What the child does, chosen by `LOOPBACK_TEST_MODE`; the parent tests skip this one.
#[test]
fn child() {
    let Ok(mode) = std::env::var("LOOPBACK_TEST_MODE") else {
        return;
    };
    match mode.as_str() {
        "serve" => serve(),
        "exit" => std::process::exit(3),
        "bad-line" => println!("{PREFIX}not-a-port"),
        "silent" => std::thread::sleep(Duration::from_secs(60)),
        _ => unreachable!(),
    }
}

/// Answers each request with its path, or 401 when it lacks the token.
fn serve() {
    let token = std::env::var(TOKEN_ENV).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    println!("{PREFIX}{}", listener.local_addr().unwrap().port());
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        let (mut authorized, mut length) = (false, 0usize);
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let (name, value) = header.split_once(':').unwrap();
            match name.to_ascii_lowercase().as_str() {
                "authorization" => authorized = value.trim() == format!("Bearer {token}"),
                "content-length" => length = value.trim().parse().unwrap(),
                _ => {}
            }
        }
        let mut body = vec![0; length];
        std::io::Read::read_exact(&mut reader, &mut body).unwrap();
        let path = request.split_whitespace().nth(1).unwrap_or("");
        if authorized && path == "/slow" {
            std::thread::sleep(Duration::from_secs(3));
        }
        if authorized && path == "/stream" {
            // No length: the body is whatever arrives until the connection closes.
            write!(stream, "HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n").unwrap();
            for n in 1..=3 {
                write!(stream, "data: {n}\n\n").unwrap();
                stream.flush().unwrap();
            }
            continue;
        }
        let (status, text) = if authorized {
            (
                "200 OK",
                format!("{path} {}", String::from_utf8_lossy(&body)),
            )
        } else {
            ("401 Unauthorized", String::new())
        };
        write!(
            stream,
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
            text.len()
        )
        .unwrap();
    }
}

/// The child, quiet so that the harness prints nothing on the line the readiness line goes on.
fn helper(mode: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "child",
        "--exact",
        "--nocapture",
        "--quiet",
        "--test-threads=1",
    ])
    .env("LOOPBACK_TEST_MODE", mode);
    cmd
}

fn options() -> LoopbackOptions {
    let mut options = LoopbackOptions::new(TOKEN_ENV, PREFIX);
    options.ready_timeout = Duration::from_secs(20);
    options
}

#[test]
fn requests_carry_the_token_and_statuses_come_back_as_answers() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let got = helper.client().get("/items").unwrap();
    assert_eq!((got.status, got.body.as_str()), (200, "/items "));
    let posted = helper.client().post_json("/items", r#"{"a":1}"#).unwrap();
    assert_eq!(posted.body, r#"/items {"a":1}"#);
    assert!(posted.is_success());
}

#[test]
fn the_whole_readiness_line_and_the_endpoint_are_kept() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let port = helper.endpoint().port();
    assert_eq!(helper.ready_line(), format!("{PREFIX}{port}"));
    assert_eq!(helper.endpoint().base(), format!("http://127.0.0.1:{port}"));
}

#[test]
fn the_parts_keep_working_apart() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let (mut sidecar, endpoint) = helper.into_parts();
    assert!(sidecar.is_running());
    let client = Client::new(&endpoint, &ClientOptions::default());
    assert_eq!(client.get("/items").unwrap().status, 200);
    sidecar.shutdown(Duration::ZERO).unwrap();
}

#[test]
fn an_empty_post_carries_the_token_too() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let posted = helper.client().post("/shutdown").unwrap();
    assert_eq!((posted.status, posted.body.as_str()), (200, "/shutdown "));
}

#[test]
fn a_streamed_answer_is_read_as_it_arrives() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let stream = helper.client().post_json_stream("/stream", "{}").unwrap();
    assert!(stream.is_success());
    let events: Vec<String> = stream
        .lines()
        .map(Result::unwrap)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(events, ["data: 1", "data: 2", "data: 3"]);
}

#[test]
fn a_streamed_get_is_read_as_it_arrives() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let stream = helper.client().get_stream("/stream").unwrap();
    assert!(stream.is_success());
    let events: Vec<String> = stream
        .lines()
        .map(Result::unwrap)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(events, ["data: 1", "data: 2", "data: 3"]);
}

/// A backend in this process that sends one event and then says nothing, reporting when the
/// client goes away.
fn quiet_feed() -> (Client, mpsc::Receiver<(String, Instant)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = Endpoint::new(
        listener.local_addr().unwrap().port(),
        Token::generate().unwrap(),
    );
    let (gone, went) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header.trim_end().is_empty() {
                break;
            }
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n"
        )
        .unwrap();
        write!(stream, "9\r\ndata: 1\n\n\r\n").unwrap();
        stream.flush().unwrap();
        // Nothing more until the client disconnects.
        let _ = std::io::Read::read(&mut reader, &mut [0; 64]);
        let _ = gone.send((request, Instant::now()));
        // Hold the connection open: the client's read must end by its own close, not this one.
        std::thread::sleep(Duration::from_secs(3));
        drop(stream);
    });
    (Client::new(&endpoint, &ClientOptions::default()), went)
}

#[test]
fn closing_a_quiet_stream_from_another_thread_ends_the_read_and_the_connection_at_once() {
    let (client, went) = quiet_feed();
    let mut stream = client.get_stream("/events").unwrap();
    let mut first = String::new();
    stream.read_line(&mut first).unwrap();
    assert_eq!(first, "data: 1\n");

    let closer = stream.closer();
    let closing = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let at = Instant::now();
        closer.close();
        closer.close(); // twice does nothing more
        at
    });
    let mut rest = String::new();
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).unwrap() == 0 {
            break;
        }
        rest.push_str(&line);
    }
    let ended = Instant::now();
    let closed_at = closing.join().unwrap();
    assert_eq!(rest, "\n");
    assert!(
        ended.duration_since(closed_at) < Duration::from_millis(500),
        "read ended {:?} after close",
        ended.duration_since(closed_at)
    );
    let (request, gone_at) = went.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(request.starts_with("GET /events "), "{request}");
    assert!(gone_at.duration_since(closed_at) < Duration::from_millis(500));
    assert!(stream.closer().is_closed());
    assert_eq!(stream.read_line(&mut String::new()).unwrap(), 0);
}

#[test]
fn dropping_a_stream_lets_the_backend_go_even_while_a_closer_lives() {
    let (client, went) = quiet_feed();
    let stream = client.get_stream("/events").unwrap();
    let closer = stream.closer();
    drop(stream);
    assert!(went.recv_timeout(Duration::from_secs(2)).is_ok());
    assert!(!closer.is_closed());
}

#[test]
fn a_client_within_a_shorter_time_gives_up_on_a_slow_answer() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let started = std::time::Instant::now();
    assert!(helper
        .client()
        .within(Duration::from_millis(500))
        .get("/slow")
        .is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn stdout_is_kept_in_a_file_from_the_readiness_line_on() {
    let dir = std::env::temp_dir().join(format!("loopback-stdout-{}", std::process::id()));
    let log = dir.join("out.log");
    let mut options = options();
    options.stdout = Some(log.clone());
    let helper = Loopback::start(helper("serve"), &options).unwrap();
    let _ = helper.client().get("/items");
    drop(helper);
    let kept = std::fs::read_to_string(&log).unwrap();
    assert!(kept.contains(PREFIX), "{kept}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_request_without_the_token_is_refused() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let other = Client::new(
        &Endpoint::new(helper.endpoint().port(), Token::generate().unwrap()),
        &ClientOptions::default(),
    );
    let refused = other.get("/items").unwrap();
    assert_eq!(refused.status, 401);
    assert!(!refused.is_success());
}

#[test]
fn a_sidecar_that_exits_before_announcing_is_reported_at_once() {
    match Loopback::start(helper("exit"), &options()) {
        Err(StartError::Exited(status)) => assert!(!status.success()),
        other => panic!("expected Exited, got {:?}", other.err()),
    }
}

#[test]
fn a_readiness_line_without_a_port_is_reported() {
    match Loopback::start(helper("bad-line"), &options()) {
        Err(StartError::BadReadyLine(line)) => assert!(line.ends_with("not-a-port")),
        other => panic!("expected BadReadyLine, got {:?}", other.err()),
    }
}

#[test]
fn no_readiness_line_in_time_is_reported() {
    let mut options = options();
    options.ready_timeout = Duration::from_millis(1500);
    match Loopback::start(helper("silent"), &options) {
        Err(StartError::TimedOut(after)) => assert_eq!(after, Duration::from_millis(1500)),
        other => panic!("expected TimedOut, got {:?}", other.err()),
    }
}
