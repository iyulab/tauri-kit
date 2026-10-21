//! A blocking HTTP client for a loopback backend (feature `loopback-client`).

use std::fmt;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

use super::{Endpoint, Fault, Problem};

/// How a [`Client`] behaves.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// How long one request may take, from connecting to the last byte. Default 120 seconds. A
    /// streamed answer ([`Client::get_stream`], [`Client::post_json_stream`]) is held to it until
    /// its status and headers arrive; reading its body has no deadline — close it with
    /// [`Stream::closer`].
    pub request_timeout: Duration,
    /// The most bytes of a response body read. Default 256 MiB.
    pub body_limit: u64,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            request_timeout: Duration::from_secs(120),
            body_limit: 256 * 1024 * 1024,
        }
    }
}

/// An answer from the backend: its status and its body as text. A status outside 2xx is an answer
/// like any other, for the app to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// What the host said about a request it failed, when it said something — see
    /// [`Fault::from_answer`].
    pub fn fault(&self) -> Option<Fault> {
        Fault::from_answer(self.status, &self.body)
    }

    /// The RFC 9457 problem a status outside 2xx carries, as far as its body says — see
    /// [`Problem::from_answer`]. `None` for a 2xx answer.
    pub fn problem(&self) -> Option<Problem> {
        Problem::from_answer(self.status, &self.body)
    }
}

/// Why a request got no answer.
#[derive(Debug)]
pub struct TransportError(String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the backend could not be reached: {}", self.0)
    }
}

impl std::error::Error for TransportError {}

/// An answer read as it arrives — a streamed body such as server-sent events. Read it line by line
/// through [`BufRead`](std::io::BufRead); the body ends when the backend closes it, or when the
/// stream is closed from this side ([`Stream::closer`]).
pub struct Stream {
    /// The answer's status. A status outside 2xx streams its body like any other.
    pub status: u16,
    body: std::io::BufReader<ureq::BodyReader<'static>>,
    connection: Arc<Connection>,
}

impl Stream {
    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// A handle that ends this stream from any thread — when the view that shows it goes away, or
    /// the person stops what it reports. Closing shuts the connection, so the backend sees the
    /// disconnect at once; a read blocked on a quiet stream returns as the end of the body within
    /// about 50 ms, and every later read does too.
    pub fn closer(&self) -> StreamCloser {
        StreamCloser(self.connection.clone())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // The held copy of the socket would otherwise keep the connection open for as long as a
        // closer lives.
        self.connection.shut();
    }
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("status", &self.status)
            .field("closed", &self.connection.is_closed())
            .finish_non_exhaustive()
    }
}

impl std::io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.connection.is_closed() {
            return Ok(0);
        }
        match self.body.read(buf) {
            Err(_) if self.connection.is_closed() => Ok(0),
            read => read,
        }
    }
}

impl std::io::BufRead for Stream {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.connection.is_closed() {
            return Ok(&[]);
        }
        let connection = &self.connection;
        match self.body.fill_buf() {
            Err(_) if connection.is_closed() => Ok(&[]),
            filled => filled,
        }
    }
    fn consume(&mut self, amount: usize) {
        self.body.consume(amount)
    }
}

/// Ends a [`Stream`] from any thread — see [`Stream::closer`]. Closing twice, or after the stream
/// ended by itself, does nothing.
#[derive(Clone)]
pub struct StreamCloser(Arc<Connection>);

impl StreamCloser {
    /// Shuts the stream's connection: the reading side gets the end of the body now.
    pub fn close(&self) {
        self.0.close();
    }

    /// Whether [`close`](StreamCloser::close) was called.
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

impl fmt::Debug for StreamCloser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamCloser")
            .field("closed", &self.0.is_closed())
            .finish()
    }
}

/// A streamed answer's connection, held apart from the reader so that another thread can shut it.
#[derive(Debug, Default)]
struct Connection {
    closed: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

impl Connection {
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.shut();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Shuts the socket both ways, which wakes a read blocked on it, and lets the copy go.
    fn shut(&self) {
        let held = self
            .socket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(socket) = held {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    fn hold(&self, socket: TcpStream) {
        *self
            .socket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(socket);
    }
}

/// Connects a streamed request over plain TCP, keeping a copy of the socket in the stream's
/// [`Connection`] — ureq offers no other way to reach the socket under a body reader. One per
/// stream, so the copy is always that stream's.
#[derive(Debug)]
struct HoldSocket(Arc<Connection>);

impl Connector for HoldSocket {
    type Out = SocketTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<SocketTransport>, ureq::Error> {
        let mut failed = None;
        for addr in details.addrs.iter() {
            let connected = match details.timeout.not_zero() {
                Some(timeout) => TcpStream::connect_timeout(addr, *timeout),
                None => TcpStream::connect(addr),
            };
            match connected {
                Ok(socket) => {
                    if details.config.no_delay() {
                        socket.set_nodelay(true)?;
                    }
                    self.0.hold(socket.try_clone()?);
                    let buffers = LazyBuffers::new(
                        details.config.input_buffer_size(),
                        details.config.output_buffer_size(),
                    );
                    return Ok(Some(SocketTransport {
                        socket,
                        buffers,
                        connection: self.0.clone(),
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(ureq::Error::Timeout(ureq::Timeout::Connect));
                }
                Err(e) => failed = Some(e),
            }
        }
        Err(ureq::Error::Io(failed.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "no address to connect to",
            )
        })))
    }
}

/// A streamed request's TCP connection. It serves one request — the agent around it lives for one
/// stream — so it is never offered back for another.
///
/// It waits for input in short slices and looks at the stream's [`Connection`] between them:
/// shutting a socket does not wake a read blocked on it in another thread on every platform
/// (Windows leaves it waiting until the peer closes), so a close is noticed here within one slice.
#[derive(Debug)]
struct SocketTransport {
    socket: TcpStream,
    buffers: LazyBuffers,
    connection: Arc<Connection>,
}

/// How long one wait for input lasts before the transport looks for a close.
const CLOSE_CHECK: Duration = Duration::from_millis(50);

impl SocketTransport {
    fn timed_out(timeout: NextTimeout, e: std::io::Error) -> ureq::Error {
        match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                ureq::Error::Timeout(timeout.reason)
            }
            _ => ureq::Error::Io(e),
        }
    }
}

impl Transport for SocketTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        use std::io::Write;
        self.socket
            .set_write_timeout(timeout.not_zero().map(|t| *t))?;
        let output = &self.buffers.output()[..amount];
        self.socket
            .write_all(output)
            .map_err(|e| Self::timed_out(timeout, e))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        use std::io::Read;
        let deadline = timeout.not_zero().map(|t| std::time::Instant::now() + *t);
        loop {
            if self.connection.is_closed() {
                return Err(ureq::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    "the stream was closed",
                )));
            }
            let wait = match deadline {
                Some(deadline) => deadline.saturating_duration_since(std::time::Instant::now()),
                None => CLOSE_CHECK,
            };
            if wait.is_zero() {
                return Err(ureq::Error::Timeout(timeout.reason));
            }
            self.socket.set_read_timeout(Some(wait.min(CLOSE_CHECK)))?;
            match self.socket.read(self.buffers.input_append_buf()) {
                Ok(amount) => {
                    self.buffers.input_appended(amount);
                    return Ok(amount > 0);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(e) => return Err(ureq::Error::Io(e)),
            }
        }
    }

    fn is_open(&mut self) -> bool {
        false
    }
}

/// Talks HTTP to a backend on 127.0.0.1: every request carries the token, no proxy is ever used
/// (a system proxy would otherwise see loopback traffic and the token), and a non-2xx status comes
/// back as a [`Response`] rather than an error.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
    authorization: String,
    body_limit: u64,
    request_timeout: Duration,
    timeout: Option<Duration>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for `endpoint`.
    pub fn new(endpoint: &Endpoint, options: &ClientOptions) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(options.request_timeout))
            .build()
            .into();
        Client {
            agent,
            base: endpoint.base(),
            authorization: endpoint.authorization(),
            body_limit: options.body_limit,
            request_timeout: options.request_timeout,
            timeout: None,
        }
    }

    /// The same client, giving up on each request after `timeout` instead of
    /// [`ClientOptions::request_timeout`] — a quick health check, or a backend slow to start a
    /// streamed answer (for a stream it bounds the wait for the status and headers only).
    pub fn within(&self, timeout: Duration) -> Client {
        Client {
            timeout: Some(timeout),
            ..self.clone()
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// GET `path` (starting with `/`).
    pub fn get(&self, path: &str) -> Result<Response, TransportError> {
        let response = self.prepare(self.agent.get(self.url(path))).call();
        self.read(response)
    }

    /// POST to `path` (starting with `/`) with no body.
    pub fn post(&self, path: &str) -> Result<Response, TransportError> {
        let response = self.prepare(self.agent.post(self.url(path))).send_empty();
        self.read(response)
    }

    /// POST `json` to `path` (starting with `/`), as `application/json`.
    pub fn post_json(&self, path: &str, json: &str) -> Result<Response, TransportError> {
        let response = self
            .prepare(self.agent.post(self.url(path)))
            .content_type("application/json")
            .send(json);
        self.read(response)
    }

    /// Sends `method` (`"PUT"`, `"DELETE"`, `"PATCH"`, any HTTP method name) to `path` (starting
    /// with `/`), with `json` as an `application/json` body when given — what a shell relaying its
    /// page's requests needs, whatever verb the page used. A name that is not an HTTP method is a
    /// [`TransportError`]; nothing is sent.
    pub fn send(
        &self,
        method: &str,
        path: &str,
        json: Option<&str>,
    ) -> Result<Response, TransportError> {
        let method = ureq::http::Method::from_bytes(method.as_bytes())
            .map_err(|_| TransportError(format!("not an HTTP method: {method:?}")))?;
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(self.url(path))
            .header("Authorization", &self.authorization);
        let response = match json {
            Some(json) => request
                .header("Content-Type", "application/json")
                .body(json.to_owned())
                .map_err(ureq::Error::from)
                .and_then(|request| self.run(request)),
            None => request
                .body(())
                .map_err(ureq::Error::from)
                .and_then(|request| self.run(request)),
        };
        self.read(response)
    }

    /// GET `path` (starting with `/`) and read the answer as it arrives — an event feed, the shape
    /// a page's `EventSource` opens.
    pub fn get_stream(&self, path: &str) -> Result<Stream, TransportError> {
        let (agent, connection) = self.stream_agent();
        let response = self.prepare_stream(agent.get(self.url(path))).call();
        self.stream(response, connection)
    }

    /// POST `json` to `path` (starting with `/`) and read the answer as it arrives.
    pub fn post_json_stream(&self, path: &str, json: &str) -> Result<Stream, TransportError> {
        let (agent, connection) = self.stream_agent();
        let response = self
            .prepare_stream(agent.post(self.url(path)))
            .content_type("application/json")
            .send(json);
        self.stream(response, connection)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn prepare<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let request = request.header("Authorization", &self.authorization);
        match self.timeout {
            Some(timeout) => request.config().timeout_global(Some(timeout)).build(),
            None => request,
        }
    }

    /// As [`Client::prepare`], for a streamed answer: the timeout holds until the status and headers
    /// arrive, and reading the body has none — an event feed stays quiet for as long as it likes, and
    /// a long answer (a model's reply, a download's progress) is read to its end.
    fn prepare_stream<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let timeout = Some(self.timeout.unwrap_or(self.request_timeout));
        request
            .header("Authorization", &self.authorization)
            .config()
            .timeout_global(None)
            .timeout_connect(timeout)
            .timeout_send_request(timeout)
            .timeout_send_body(timeout)
            .timeout_recv_response(timeout)
            .build()
    }

    /// Runs an http-crate request with this client's timeout, as [`Client::prepare`] does for a builder.
    fn run<S: ureq::AsSendBody>(
        &self,
        request: ureq::http::Request<S>,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        match self.timeout {
            Some(timeout) => {
                let request = self
                    .agent
                    .configure_request(request)
                    .timeout_global(Some(timeout))
                    .build();
                self.agent.run(request)
            }
            None => self.agent.run(request),
        }
    }

    /// An agent of its own for one streamed request — the same settings, its socket held apart.
    fn stream_agent(&self) -> (ureq::Agent, Arc<Connection>) {
        let connection = Arc::new(Connection::default());
        let agent = ureq::Agent::with_parts(
            self.agent.config().clone(),
            HoldSocket(connection.clone()),
            DefaultResolver::default(),
        );
        (agent, connection)
    }

    fn stream(
        &self,
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
        connection: Arc<Connection>,
    ) -> Result<Stream, TransportError> {
        let response = response.map_err(|e| TransportError(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .into_with_config()
            .limit(self.body_limit)
            .reader();
        Ok(Stream {
            status,
            body: std::io::BufReader::new(body),
            connection,
        })
    }

    fn read(
        &self,
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Response, TransportError> {
        let mut response = response.map_err(|e| TransportError(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(self.body_limit)
            .read_to_string()
            .map_err(|e| TransportError(e.to_string()))?;
        Ok(Response { status, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_answer_says_what_failed() {
        let failed = Response {
            status: 500,
            body: r#"{"fault":{"type":"System.IO.IOException","at":"App.Store.Read","frames":["App.Store.Read","App.Api.Get"]}}"#.into(),
        };
        assert_eq!(
            failed.fault(),
            Some(Fault {
                kind: "System.IO.IOException".into(),
                at: Some("App.Store.Read".into()),
                frames: vec!["App.Store.Read".into(), "App.Api.Get".into()],
            })
        );
    }

    #[test]
    fn a_client_never_shows_its_token() {
        let endpoint = Endpoint::new(5100, super::super::Token::generate().unwrap());
        let client = Client::new(&endpoint, &ClientOptions::default());
        assert_eq!(client.base(), "http://127.0.0.1:5100");
        assert!(!format!("{client:?}").contains(endpoint.token().as_str()));
    }
}
