//! Sending queued reports to Azure Application Insights (the `appinsights` feature).

use std::io;
use std::path::Path;

use crate::queue::read_offset;
use crate::Report;

/// How many reports go out in one request.
const BATCH: usize = 100;

/// Where reports go: an Application Insights resource, named by its connection string.
///
/// ```no_run
/// use std::path::Path;
/// use tauri_kit_diagnostics::Sink;
///
/// // The connection string is usually baked in at build time; a build without one sends nothing.
/// if let Some(sink) = option_env!("MY_APP_APPINSIGHTS_CONNECTION_STRING").and_then(Sink::parse) {
///     let dir = Path::new("/path/to/logs");
///     let (file, sent) = (dir.join("reports.jsonl"), dir.join("reports.sent"));
///     // Off the startup path: what fails to go out now stays for the next launch.
///     std::thread::spawn(move || sink.send_pending(&Sink::agent(), &file, &sent));
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sink {
    /// The resource's instrumentation key.
    pub instrumentation_key: String,
    /// The ingestion endpoint's track URL.
    pub track_url: String,
}

impl Sink {
    /// The sink a connection string names, if it names one: it needs an `InstrumentationKey` and
    /// an `https://` `IngestionEndpoint`.
    pub fn parse(connection_string: &str) -> Option<Self> {
        let field = |name: &str| {
            connection_string
                .split(';')
                .find_map(|part| part.trim().strip_prefix(name)?.strip_prefix('='))
                .filter(|value| !value.is_empty())
        };
        let key = field("InstrumentationKey")?;
        let endpoint = field("IngestionEndpoint").filter(|e| e.starts_with("https://"))?;
        Some(Sink {
            instrumentation_key: key.to_string(),
            track_url: format!("{}/v2.1/track", endpoint.trim_end_matches('/')),
        })
    }

    /// A report as Application Insights takes it: one exception telemetry item, whose type and
    /// message are both the report's kind, whose stack is the report's frames, whose properties
    /// are the report's details, and whose cloud role is the report's layer. It adds nothing that
    /// is not in the report.
    pub fn envelope(&self, report: &Report) -> serde_json::Value {
        serde_json::json!({
            "name": "Microsoft.ApplicationInsights.Exception",
            "time": report.time,
            "iKey": self.instrumentation_key,
            "tags": {
                "ai.cloud.role": report.layer,
                "ai.application.ver": report.version,
                "ai.device.osVersion": format!("{} {}", report.os, report.arch),
            },
            "data": {
                "baseType": "ExceptionData",
                "baseData": {
                    "ver": 2,
                    "exceptions": [{
                        "typeName": report.kind,
                        "message": report.kind,
                        "hasFullStack": false,
                        "stack": report.frames.join("\n"),
                    }],
                    "severityLevel": 3,
                    "properties": report.details,
                },
            },
        })
    }

    /// An agent for the ingestion endpoint: the OS's TLS and certificate store, and the proxy the
    /// PC is set up with — an office network that inspects TLS has its own root in that store.
    /// HTTP error statuses are answers, not errors, and a request gives up after 30 seconds.
    pub fn agent() -> ureq::Agent {
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
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
    /// Reports go out in batches of up to 100, and `sent` is written after each batch. A line
    /// that is not a report is passed over, and a last line without its newline (one still being
    /// written) is left for later. A `file` shorter than what `sent` records is a new file and is
    /// sent from its start; a missing `file` is nothing to send. Stops with an error at the first
    /// request that fails for a reason that may pass — the network, or the endpoint answering
    /// 408, 429 or 5xx — and leaves the rest for the next call. Any other answer counts as
    /// handled: what was not taken then never will be, and is not sent again. Returns how many
    /// reports the endpoint took.
    ///
    /// `agent` should hand HTTP error statuses back as answers (`http_status_as_error(false)`), as
    /// [`Sink::agent`] does; an agent that turns them into errors makes every refusal a reason to
    /// try again later.
    pub fn send_pending(&self, agent: &ureq::Agent, file: &Path, sent: &Path) -> io::Result<usize> {
        use std::io::{Read, Seek};
        let mut pending = match std::fs::File::open(file) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let len = usize::try_from(pending.metadata()?.len()).map_err(io::Error::other)?;
        // A file shorter than what was sent is a new file: the old one was deleted.
        let mut offset = read_offset(sent)
            .filter(|&offset| offset <= len)
            .unwrap_or(0);
        // Only what follows what was sent is read: the file only grows between trims.
        pending.seek(io::SeekFrom::Start(offset as u64))?;
        let mut bytes = Vec::new();
        pending.read_to_end(&mut bytes)?;
        // Only whole lines: a launch may still be writing the last one.
        let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let lines: Vec<&[u8]> = bytes[..end].split_inclusive(|&b| b == b'\n').collect();
        let mut count = 0;
        for batch in lines.chunks(BATCH) {
            let reports: Vec<Report> = batch
                .iter()
                .filter_map(|line| serde_json::from_slice(line).ok())
                .collect();
            if !reports.is_empty() {
                let items: Vec<_> = reports.iter().map(|report| self.envelope(report)).collect();
                let body = serde_json::to_vec(&items).map_err(io::Error::other)?;
                let status = agent
                    .post(&self.track_url)
                    .header("Content-Type", "application/json")
                    .send(&body[..])
                    .map_err(io::Error::other)?
                    .status()
                    .as_u16();
                // Busy, throttled or down: the same reports may be taken later.
                if matches!(status, 408 | 429) || status >= 500 {
                    return Err(io::Error::other(format!("the endpoint answered {status}")));
                }
                // Anything else was taken, or will never be: either way it is not sent again.
                if (200..300).contains(&status) {
                    count += reports.len();
                }
            }
            offset += batch.iter().map(|line| line.len()).sum::<usize>();
            std::fs::write(sent, offset.to_string())?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameRule, Layer, Reporter};

    fn ui() -> Layer {
        Layer::new("ui", FrameRule::web_bundle())
    }

    #[test]
    fn reads_the_sink_from_a_connection_string() {
        let sink = Sink::parse(
            "InstrumentationKey=00000000-1111-2222-3333-444444444444;IngestionEndpoint=https://westeurope-5.in.applicationinsights.azure.com/;LiveEndpoint=https://live/;ApplicationId=x",
        )
        .unwrap();
        assert_eq!(
            sink.instrumentation_key,
            "00000000-1111-2222-3333-444444444444"
        );
        assert_eq!(
            sink.track_url,
            "https://westeurope-5.in.applicationinsights.azure.com/v2.1/track"
        );
    }

    #[test]
    fn a_connection_string_without_a_key_or_an_https_endpoint_names_no_sink() {
        assert_eq!(Sink::parse(""), None);
        assert_eq!(Sink::parse("IngestionEndpoint=https://x/"), None);
        assert_eq!(
            Sink::parse("InstrumentationKey=k;IngestionEndpoint=http://x/"),
            None
        );
        assert_eq!(
            Sink::parse("InstrumentationKey=;IngestionEndpoint=https://x/"),
            None
        );
    }

    #[test]
    fn a_report_goes_out_as_one_exception_item() {
        let sink = Sink {
            instrumentation_key: "k".into(),
            track_url: "https://x/v2.1/track".into(),
        };
        let mut report = Report::new(
            &ui(),
            "TypeError",
            "at save (http://tauri.localhost/assets/index-a.js:1:2)",
            "1.2.3",
        );
        report.time = "2026-09-29T09:26:31Z".into();
        let item = sink.envelope(&report);
        assert_eq!(item["name"], "Microsoft.ApplicationInsights.Exception");
        assert_eq!(item["time"], "2026-09-29T09:26:31Z");
        assert_eq!(item["iKey"], "k");
        assert_eq!(item["tags"]["ai.cloud.role"], "ui");
        assert_eq!(item["tags"]["ai.application.ver"], "1.2.3");
        assert_eq!(item["data"]["baseType"], "ExceptionData");
        let exception = &item["data"]["baseData"]["exceptions"][0];
        assert_eq!(exception["typeName"], "TypeError");
        assert_eq!(exception["message"], "TypeError");
        assert_eq!(exception["stack"], "save index-a.js:1:2");
        // Still nothing but the report: the envelope adds no field that could carry content.
        assert!(item.to_string().is_ascii());
        assert_eq!(
            item["data"]["baseData"]["properties"],
            serde_json::json!({})
        );
    }

    #[test]
    fn a_reports_details_go_out_as_its_properties() {
        let sink = Sink {
            instrumentation_key: "k".into(),
            track_url: "https://x/v2.1/track".into(),
        };
        let report = Report::new(&ui(), "EngineFailed", "", "1.2.3")
            .detail("status", "500")
            .detail("code", "engine");
        let item = sink.envelope(&report);
        assert_eq!(
            item["data"]["baseData"]["properties"],
            serde_json::json!({ "code": "engine", "status": "500" })
        );
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
