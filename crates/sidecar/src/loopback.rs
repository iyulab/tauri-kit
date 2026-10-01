//! A sidecar that serves HTTP on the loopback interface, and the app talking to it.
//!
//! The arrangement most such helpers end up with: the app makes a fresh random token and hands it
//! to the sidecar in an environment variable; the sidecar listens on 127.0.0.1 on a port the system
//! picks, says which on its first line of stdout, and answers only requests that carry the token as
//! a bearer credential. Another program on the same computer can reach the port, but not without
//! the token.
//!
//! The variable's name, the readiness line's prefix and every path are the app's — they arrive as
//! [`LoopbackOptions`]. The .NET host side of the same arrangement is the `TauriKit.Sidecar.Loopback`
//! package in this repository's `dotnet/` folder.
//!
//! ```no_run
//! # fn main() -> Result<(), tauri_kit_sidecar::loopback::StartError> {
//! use std::process::Command;
//! use tauri_kit_sidecar::loopback::{Loopback, LoopbackOptions};
//!
//! let helper = Loopback::start(Command::new("helper"), &LoopbackOptions::new("HELPER_TOKEN", "helper ready port="))?;
//! let response = helper.client().post_json("/items", r#"{"name":"a"}"#).expect("the helper answers");
//! assert!(response.is_success(), "{}", response.body);
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::process::Command;
use std::time::Duration;

use crate::{LineReadiness, Output, Sidecar};

/// A fresh random secret for one sidecar run: 32 bytes from the operating system, as 64 hex digits.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A new token from the operating system's random source.
    pub fn generate() -> std::io::Result<Token> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Token(secret.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// The token itself, to hand to the sidecar.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `Authorization` header value that carries it.
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

/// Never printed: a token in a log is a token anyone reading the log can use.
impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

/// The port a readiness line announces: what follows `prefix`, as a port number.
pub fn ready_port(line: &str, prefix: &str) -> Option<u16> {
    line.strip_prefix(prefix)?.trim().parse().ok()
}

/// How to start a loopback sidecar and talk to it.
#[derive(Debug, Clone)]
pub struct LoopbackOptions {
    /// The environment variable the token is handed over in.
    pub token_env: String,
    /// What the readiness line starts with; the port follows it.
    pub ready_prefix: String,
    /// How long to wait for the readiness line. Default 30 seconds.
    pub ready_timeout: Duration,
    /// How long one request may take, from connecting to the last byte. Default 120 seconds.
    pub request_timeout: Duration,
    /// The most bytes of a response body read. Default 256 MiB.
    pub body_limit: u64,
    /// Where the sidecar's stderr is appended, if anywhere.
    pub stderr: Option<std::path::PathBuf>,
}

impl LoopbackOptions {
    /// Options with the defaults, for a token in `token_env` and a readiness line starting with
    /// `ready_prefix`.
    pub fn new(token_env: impl Into<String>, ready_prefix: impl Into<String>) -> Self {
        LoopbackOptions {
            token_env: token_env.into(),
            ready_prefix: ready_prefix.into(),
            ready_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(120),
            body_limit: 256 * 1024 * 1024,
            stderr: None,
        }
    }
}

/// Why a loopback sidecar did not start.
#[derive(Debug)]
pub enum StartError {
    /// The process could not be started, or the token could not be made.
    Spawn(std::io::Error),
    /// It exited before announcing its port.
    Exited(std::process::ExitStatus),
    /// It printed no readiness line in time. It has been stopped.
    TimedOut(Duration),
    /// The line that started with the prefix did not end in a port.
    BadReadyLine(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start: {e}"),
            Self::Exited(status) => write!(f, "exited with {status} before announcing its port"),
            Self::TimedOut(after) => write!(f, "no readiness line within {} s", after.as_secs()),
            Self::BadReadyLine(line) => write!(f, "unexpected readiness line: {line}"),
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(e) => Some(e),
            _ => None,
        }
    }
}

/// A running loopback sidecar. Dropping it stops the process and everything it started.
pub struct Loopback {
    sidecar: Sidecar,
    client: Client,
}

impl Loopback {
    /// Starts `cmd` with a fresh token in the environment, waits for its readiness line, and answers
    /// the running sidecar with a client for its port.
    pub fn start(mut cmd: Command, options: &LoopbackOptions) -> Result<Loopback, StartError> {
        let token = Token::generate().map_err(StartError::Spawn)?;
        cmd.env(&options.token_env, token.as_str());
        let mut sidecar = Sidecar::spawn(
            cmd,
            Output::Lines {
                stderr: options.stderr.clone(),
            },
        )
        .map_err(StartError::Spawn)?;
        let prefix = options.ready_prefix.as_str();
        let port = match sidecar
            .wait_line(options.ready_timeout, |line| line.starts_with(prefix))
            .map_err(StartError::Spawn)?
        {
            LineReadiness::Line(line) => {
                ready_port(&line, prefix).ok_or(StartError::BadReadyLine(line))?
            }
            LineReadiness::Exited(status) => return Err(StartError::Exited(status)),
            LineReadiness::TimedOut => return Err(StartError::TimedOut(options.ready_timeout)),
        };
        Ok(Loopback {
            sidecar,
            client: Client::new(port, &token, options),
        })
    }

    /// The client for the sidecar's port, carrying its token.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The sidecar process, to check on or stop.
    pub fn sidecar(&mut self) -> &mut Sidecar {
        &mut self.sidecar
    }

    /// Stops the sidecar, waiting up to `grace` for it to exit on its own first.
    pub fn shutdown(self, grace: Duration) -> std::io::Result<std::process::ExitStatus> {
        self.sidecar.shutdown(grace)
    }
}

/// An answer from the sidecar: its status and its body as text. A status outside 2xx is an answer
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

    /// What the host said about a request it failed, when it said something: the body
    /// `{"fault":{…}}` that `UseFaults` in `TauriKit.Sidecar.Loopback` answers an unexpected
    /// exception with. `None` for a 2xx answer and for any other body.
    pub fn fault(&self) -> Option<Fault> {
        #[derive(serde::Deserialize)]
        struct Answer {
            fault: Fault,
        }
        if self.is_success() {
            return None;
        }
        serde_json::from_str::<Answer>(&self.body)
            .ok()
            .map(|a| a.fault)
    }
}

/// An unexpected failure on the host side, without anything it was about — the shape of
/// `FaultView` in `TauriKit.Sidecar.Loopback`: the exception's type, and the methods of the app's
/// own code it passed through, innermost first ([`at`](Fault::at) is the first of them). The
/// exception's message never crosses: it can quote the data the request was about.
///
/// It reads a list of them too, for a host that keeps failures of work no request waited on and
/// hands them over when asked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Fault {
    /// The exception's type, such as `System.IO.IOException`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The innermost method of the app's own code, when the failure passed through any.
    #[serde(default)]
    pub at: Option<String>,
    /// The app's own methods the failure passed through, innermost first.
    #[serde(default)]
    pub frames: Vec<String>,
}

/// Why a request got no answer.
#[derive(Debug)]
pub struct TransportError(String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the sidecar could not be reached: {}", self.0)
    }
}

impl std::error::Error for TransportError {}

/// Talks HTTP to a sidecar on 127.0.0.1: every request carries the token, no proxy is ever used
/// (a system proxy would otherwise see loopback traffic and the token), and a non-2xx status comes
/// back as a [`Response`] rather than an error.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
    authorization: String,
    body_limit: u64,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for `port` on 127.0.0.1, carrying `token`.
    pub fn new(port: u16, token: &Token, options: &LoopbackOptions) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(options.request_timeout))
            .build()
            .into();
        Client {
            agent,
            base: format!("http://127.0.0.1:{port}"),
            authorization: token.bearer(),
            body_limit: options.body_limit,
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// GET `path` (starting with `/`).
    pub fn get(&self, path: &str) -> Result<Response, TransportError> {
        let response = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Authorization", &self.authorization)
            .call();
        self.read(response)
    }

    /// POST `json` to `path` (starting with `/`), as `application/json`.
    pub fn post_json(&self, path: &str, json: &str) -> Result<Response, TransportError> {
        let response = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Authorization", &self.authorization)
            .content_type("application/json")
            .send(json);
        self.read(response)
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
    fn a_token_is_64_hex_digits_and_new_each_time() {
        let a = Token::generate().unwrap();
        let b = Token::generate().unwrap();
        assert_eq!(a.as_str().len(), 64);
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(a.bearer(), format!("Bearer {}", a.as_str()));
    }

    #[test]
    fn a_token_never_shows_in_debug_output() {
        let token = Token::generate().unwrap();
        assert!(!format!("{token:?}").contains(token.as_str()));
    }

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
    fn a_fault_outside_the_apps_code_has_no_frames() {
        let failed = Response {
            status: 500,
            body: r#"{"fault":{"type":"System.InvalidOperationException","at":null,"frames":[]}}"#
                .into(),
        };
        let fault = failed.fault().unwrap();
        assert_eq!(fault.at, None);
        assert!(fault.frames.is_empty());
    }

    #[test]
    fn only_a_failed_answer_with_a_fault_body_has_a_fault() {
        let ok = Response {
            status: 200,
            body: r#"{"fault":{"type":"X","frames":[]}}"#.into(),
        };
        assert_eq!(ok.fault(), None);
        let bare = Response {
            status: 503,
            body: String::new(),
        };
        assert_eq!(bare.fault(), None);
        let other = Response {
            status: 400,
            body: r#"{"error":"bad"}"#.into(),
        };
        assert_eq!(other.fault(), None);
    }

    #[test]
    fn the_port_follows_the_prefix() {
        assert_eq!(
            ready_port("helper ready port=51234", "helper ready port="),
            Some(51234)
        );
        assert_eq!(
            ready_port("helper ready port= 80 \r", "helper ready port="),
            Some(80)
        );
        assert_eq!(
            ready_port("helper ready port=x", "helper ready port="),
            None
        );
        assert_eq!(ready_port("something else", "helper ready port="), None);
    }
}
