//! A blocking HTTP client for a loopback backend (feature `loopback-client`).

use std::fmt;
use std::time::Duration;

use super::{Endpoint, Fault};

/// How a [`Client`] behaves.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// How long one request may take, from connecting to the last byte. Default 120 seconds.
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
/// through [`BufRead`](std::io::BufRead); the body ends when the backend closes it.
pub struct Stream {
    /// The answer's status. A status outside 2xx streams its body like any other.
    pub status: u16,
    body: std::io::BufReader<ureq::BodyReader<'static>>,
}

impl Stream {
    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl std::io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.body.read(buf)
    }
}

impl std::io::BufRead for Stream {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.body.fill_buf()
    }
    fn consume(&mut self, amount: usize) {
        self.body.consume(amount)
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
            timeout: None,
        }
    }

    /// The same client, giving up on each request after `timeout` instead of
    /// [`ClientOptions::request_timeout`] — a quick health check, or a long streamed answer.
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

    /// POST `json` to `path` (starting with `/`) and read the answer as it arrives.
    pub fn post_json_stream(&self, path: &str, json: &str) -> Result<Stream, TransportError> {
        let response = self
            .prepare(self.agent.post(self.url(path)))
            .content_type("application/json")
            .send(json)
            .map_err(|e| TransportError(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .into_with_config()
            .limit(self.body_limit)
            .reader();
        Ok(Stream {
            status,
            body: std::io::BufReader::new(body),
        })
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
