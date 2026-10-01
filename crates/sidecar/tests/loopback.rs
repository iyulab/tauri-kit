//! The loopback arrangement against a real child process: this test binary, started again as a
//! small HTTP server that checks the token it was handed.
#![cfg(feature = "loopback")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

use tauri_kit_sidecar::loopback::{Loopback, LoopbackOptions, StartError};

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
fn a_request_without_the_token_is_refused() {
    let helper = Loopback::start(helper("serve"), &options()).unwrap();
    let other = tauri_kit_sidecar::loopback::Client::new(
        helper
            .client()
            .base()
            .rsplit(':')
            .next()
            .unwrap()
            .parse()
            .unwrap(),
        &tauri_kit_sidecar::loopback::Token::generate().unwrap(),
        &options(),
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
