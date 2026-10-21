//! The loopback arrangement against a real child process: this test binary, started again as a
//! small HTTP server that checks the token it was handed.
#![cfg(feature = "loopback-client")]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
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

/// One request as a test backend read it: the request line, the headers (names lower-cased), and the
/// body. The body is read too, always — a server that answers and closes with request bytes still
/// unread makes Windows reset the connection, and the client sees the reset instead of the answer.
struct Request {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&String> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Request {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap();
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    let mut request = Request {
        line,
        headers,
        body: Vec::new(),
    };
    let length = request
        .header("content-length")
        .map_or(0, |n| n.parse().unwrap());
    request.body = vec![0; length];
    std::io::Read::read_exact(reader, &mut request.body).unwrap();
    request
}

/// Answers each request with its path, or 401 when it lacks the token.
fn serve() {
    let token = std::env::var(TOKEN_ENV).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    println!("{PREFIX}{}", listener.local_addr().unwrap().port());
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let request = read_request(&mut reader);
        let authorized = request.header("authorization") == Some(&format!("Bearer {token}"));
        let path = request.line.split_whitespace().nth(1).unwrap_or("");
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
        let method = request.line.split_whitespace().next().unwrap_or("");
        let (status, text) = if authorized && path == "/method" {
            (
                "200 OK",
                format!("{method} {}", String::from_utf8_lossy(&request.body)),
            )
        } else if authorized {
            (
                "200 OK",
                format!("{path} {}", String::from_utf8_lossy(&request.body)),
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
fn any_method_goes_out_as_named_with_the_token_and_its_body() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let client = helper.client();
    let put = client.send("PUT", "/method", Some(r#"{"a":1}"#)).unwrap();
    assert_eq!((put.status, put.body.as_str()), (200, r#"PUT {"a":1}"#));
    let deleted = client.send("DELETE", "/method", None).unwrap();
    assert_eq!(deleted.body, "DELETE ");
    let patched = client
        .within(Duration::from_secs(5))
        .send("PATCH", "/method", Some("{}"))
        .unwrap();
    assert_eq!(patched.body, "PATCH {}");
    assert!(client.send("NOT A METHOD", "/method", None).is_err());
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
        let request = read_request(&mut reader).line;
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

/// A backend in this process that answers one streamed request: after `head_delay`, the status
/// and headers, then `events` events `gap` apart, then the end of the body.
fn paced_feed(head_delay: Duration, events: usize, gap: Duration) -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = Endpoint::new(
        listener.local_addr().unwrap().port(),
        Token::generate().unwrap(),
    );
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut BufReader::new(stream.try_clone().unwrap()));
        std::thread::sleep(head_delay);
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n"
        );
        for _ in 0..events {
            let _ = write!(stream, "9\r\ndata: 1\n\n\r\n");
            let _ = stream.flush();
            std::thread::sleep(gap);
        }
        let _ = write!(stream, "0\r\n\r\n");
    });
    let options = ClientOptions {
        request_timeout: Duration::from_millis(500),
        ..ClientOptions::default()
    };
    Client::new(&endpoint, &options)
}

#[test]
fn a_stream_that_outlasts_the_request_timeout_is_read_to_its_end() {
    let stream = paced_feed(Duration::ZERO, 6, Duration::from_millis(250))
        .get_stream("/events")
        .unwrap();
    let events: Vec<String> = stream
        .lines()
        .map(Result::unwrap)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(events.len(), 6);
}

#[test]
fn a_stream_whose_answer_never_starts_gives_up_after_the_request_timeout() {
    let started = Instant::now();
    let opened = paced_feed(Duration::from_secs(5), 1, Duration::ZERO).get_stream("/events");
    assert!(opened.is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// A backend in this process that answers one request with the request's own headers as its body
/// (one `name: value` line each, lower-cased) and a `Session-Id: s-1` header.
fn header_echo() -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = Endpoint::new(
        listener.local_addr().unwrap().port(),
        Token::generate().unwrap(),
    );
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut BufReader::new(stream.try_clone().unwrap()));
        let seen: String = request
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {}\n", value.to_ascii_lowercase()))
            .collect();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nSession-Id: s-1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{seen}",
            seen.len()
        )
        .unwrap();
    });
    Client::new(&endpoint, &ClientOptions::default())
}

#[test]
fn headers_go_out_with_the_request_and_come_back_with_the_answer() {
    let answer = header_echo()
        .send_with_headers(
            "POST",
            "/rpc",
            Some("{}"),
            &[
                ("Accept", "application/json, text/event-stream"),
                ("Session-Id", "s-0"),
            ],
        )
        .unwrap();
    assert!(answer
        .body
        .contains("accept: application/json, text/event-stream\n"));
    assert!(answer.body.contains("session-id: s-0\n"));
    assert!(answer.body.contains("authorization: bearer "));
    assert_eq!(answer.header("session-id"), Some("s-1"));
    assert_eq!(answer.header("SESSION-ID"), Some("s-1"));
    assert_eq!(answer.header("x-absent"), None);
}

#[test]
fn a_caller_cannot_replace_the_token_or_send_a_malformed_header() {
    let client = header_echo();
    assert!(client
        .send_with_headers("GET", "/x", None, &[("authorization", "Bearer other")])
        .is_err());
    assert!(client
        .send_with_headers("GET", "/x", None, &[("bad name", "v")])
        .is_err());
    assert!(client
        .send_with_headers("GET", "/x", None, &[("x-ok", "line\nbreak")])
        .is_err());
}
