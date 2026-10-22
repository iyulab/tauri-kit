//! The relay against a backend on this machine: one in-process HTTP server per test, each with a
//! credential of its own, so a "restart" is a second server with a new port and token.

#![cfg(feature = "relay")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use http::{header, Method, Request, StatusCode};
use tauri_kit_sidecar::loopback::relay::{reason, Body, Relay};
use tauri_kit_sidecar::loopback::{Endpoint, Token};

const PAGE: &str = "http://tauri.localhost";

/// A backend that answers every request carrying its token; `retired` makes it answer 401 to all —
/// what a backend that restarted with a new credential looks like to a caller holding the old one.
struct Backend {
    endpoint: Endpoint,
    retired: Arc<AtomicBool>,
}

fn backend() -> Backend {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let token = Token::generate().unwrap();
    let bearer = token.bearer();
    let retired = Arc::new(AtomicBool::new(false));
    let gone = retired.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let bearer = bearer.clone();
            let gone = gone.clone();
            thread::spawn(move || answer(stream, &bearer, &gone));
        }
    });
    Backend {
        endpoint: Endpoint::new(port, token),
        retired,
    }
}

fn answer(mut stream: TcpStream, bearer: &str, retired: &AtomicBool) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        let h = h.trim_end().to_owned();
        if h.is_empty() {
            break;
        }
        let (name, value) = h.split_once(':').unwrap();
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let get = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let length: usize = get("content-length").parse().unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();

    if retired.load(Ordering::SeqCst) || get("authorization") != bearer {
        reply(&mut stream, "401 Unauthorized", "text/plain", b"");
        return;
    }
    match target.split('?').next().unwrap_or("") {
        "/api/events" => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
            )
            .unwrap();
            for n in 1..=3 {
                write!(stream, "data: {n}\n\n").unwrap();
                stream.flush().unwrap();
                thread::sleep(Duration::from_millis(30));
            }
        }
        "/api/slow" => {
            thread::sleep(Duration::from_secs(2));
            reply(&mut stream, "200 OK", "text/plain", b"late");
        }
        "/api/missing" | "/api/events-missing" => reply(
            &mut stream,
            "404 Not Found",
            "application/problem+json",
            br#"{"title":"Not Found","status":404,"reason":"no-such-item"}"#,
        ),
        _ => {
            let mut echo = format!(
                "{method} {target} type={} accept={} body=",
                get("content-type"),
                get("accept")
            )
            .into_bytes();
            echo.extend_from_slice(&body);
            reply(&mut stream, "200 OK", "application/octet-stream", &echo);
        }
    }
}

fn reply(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}

/// A relay whose source is whatever endpoint the test puts in `slot`.
fn relay_on(slot: Arc<Mutex<Option<Endpoint>>>) -> Relay {
    Relay::builder(move || slot.lock().unwrap().clone().ok_or("no backend published"))
        .origin(PAGE)
        .allow(Method::GET, "/api/items")
        .allow(Method::PUT, "/api/items/{id}")
        .allow(Method::GET, "/api/search?q&limit")
        .allow(Method::GET, "/api/missing")
        .allow(Method::GET, "/api/slow")
        .timeout_for(Method::GET, "/api/slow", Duration::from_millis(300))
        .allow_stream(Method::GET, "/api/events")
        .allow_stream(Method::GET, "/api/events-missing")
        .build()
}

fn relay_to(backend: &Backend) -> Relay {
    relay_on(Arc::new(Mutex::new(Some(backend.endpoint.clone()))))
}

fn request(method: Method, path: &str, origin: Option<&str>, body: &[u8]) -> Request<Vec<u8>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("app://backend{path}"));
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(body.to_vec()).unwrap()
}

fn bytes(body: Body) -> Vec<u8> {
    body.into_bytes().unwrap()
}

fn text(body: Body) -> String {
    String::from_utf8(bytes(body)).unwrap()
}

#[test]
fn an_allowed_request_reaches_the_backend_with_its_credential_and_comes_back_readable_by_the_page()
{
    let backend = backend();
    let relay = relay_to(&backend);
    let r = relay.handle(request(Method::GET, "/api/items", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], PAGE);
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        r.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    assert!(text(r.into_body()).starts_with("GET /api/items "));
}

#[test]
fn a_body_goes_out_as_bytes_with_its_type_and_the_answer_comes_back_as_bytes() {
    let backend = backend();
    let relay = relay_to(&backend);
    // Not UTF-8: the relay passes what it is given without reading it as text.
    let payload = [0xFFu8, 0x00, 0xC3, 0x28];
    let mut req = request(Method::PUT, "/api/items/a%2Fb", Some(PAGE), &payload);
    req.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );
    req.headers_mut()
        .insert(header::ACCEPT, "application/json".parse().unwrap());
    let r = relay.handle(req);
    assert_eq!(r.status(), StatusCode::OK);
    let answer = bytes(r.into_body());
    let mut expected =
        b"PUT /api/items/a%2Fb type=application/octet-stream accept=application/json body="
            .to_vec();
    expected.extend_from_slice(&payload);
    assert_eq!(answer, expected);
}

#[test]
fn a_request_from_another_origin_or_none_is_refused_as_a_problem_before_any_call() {
    // No backend at all: a refusal must not depend on reaching one.
    let relay = relay_on(Arc::new(Mutex::new(None)));
    for origin in [None, Some("https://example.com")] {
        let r = relay.handle(request(Method::GET, "/api/items", origin, b""));
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert!(r
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
        let problem: serde_json::Value = serde_json::from_str(&text(r.into_body())).unwrap();
        assert_eq!(problem["reason"], reason::ORIGIN_REFUSED);
        assert_eq!(problem["status"], 403);
    }
}

#[test]
fn a_preflight_from_the_page_is_answered_with_the_methods_the_routes_take() {
    let relay = relay_on(Arc::new(Mutex::new(None)));
    let r = relay.handle(request(Method::OPTIONS, "/api/items/1", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        r.headers()[header::ACCESS_CONTROL_ALLOW_METHODS],
        "GET, PUT"
    );
    assert_eq!(r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], PAGE);
}

#[test]
fn only_the_listed_methods_paths_and_query_keys_go_through() {
    let backend = backend();
    let relay = relay_to(&backend);
    let refused = [
        (Method::POST, "/api/items"),         // a method the route does not take
        (Method::GET, "/api/other"),          // a path not listed
        (Method::GET, "/api/items?q=1"),      // a query on a route that takes none
        (Method::GET, "/api/search?q=1&x=2"), // a key the route does not name
        (Method::PUT, "/api/items/.."),       // a segment that climbs
        (Method::PUT, "/api/items/%2e%2E"),   // the same, encoded
        (Method::PUT, "/api/items/"),         // an empty segment
        (Method::PUT, "/api/items/a/b"),      // two segments for one
        (Method::PUT, "/api/items/%zz"),      // a malformed escape
    ];
    for (method, path) in refused {
        let r = relay.handle(request(method.clone(), path, Some(PAGE), b""));
        assert_eq!(r.status(), StatusCode::FORBIDDEN, "{method} {path}");
        // The page may read why.
        assert_eq!(r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], PAGE);
        let problem: serde_json::Value = serde_json::from_str(&text(r.into_body())).unwrap();
        assert_eq!(problem["reason"], reason::ROUTE_REFUSED, "{method} {path}");
    }
    for path in ["/api/search?q=a", "/api/search?q=a&limit=5", "/api/search"] {
        let r = relay.handle(request(Method::GET, path, Some(PAGE), b""));
        assert_eq!(r.status(), StatusCode::OK, "{path}");
    }
}

#[test]
fn a_backend_that_restarted_with_a_new_credential_is_found_again_on_the_401() {
    let first = backend();
    let slot = Arc::new(Mutex::new(Some(first.endpoint.clone())));
    let relay = relay_on(slot.clone());
    assert_eq!(
        relay
            .handle(request(Method::GET, "/api/items", Some(PAGE), b""))
            .status(),
        StatusCode::OK
    );
    // The restart: the old credential stops working, the new backend is published.
    first.retired.store(true, Ordering::SeqCst);
    let second = backend();
    *slot.lock().unwrap() = Some(second.endpoint.clone());
    let r = relay.handle(request(Method::GET, "/api/items", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::OK);
}

#[test]
fn a_backend_that_cannot_be_reached_is_a_503_and_the_next_call_looks_again() {
    let slot = Arc::new(Mutex::new(None));
    let relay = relay_on(slot.clone());
    let r = relay.handle(request(Method::GET, "/api/items", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let problem: serde_json::Value = serde_json::from_str(&text(r.into_body())).unwrap();
    assert_eq!(problem["reason"], reason::BACKEND_UNREACHABLE);

    // A port with nothing behind it: published, then gone.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    *slot.lock().unwrap() = Some(Endpoint::new(closed, Token::generate().unwrap()));
    let r = relay.handle(request(Method::GET, "/api/items", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);

    let backend = backend();
    *slot.lock().unwrap() = Some(backend.endpoint.clone());
    let r = relay.handle(request(Method::GET, "/api/items", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::OK);
}

#[test]
fn a_route_given_less_time_answers_504_when_the_backend_is_slower() {
    let backend = backend();
    let relay = relay_to(&backend);
    let r = relay.handle(request(Method::GET, "/api/slow", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::GATEWAY_TIMEOUT);
    let problem: serde_json::Value = serde_json::from_str(&text(r.into_body())).unwrap();
    assert_eq!(problem["reason"], reason::BACKEND_TIMEOUT);
    // A timeout is not a lost backend: the next call uses the same one.
    assert_eq!(
        relay
            .handle(request(Method::GET, "/api/items", Some(PAGE), b""))
            .status(),
        StatusCode::OK
    );
}

#[test]
fn the_backends_own_failure_passes_through_as_it_is() {
    let backend = backend();
    let relay = relay_to(&backend);
    let r = relay.handle(request(Method::GET, "/api/missing", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        r.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert!(text(r.into_body()).contains("no-such-item"));
}

#[test]
fn a_stream_route_answers_with_a_body_read_as_it_arrives() {
    let backend = backend();
    let relay = relay_to(&backend);
    let r = relay.handle(request(Method::GET, "/api/events", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::CONTENT_TYPE], "text/event-stream");
    let Body::Stream(stream) = r.into_body() else {
        panic!("a stream route answers with a stream")
    };
    let mut all = Vec::new();
    let mut chunks = 0;
    for chunk in stream {
        all.extend(chunk.unwrap());
        chunks += 1;
    }
    assert!(chunks >= 1);
    assert_eq!(
        String::from_utf8(all).unwrap(),
        "data: 1\n\ndata: 2\n\ndata: 3\n\n"
    );
}

#[test]
fn a_stream_route_that_fails_answers_with_the_whole_failure() {
    let backend = backend();
    let relay = relay_to(&backend);
    let r = relay.handle(request(Method::GET, "/api/events-missing", Some(PAGE), b""));
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let Body::Full(body) = r.into_body() else {
        panic!("a failed stream is read whole")
    };
    assert!(String::from_utf8(body).unwrap().contains("no-such-item"));
}

#[test]
fn a_stream_can_be_closed_from_another_thread() {
    let backend = backend();
    let relay = relay_to(&backend);
    let r = relay.handle(request(Method::GET, "/api/events", Some(PAGE), b""));
    let Body::Stream(stream) = r.into_body() else {
        panic!("a stream route answers with a stream")
    };
    let closer = stream.closer();
    closer.close();
    assert!(closer.is_closed());
    assert_eq!(stream.count(), 0);
}
