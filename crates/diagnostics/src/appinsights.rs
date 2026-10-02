//! Sending queued reports to Azure Application Insights (the `appinsights` and
//! `appinsights-rustls` features).

use std::io;
use std::path::Path;

use crate::{mark_sent, unsent, Sink};

impl Sink {
    /// An agent for the ingestion endpoint: the OS's certificate store and the proxy the PC is set
    /// up with — an office network that inspects TLS has its own root in that store. The handshake
    /// is the OS's own TLS with the `appinsights` feature, rustls with `appinsights-rustls` alone.
    /// HTTP error statuses are answers, not errors, and a request gives up after 30 seconds.
    pub fn agent() -> ureq::Agent {
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
        #[cfg(feature = "appinsights")]
        let provider = TlsProvider::NativeTls;
        #[cfg(not(feature = "appinsights"))]
        let provider = TlsProvider::Rustls;
        let tls = TlsConfig::builder()
            .provider(provider)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into()
    }

    /// Sends the reports `file` has gained since the last send, and records in `sent` how far
    /// into `file` has been sent.
    ///
    /// Reports go out in the batches [`unsent`](crate::unsent) reads, and `sent` is written after
    /// each batch. Stops with an error at the first request that fails for a reason that may pass —
    /// the network, or an answer [`Sink::retry_later`] names — and leaves the rest for the next
    /// call. Any other answer counts as handled: what was not taken then never will be, and is not
    /// sent again. Returns how many reports the endpoint took.
    ///
    /// `agent` should hand HTTP error statuses back as answers (`http_status_as_error(false)`), as
    /// [`Sink::agent`] does; an agent that turns them into errors makes every refusal a reason to
    /// try again later.
    pub fn send_pending(&self, agent: &ureq::Agent, file: &Path, sent: &Path) -> io::Result<usize> {
        let mut count = 0;
        for batch in unsent(file, sent)? {
            if !batch.reports.is_empty() {
                let items: Vec<_> = batch
                    .reports
                    .iter()
                    .map(|report| self.envelope(report))
                    .collect();
                let body = serde_json::to_vec(&items).map_err(io::Error::other)?;
                let status = agent
                    .post(&self.track_url)
                    .header("Content-Type", "application/json")
                    .send(&body[..])
                    .map_err(io::Error::other)?
                    .status()
                    .as_u16();
                if Sink::retry_later(status) {
                    return Err(io::Error::other(format!("the endpoint answered {status}")));
                }
                if (200..300).contains(&status) {
                    count += batch.reports.len();
                }
            }
            mark_sent(sent, batch.end)?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::BATCH;
    use crate::{FrameRule, Layer, Report, Reporter};

    fn ui() -> Layer {
        Layer::new("ui", FrameRule::web_bundle())
    }

    /// A loopback endpoint that answers each request with the next of `statuses`, and hands back
    /// the bodies it was sent.
    fn endpoint(statuses: Vec<u16>) -> (Sink, std::thread::JoinHandle<Vec<serde_json::Value>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let sink = Sink {
            instrumentation_key: "k".into(),
            track_url: format!("http://{}/v2.1/track", listener.local_addr().unwrap()),
        };
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for status in statuses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                bodies.push(serde_json::from_slice(&body).unwrap());
                let answer = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                reader.into_inner().write_all(answer.as_bytes()).unwrap();
            }
            bodies
        });
        (sink, server)
    }

    fn loopback() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .build()
            .into()
    }

    fn paths(dir: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
        (
            dir.path().join("reports.jsonl"),
            dir.path().join("reports.sent"),
        )
    }

    #[test]
    fn sends_what_was_written_since_the_last_send() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        let reporter = Reporter::new(&file);
        reporter
            .record(Report::new(&ui(), "TypeError", "", "1.2.3"))
            .unwrap();
        reporter
            .record(Report::new(
                &Layer::new("host", FrameRule::dotnet_method()),
                "System.IOException",
                "",
                "1.2.3",
            ))
            .unwrap();

        let (sink, server) = endpoint(vec![200]);
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 2);
        let bodies = server.join().unwrap();
        let items = bodies[0].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0]["data"]["baseData"]["exceptions"][0]["typeName"],
            "TypeError"
        );
        assert_eq!(items[1]["tags"]["ai.cloud.role"], "host");

        // Nothing new: no request at all.
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 0);

        // Only what came after.
        reporter
            .record(Report::new(
                &Layer::new("shell", FrameRule::rust_source()),
                "Panic",
                "",
                "1.2.3",
            ))
            .unwrap();
        let (sink, server) = endpoint(vec![200]);
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 1);
        assert_eq!(
            server.join().unwrap()[0][0]["data"]["baseData"]["exceptions"][0]["typeName"],
            "Panic"
        );
    }

    #[test]
    fn sends_in_batches() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        let reporter = Reporter::new(&file).max_reports(BATCH + 10);
        for i in 0..BATCH + 1 {
            reporter
                .record(Report::new(&ui(), &format!("Failure{i}"), "", "1.2.3"))
                .unwrap();
        }
        let (sink, server) = endpoint(vec![200, 200]);
        assert_eq!(
            sink.send_pending(&loopback(), &file, &sent).unwrap(),
            BATCH + 1
        );
        let bodies = server.join().unwrap();
        assert_eq!(bodies[0].as_array().unwrap().len(), BATCH);
        assert_eq!(bodies[1].as_array().unwrap().len(), 1);
    }

    #[test]
    fn keeps_the_reports_for_later_when_the_endpoint_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        Reporter::new(&file)
            .record(Report::new(&ui(), "TypeError", "", "1.2.3"))
            .unwrap();

        for busy in [503, 429, 408] {
            let (sink, server) = endpoint(vec![busy]);
            assert!(sink.send_pending(&loopback(), &file, &sent).is_err());
            server.join().unwrap();
        }

        let (sink, server) = endpoint(vec![200]);
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 1);
        server.join().unwrap();
    }

    #[test]
    fn does_not_send_again_what_the_endpoint_will_never_take() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        Reporter::new(&file)
            .record(Report::new(&ui(), "TypeError", "", "1.2.3"))
            .unwrap();

        let (sink, server) = endpoint(vec![400]);
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 0);
        server.join().unwrap();
        // The line counts as handled: a second send makes no request.
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 0);
    }

    #[test]
    fn passes_over_lines_that_are_not_reports_and_a_line_still_being_written() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        // A line without a time is not a report.
        let old = r#"{"layer":"ui","kind":"TypeError","frames":[],"version":"0.1.0","os":"windows","arch":"x86_64"}"#;
        let report = serde_json::to_string(&Report::new(&ui(), "RangeError", "", "1.2.3")).unwrap();
        std::fs::write(&file, format!("{old}\n{report}\n{{\"layer\":\"ui\"")).unwrap();

        let (sink, server) = endpoint(vec![200]);
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 1);
        let bodies = server.join().unwrap();
        assert_eq!(bodies[0].as_array().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(&sent).unwrap(),
            (old.len() + 1 + report.len() + 1).to_string()
        );
    }

    #[test]
    fn a_new_file_shorter_than_what_was_sent_is_sent_from_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        std::fs::write(&sent, "999999").unwrap();
        Reporter::new(&file)
            .record(Report::new(&ui(), "TypeError", "", "1.2.3"))
            .unwrap();

        let (sink, server) = endpoint(vec![200]);
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 1);
        server.join().unwrap();
    }

    #[test]
    fn a_missing_file_is_nothing_to_send() {
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        let sink = Sink {
            instrumentation_key: "k".into(),
            track_url: "http://127.0.0.1:9/v2.1/track".into(),
        };
        assert_eq!(sink.send_pending(&loopback(), &file, &sent).unwrap(), 0);
    }

    /// Sends one report to a real resource over HTTPS — the OS's TLS and certificate store. Run by
    /// hand with the connection string in the environment:
    /// `TAURI_KIT_APPINSIGHTS_CONNECTION_STRING=… cargo test -p tauri-kit-diagnostics --features appinsights -- --ignored reaches_the_ingestion_endpoint`
    #[test]
    #[ignore = "needs an Application Insights resource"]
    fn reaches_the_ingestion_endpoint() {
        let sink = Sink::parse(&std::env::var("TAURI_KIT_APPINSIGHTS_CONNECTION_STRING").unwrap())
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (file, sent) = paths(&dir);
        Reporter::new(&file)
            .record(Report::new(
                &Layer::new("shell", FrameRule::rust_source()),
                "EnvelopeProbe",
                "",
                env!("CARGO_PKG_VERSION"),
            ))
            .unwrap();
        assert_eq!(sink.send_pending(&Sink::agent(), &file, &sent).unwrap(), 1);
    }
}
