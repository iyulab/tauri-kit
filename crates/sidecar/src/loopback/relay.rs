//! A web page's way to a loopback backend (feature `relay`): the page asks in HTTP's shape, the app
//! answers through [`Relay::handle`], and the backend's credential never reaches the page.
//!
//! A desktop app that keeps its backend apart from its window wants the page to call that backend
//! as if it were a web server, without handing page script the bearer token — anything in the page
//! can read what the page holds. The usual arrangement is a custom URI scheme the app registers with
//! its web view: the page fetches `<scheme>://<host>/<path>`, the app makes the call itself with the
//! credential, and hands the answer back. [`Relay`] is that call, written once:
//!
//! - **The credential stays in the app.** It is read from the app's [`Endpoint`] source — a
//!   discovery file, a child the app started — and attached to each call.
//! - **Only what the app lists goes through.** Each route is a method and a path template
//!   ([`RelayBuilder::allow`]); everything else is refused before any call is made, and so is a
//!   request from a page origin the app did not name ([`RelayBuilder::origin`]).
//! - **A backend that restarted is found again.** Every start of a backend may give it a new port
//!   and a new credential: a 401 reads the source once more and repeats the call, and a call that
//!   reached nothing makes the next one read it again.
//! - **Long answers stream.** A route allowed with [`RelayBuilder::allow_stream`] — server-sent
//!   events, a model's reply — answers with a [`Body::Stream`] the app reads as it arrives.
//! - **A refusal says why, the standard way.** The relay's own refusals are RFC 9457 problems
//!   (`application/problem+json`) with a `reason` the page can act on ([`reason`]); the backend's
//!   own answers, failed or not, pass through as they are.
//!
//! The relay holds no web view type: requests and responses are the `http` crate's, which Tauri's
//! URI scheme handlers take and give. A stream is the app's to carry — a Tauri app sends its chunks
//! down an IPC channel, or a response body that streams where the web view allows one.
//!
//! ```no_run
//! # fn main() {
//! use std::path::PathBuf;
//! use std::time::Duration;
//! use http::Method;
//! use tauri_kit_sidecar::loopback::{discover, relay::Relay};
//!
//! let discovery = PathBuf::from("run/backend.json");
//! let relay = Relay::builder(move || discover(&discovery))
//!     .origin("http://tauri.localhost")
//!     .allow(Method::GET, "/api/items")
//!     .allow(Method::PUT, "/api/items/{id}")
//!     .allow(Method::GET, "/api/search?q&limit")
//!     .allow_stream(Method::GET, "/api/events")
//!     .timeout_for(Method::POST, "/api/import", Duration::from_secs(300))
//!     .allow(Method::POST, "/api/import")
//!     .build();
//! // In the app's URI scheme handler: `let response = relay.handle(request);`
//! # let _ = relay;
//! # }
//! ```

use std::fmt;
use std::io::{BufRead, Read};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{header, HeaderValue, Method, Request, Response, StatusCode};

use super::client::RawResponse;
use super::{Client, ClientOptions, Endpoint, Stream, StreamCloser, TransportError};

/// The `reason` values of the relay's own refusals — a page tells them apart from the backend's
/// answers by these.
pub mod reason {
    /// The request came from a page origin the app did not name, or carried none.
    pub const ORIGIN_REFUSED: &str = "origin-refused";
    /// The method and path are not on the app's list.
    pub const ROUTE_REFUSED: &str = "route-refused";
    /// The backend could not be reached: not running, not yet ready, or its address unknown.
    pub const BACKEND_UNREACHABLE: &str = "backend-unreachable";
    /// The backend was reached but gave no answer in time.
    pub const BACKEND_TIMEOUT: &str = "backend-timeout";
}

/// Why a relayed call got no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Unreachable,
    Timeout,
}

impl From<TransportError> for Failure {
    fn from(error: TransportError) -> Failure {
        if error.is_timeout() {
            Failure::Timeout
        } else {
            Failure::Unreachable
        }
    }
}

/// Where the relay learns the backend's address and credential, called on first use and again after
/// a 401 or a call that reached nothing.
type Source = dyn Fn() -> Result<Endpoint, String> + Send + Sync;

#[derive(Debug, Clone)]
struct Route {
    method: Method,
    template: String,
    stream: bool,
}

/// Builds a [`Relay`] — see [`Relay::builder`].
pub struct RelayBuilder {
    source: Arc<Source>,
    origins: Vec<String>,
    routes: Vec<Route>,
    timeouts: Vec<(Method, String, Duration)>,
    options: ClientOptions,
}

impl fmt::Debug for RelayBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayBuilder")
            .field("origins", &self.origins)
            .field("routes", &self.routes)
            .finish_non_exhaustive()
    }
}

impl RelayBuilder {
    /// A page origin the relay answers, exactly as the web view sends it in `Origin`
    /// (`http://tauri.localhost`, `tauri://localhost`, a development server's). A request from any
    /// other origin, or with none, is refused; with no origin named, every request is.
    pub fn origin(mut self, origin: impl Into<String>) -> Self {
        self.origins.push(origin.into());
        self
    }

    /// Lets `method` reach `template`. A template is a path matched exactly, except that a segment
    /// written `{name}` stands for any one segment — never empty, never `.` or `..` (encoded or
    /// not); an encoded `/` (`%2F`) stays inside the segment it is in. A `?key&key` tail names the
    /// query keys the route takes: a query with any other key is refused, and without the tail a
    /// route takes no query.
    pub fn allow(mut self, method: Method, template: impl Into<String>) -> Self {
        self.routes.push(Route {
            method,
            template: template.into(),
            stream: false,
        });
        self
    }

    /// As [`RelayBuilder::allow`], for a route whose answer is read as it arrives: a 2xx answer is a
    /// [`Body::Stream`]. Any other status is read whole, as a refusal is.
    pub fn allow_stream(mut self, method: Method, template: impl Into<String>) -> Self {
        self.routes.push(Route {
            method,
            template: template.into(),
            stream: true,
        });
        self
    }

    /// How long `method` on `template` (written as for [`RelayBuilder::allow`]) may take instead of
    /// [`ClientOptions::request_timeout`] — a request the backend is known to answer slowly. For a
    /// streamed route it bounds the wait for the answer's status and headers.
    pub fn timeout_for(
        mut self,
        method: Method,
        template: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        self.timeouts.push((method, template.into(), timeout));
        self
    }

    /// The client settings each call is made with. Default [`ClientOptions::default`].
    pub fn client_options(mut self, options: ClientOptions) -> Self {
        self.options = options;
        self
    }

    /// The relay.
    pub fn build(self) -> Relay {
        let methods = {
            let mut names: Vec<&str> = Vec::new();
            for route in &self.routes {
                if !names.contains(&route.method.as_str()) {
                    names.push(route.method.as_str());
                }
            }
            names.join(", ")
        };
        Relay {
            source: self.source,
            origins: self.origins,
            routes: self.routes,
            timeouts: self.timeouts,
            options: self.options,
            methods,
            client: Mutex::new(None),
        }
    }
}

/// Relays a page's requests to a loopback backend — see the [module](self) documentation.
pub struct Relay {
    source: Arc<Source>,
    origins: Vec<String>,
    routes: Vec<Route>,
    timeouts: Vec<(Method, String, Duration)>,
    options: ClientOptions,
    methods: String,
    client: Mutex<Option<Client>>,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay")
            .field("origins", &self.origins)
            .field("routes", &self.routes)
            .finish_non_exhaustive()
    }
}

/// A relayed answer's body: whole, or read as it arrives.
pub enum Body {
    /// The whole body.
    Full(Vec<u8>),
    /// A body still arriving — a route allowed with [`RelayBuilder::allow_stream`]. Each item is
    /// what arrived since the last; the iterator ends with the body. [`RelayStream::closer`] ends it
    /// from another thread.
    Stream(RelayStream),
}

impl Body {
    /// The whole body, reading a stream to its end.
    pub fn into_bytes(self) -> std::io::Result<Vec<u8>> {
        match self {
            Body::Full(bytes) => Ok(bytes),
            Body::Stream(mut stream) => {
                let mut bytes = Vec::new();
                stream.stream.read_to_end(&mut bytes)?;
                Ok(bytes)
            }
        }
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Body::Full(bytes) => f.debug_tuple("Full").field(&bytes.len()).finish(),
            Body::Stream(stream) => f.debug_tuple("Stream").field(stream).finish(),
        }
    }
}

/// A streamed answer's body, chunk by chunk as it arrives.
#[derive(Debug)]
pub struct RelayStream {
    stream: Stream,
}

impl RelayStream {
    /// Ends the stream from any thread — when the page that asked goes away.
    pub fn closer(&self) -> StreamCloser {
        self.stream.closer()
    }
}

impl Iterator for RelayStream {
    type Item = std::io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        let chunk = match self.stream.fill_buf() {
            Ok([]) => return None,
            Ok(chunk) => chunk.to_vec(),
            Err(e) => return Some(Err(e)),
        };
        self.stream.consume(chunk.len());
        Some(Ok(chunk))
    }
}

impl Relay {
    /// A relay that learns the backend's address and credential from `source` — called on first
    /// use, and again after a 401 or a call that reached nothing (a backend that restarted). An
    /// app whose backend publishes a discovery file passes `move || discover(&path)`.
    pub fn builder<F, E>(source: F) -> RelayBuilder
    where
        F: Fn() -> Result<Endpoint, E> + Send + Sync + 'static,
        E: fmt::Display,
    {
        RelayBuilder {
            source: Arc::new(move || source().map_err(|e| e.to_string())),
            origins: Vec::new(),
            routes: Vec::new(),
            timeouts: Vec::new(),
            options: ClientOptions::default(),
        }
    }

    /// Answers one request of the page: the path and query of its URI (scheme and host are the
    /// app's, and ignored), its method, its body and `Content-Type`/`Accept`. A preflight
    /// (`OPTIONS`) from a named origin is answered here. The backend's answer comes back with its
    /// status, its `Content-Type` and its body; every answer to a named origin carries the CORS
    /// headers that let the page read it, and none is cached.
    pub fn handle(&self, request: Request<Vec<u8>>) -> Response<Body> {
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .filter(|o| self.origins.iter().any(|named| named == o))
            .map(str::to_owned);
        let Some(origin) = origin else {
            return self.refuse(
                None,
                StatusCode::FORBIDDEN,
                reason::ORIGIN_REFUSED,
                "This page may not reach the backend.",
            );
        };
        if request.method() == Method::OPTIONS {
            return self.answer(
                Some(&origin),
                StatusCode::NO_CONTENT,
                None,
                Body::Full(Vec::new()),
            );
        }
        let path = request
            .uri()
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/");
        let Some(route) = self
            .routes
            .iter()
            .find(|r| r.method == request.method() && route_matches(&r.template, path))
        else {
            return self.refuse(
                Some(&origin),
                StatusCode::FORBIDDEN,
                reason::ROUTE_REFUSED,
                "This request is not one the page may make.",
            );
        };
        let timeout = self
            .timeouts
            .iter()
            .find(|(m, t, _)| m == request.method() && route_matches(t, path))
            .map(|(_, _, d)| *d);
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        let accept = request
            .headers()
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok());
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(accept) = accept {
            headers.push(("Accept", accept));
        }
        let body = (!request.body().is_empty()).then(|| {
            (
                content_type.unwrap_or("application/octet-stream"),
                request.body().as_slice(),
            )
        });
        let call = Call {
            method: request.method().as_str(),
            path,
            body,
            headers: &headers,
            timeout,
        };
        if route.stream {
            match self.attempt(|client| call.stream(client)) {
                Ok(Streamed::Open(stream, content_type)) => self.answer(
                    Some(&origin),
                    StatusCode::from_u16(stream.status).unwrap_or(StatusCode::BAD_GATEWAY),
                    content_type.as_deref(),
                    Body::Stream(RelayStream { stream }),
                ),
                Ok(Streamed::Whole(raw)) => self.pass(&origin, raw),
                Err(failure) => self.no_answer(&origin, failure),
            }
        } else {
            match self.attempt(|client| call.send(client)) {
                Ok(raw) => self.pass(&origin, raw),
                Err(failure) => self.no_answer(&origin, failure),
            }
        }
    }

    /// Makes the call with the cached client, once more with a fresh read of the source after a
    /// 401 (the backend answered without acting on it). A call that reached nothing drops the cached
    /// client, so the next one reads the source again; it is not repeated, as the backend may have
    /// acted on it before the connection broke.
    fn attempt<T: Answered>(
        &self,
        call: impl Fn(&Client) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let first = self.client(false).and_then(|client| call(&client));
        let answer = match first {
            Ok(answer) if answer.status() == 401 => {
                self.client(true).and_then(|client| call(&client))
            }
            other => other,
        };
        if answer.as_ref().err() == Some(&Failure::Unreachable) {
            if let Ok(mut slot) = self.client.lock() {
                *slot = None;
            }
        }
        answer
    }

    fn client(&self, fresh: bool) -> Result<Client, Failure> {
        let mut slot = self.client.lock().map_err(|_| Failure::Unreachable)?;
        if fresh || slot.is_none() {
            let endpoint = (self.source)().map_err(|_| Failure::Unreachable)?;
            *slot = Some(Client::new(&endpoint, &self.options));
        }
        slot.clone().ok_or(Failure::Unreachable)
    }

    fn pass(&self, origin: &str, raw: RawResponse) -> Response<Body> {
        let content_type = raw
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone());
        self.answer(
            Some(origin),
            StatusCode::from_u16(raw.status).unwrap_or(StatusCode::BAD_GATEWAY),
            content_type.as_deref(),
            Body::Full(raw.body),
        )
    }

    fn no_answer(&self, origin: &str, failure: Failure) -> Response<Body> {
        match failure {
            Failure::Unreachable => self.refuse(
                Some(origin),
                StatusCode::SERVICE_UNAVAILABLE,
                reason::BACKEND_UNREACHABLE,
                "The backend is not running.",
            ),
            Failure::Timeout => self.refuse(
                Some(origin),
                StatusCode::GATEWAY_TIMEOUT,
                reason::BACKEND_TIMEOUT,
                "The backend did not answer in time.",
            ),
        }
    }

    fn refuse(
        &self,
        origin: Option<&str>,
        status: StatusCode,
        reason: &str,
        detail: &str,
    ) -> Response<Body> {
        let problem = serde_json::json!({
            "type": "about:blank",
            "title": status.canonical_reason().unwrap_or("Error"),
            "status": status.as_u16(),
            "detail": detail,
            "reason": reason,
        });
        self.answer(
            origin,
            status,
            Some("application/problem+json"),
            Body::Full(problem.to_string().into_bytes()),
        )
    }

    fn answer(
        &self,
        origin: Option<&str>,
        status: StatusCode,
        content_type: Option<&str>,
        body: Body,
    ) -> Response<Body> {
        let mut builder = Response::builder()
            .status(status)
            .header(header::CACHE_CONTROL, "no-store");
        if let Some(content_type) = content_type.and_then(|v| HeaderValue::from_str(v).ok()) {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(origin) = origin.and_then(|v| HeaderValue::from_str(v).ok()) {
            builder = builder
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, self.methods.as_str())
                .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "content-type, accept")
                .header(header::VARY, "Origin");
        }
        builder.body(body).expect("the relay's headers are valid")
    }
}

/// One relayed request, as the backend gets it.
struct Call<'a> {
    method: &'a str,
    path: &'a str,
    body: Option<(&'a str, &'a [u8])>,
    headers: &'a [(&'a str, &'a str)],
    timeout: Option<Duration>,
}

enum Streamed {
    Open(Stream, Option<String>),
    Whole(RawResponse),
}

trait Answered {
    fn status(&self) -> u16;
}

impl Answered for RawResponse {
    fn status(&self) -> u16 {
        self.status
    }
}

impl Answered for Streamed {
    fn status(&self) -> u16 {
        match self {
            Streamed::Open(stream, _) => stream.status,
            Streamed::Whole(raw) => raw.status,
        }
    }
}

impl Call<'_> {
    fn client(&self, client: &Client) -> Client {
        match self.timeout {
            Some(timeout) => client.within(timeout),
            None => client.clone(),
        }
    }

    fn send(&self, client: &Client) -> Result<RawResponse, Failure> {
        Ok(self
            .client(client)
            .exchange(self.method, self.path, self.body, self.headers)?)
    }

    fn stream(&self, client: &Client) -> Result<Streamed, Failure> {
        let mut stream =
            self.client(client)
                .exchange_stream(self.method, self.path, self.body, self.headers)?;
        if stream.is_success() {
            let content_type = stream.header("content-type").map(str::to_owned);
            return Ok(Streamed::Open(stream, content_type));
        }
        let mut body = Vec::new();
        stream
            .read_to_end(&mut body)
            .map_err(|_| Failure::Unreachable)?;
        Ok(Streamed::Whole(RawResponse {
            status: stream.status,
            headers: stream.headers.clone(),
            body,
        }))
    }
}

/// Whether `path` (with its query) is one `template` names — see [`RelayBuilder::allow`].
fn route_matches(template: &str, path: &str) -> bool {
    let (template_path, template_query) = split_query(template);
    let (request_path, request_query) = split_query(path);
    if request_path.contains('#') {
        return false;
    }
    let want: Vec<&str> = template_path.split('/').collect();
    let got: Vec<&str> = request_path.split('/').collect();
    if want.len() != got.len() {
        return false;
    }
    for (w, g) in want.iter().zip(&got) {
        let is_param = w.len() > 2 && w.starts_with('{') && w.ends_with('}');
        if is_param {
            if !is_one_segment(g) {
                return false;
            }
        } else if w != g {
            return false;
        }
    }
    match (request_query, template_query) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(query), Some(allowed)) => {
            let allowed: Vec<&str> = allowed.split('&').collect();
            let keys: Vec<Option<String>> = query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| decode(pair.split('=').next().unwrap_or("")))
                .collect();
            !keys.is_empty()
                && keys
                    .iter()
                    .all(|key| key.as_deref().is_some_and(|k| allowed.contains(&k)))
        }
    }
}

fn split_query(path: &str) -> (&str, Option<&str>) {
    match path.find('?') {
        Some(at) => (&path[..at], Some(&path[at + 1..])),
        None => (path, None),
    }
}

fn is_one_segment(segment: &str) -> bool {
    match decode(segment) {
        Some(decoded) => !segment.is_empty() && decoded != "." && decoded != "..",
        None => false, // a malformed escape is a refusal
    }
}

/// Percent-decoding as `decodeURIComponent` does it (`+` stays `+`); `None` for a malformed escape
/// or invalid UTF-8.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
