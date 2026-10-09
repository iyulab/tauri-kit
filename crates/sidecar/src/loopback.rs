//! A desktop app talking HTTP to a backend on the loopback interface.
//!
//! The backend listens on 127.0.0.1 on a port the system picks and answers only requests that carry
//! its credential as a bearer token. The app learns where it is in one of two ways:
//!
//! - **It starts the backend as its child** ([`Loopback::start`]): it makes a fresh random [`Token`],
//!   hands it over in an environment variable, and waits for the readiness line on which the backend
//!   announces its port.
//! - **The backend lives on its own** and outlives the app's windows: it issues its credential itself
//!   and publishes it with its port in a discovery file only the person running it can read
//!   ([`discover`]). Every start issues a new credential, so a caller that gets a 401 reads the file
//!   once more.
//!
//! Either way the app ends up with an [`Endpoint`] — the address and the authorization header, apart
//! from any HTTP client, so an app with its own (async) client uses that. With the `loopback-client`
//! feature, [`Client`] is a blocking one that carries the token and never goes through a proxy.
//!
//! The variable's name, the readiness line's prefix and every path are the app's. The .NET host side
//! of the same arrangement is the `TauriKit.Sidecar.Loopback` package in this repository's `dotnet/`
//! folder.
//!
//! ```no_run
//! # #[cfg(feature = "loopback-client")]
//! # fn main() -> Result<(), tauri_kit_sidecar::loopback::StartError> {
//! use std::process::Command;
//! use tauri_kit_sidecar::loopback::{Loopback, LoopbackOptions};
//!
//! let helper = Loopback::start(Command::new("helper"), &LoopbackOptions::new("HELPER_TOKEN", "helper ready port="))?;
//! let response = helper.client().post_json("/items", r#"{"name":"a"}"#).expect("the helper answers");
//! assert!(response.is_success(), "{}", response.body);
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "loopback-client"))]
//! # fn main() {}
//! ```

use std::fmt;
use std::process::Command;
use std::time::Duration;

use crate::{LineReadiness, Output, Sidecar};

#[cfg(feature = "loopback-client")]
mod client;
mod discovery;

#[cfg(feature = "loopback-client")]
pub use client::{Client, ClientOptions, Response, Stream, TransportError};
pub use discovery::{discover, DiscoveryError};

/// A secret that admits a caller: made fresh for one run, or read from a discovery file.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// The shortest secret accepted — a short one is a mistake, not a secret.
    pub const MINIMUM_LENGTH: usize = 32;

    /// A new token from the operating system's random source: 32 bytes, as 64 hex digits.
    pub fn generate() -> std::io::Result<Token> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Token(secret.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// A secret made elsewhere — the backend's own, say — or `None` when it is shorter than
    /// [`MINIMUM_LENGTH`](Token::MINIMUM_LENGTH) or holds anything but visible ASCII (it goes in a header).
    pub fn from_secret(secret: impl Into<String>) -> Option<Token> {
        let secret = secret.into();
        (secret.len() >= Self::MINIMUM_LENGTH && secret.bytes().all(|b| b.is_ascii_graphic()))
            .then_some(Token(secret))
    }

    /// The token itself, to hand to the backend.
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

/// Where a loopback backend is and how to be admitted: a port on 127.0.0.1 and the token. The host's
/// duty with it, whatever client it uses: never send it through a proxy (a system proxy would see
/// loopback traffic and the token), and never log [`authorization`](Endpoint::authorization).
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    port: u16,
    token: Token,
}

impl Endpoint {
    /// The backend on `port` that accepts `token`.
    pub fn new(port: u16, token: Token) -> Endpoint {
        Endpoint { port, token }
    }

    /// The port on 127.0.0.1.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The token the backend accepts.
    pub fn token(&self) -> &Token {
        &self.token
    }

    /// `http://127.0.0.1:<port>` — a request's path follows it.
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The `Authorization` header value for every request.
    pub fn authorization(&self) -> String {
        self.token.bearer()
    }
}

/// The port only: the token never shows.
impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// The port a readiness line announces: what follows `prefix`, as a port number.
pub fn ready_port(line: &str, prefix: &str) -> Option<u16> {
    line.strip_prefix(prefix)?.trim().parse().ok()
}

/// How to start a loopback backend as the app's child.
#[derive(Debug, Clone)]
pub struct LoopbackOptions {
    /// The environment variable the token is handed over in.
    pub token_env: String,
    /// What the readiness line starts with; the port follows it.
    pub ready_prefix: String,
    /// How long to wait for the readiness line. Default 30 seconds.
    pub ready_timeout: Duration,
    /// Where the backend's stdout is appended — every line, the readiness line included — if
    /// anywhere.
    pub stdout: Option<std::path::PathBuf>,
    /// Where the backend's stderr is appended, if anywhere.
    pub stderr: Option<std::path::PathBuf>,
    /// How the [`Client`] [`Loopback::start`] makes behaves.
    #[cfg(feature = "loopback-client")]
    pub client: ClientOptions,
}

impl LoopbackOptions {
    /// Options with the defaults, for a token in `token_env` and a readiness line starting with
    /// `ready_prefix`.
    pub fn new(token_env: impl Into<String>, ready_prefix: impl Into<String>) -> Self {
        LoopbackOptions {
            token_env: token_env.into(),
            ready_prefix: ready_prefix.into(),
            ready_timeout: Duration::from_secs(30),
            stdout: None,
            stderr: None,
            #[cfg(feature = "loopback-client")]
            client: ClientOptions::default(),
        }
    }
}

/// Why a loopback backend did not start.
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

/// A running loopback backend the app started. Dropping it stops the process and everything it
/// started.
pub struct Loopback {
    sidecar: Sidecar,
    endpoint: Endpoint,
    #[cfg(feature = "loopback-client")]
    client: Client,
}

impl Loopback {
    /// Starts `cmd` with a fresh token in the environment and waits for its readiness line.
    pub fn start(mut cmd: Command, options: &LoopbackOptions) -> Result<Loopback, StartError> {
        let token = Token::generate().map_err(StartError::Spawn)?;
        cmd.env(&options.token_env, token.as_str());
        let mut sidecar = Sidecar::spawn(
            cmd,
            Output::Lines {
                stdout: options.stdout.clone(),
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
        let endpoint = Endpoint::new(port, token);
        Ok(Loopback {
            sidecar,
            #[cfg(feature = "loopback-client")]
            client: Client::new(&endpoint, &options.client),
            endpoint,
        })
    }

    /// Where the backend listens and the token it accepts — for an app that calls it with its own
    /// HTTP client.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// A blocking client for the backend, carrying its token.
    #[cfg(feature = "loopback-client")]
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The backend process, to check on or stop.
    pub fn sidecar(&mut self) -> &mut Sidecar {
        &mut self.sidecar
    }

    /// The process and the endpoint, apart — for an app that keeps the process where it stops it and
    /// hands copies of the endpoint to whatever makes requests.
    pub fn into_parts(self) -> (Sidecar, Endpoint) {
        (self.sidecar, self.endpoint)
    }

    /// Stops the backend, waiting up to `grace` for it to exit on its own first.
    pub fn shutdown(self, grace: Duration) -> std::io::Result<std::process::ExitStatus> {
        self.sidecar.shutdown(grace)
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

impl Fault {
    /// What the host said about a request it failed, read from the answer's status and body: the
    /// body `{"fault":{…}}` that `UseFaults` in `TauriKit.Sidecar.Loopback` answers an unexpected
    /// exception with. `None` for a 2xx answer and for any other body — for an app reading answers
    /// with its own client.
    pub fn from_answer(status: u16, body: &str) -> Option<Fault> {
        #[derive(serde::Deserialize)]
        struct Answer {
            fault: Fault,
        }
        if (200..300).contains(&status) {
            return None;
        }
        serde_json::from_str::<Answer>(body).ok().map(|a| a.fault)
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
        let endpoint = Endpoint::new(5100, token.clone());
        assert!(!format!("{endpoint:?}").contains(token.as_str()));
    }

    #[test]
    fn a_secret_made_elsewhere_must_be_long_and_fit_a_header() {
        assert!(Token::from_secret("a".repeat(32)).is_some());
        assert!(Token::from_secret("a".repeat(31)).is_none());
        assert!(Token::from_secret(format!("{} b", "a".repeat(32))).is_none());
        assert!(Token::from_secret(format!("{}\r\nX-Other: 1", "a".repeat(32))).is_none());
    }

    #[test]
    fn an_endpoint_is_the_address_and_the_header() {
        let token = Token::from_secret("a".repeat(43)).unwrap();
        let endpoint = Endpoint::new(51234, token);
        assert_eq!(endpoint.base(), "http://127.0.0.1:51234");
        assert_eq!(
            endpoint.authorization(),
            format!("Bearer {}", "a".repeat(43))
        );
        assert_eq!(endpoint.port(), 51234);
    }

    #[test]
    fn a_failed_answer_says_what_failed() {
        let body = r#"{"fault":{"type":"System.IO.IOException","at":"App.Store.Read","frames":["App.Store.Read","App.Api.Get"]}}"#;
        assert_eq!(
            Fault::from_answer(500, body),
            Some(Fault {
                kind: "System.IO.IOException".into(),
                at: Some("App.Store.Read".into()),
                frames: vec!["App.Store.Read".into(), "App.Api.Get".into()],
            })
        );
    }

    #[test]
    fn a_fault_outside_the_apps_code_has_no_frames() {
        let fault = Fault::from_answer(
            500,
            r#"{"fault":{"type":"System.InvalidOperationException","at":null,"frames":[]}}"#,
        )
        .unwrap();
        assert_eq!(fault.at, None);
        assert!(fault.frames.is_empty());
    }

    #[test]
    fn only_a_failed_answer_with_a_fault_body_has_a_fault() {
        assert_eq!(
            Fault::from_answer(200, r#"{"fault":{"type":"X","frames":[]}}"#),
            None
        );
        assert_eq!(Fault::from_answer(503, ""), None);
        assert_eq!(Fault::from_answer(400, r#"{"error":"bad"}"#), None);
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
