//! What goes out to Azure Application Insights, whichever HTTP client takes it there.

use crate::Report;

/// Where reports go: an Application Insights resource, named by its connection string.
///
/// With the `appinsights` or `appinsights-rustls` feature, `Sink::send_pending` sends what a report
/// file gained since the last send. Without either, a sink still makes what goes out — the track
/// URL and one item per report ([`Sink::envelope`]) — for an app that sends with an HTTP client of
/// its own, reading the file with [`unsent`](crate::unsent) and recording what was handled with
/// [`mark_sent`](crate::mark_sent):
///
/// ```no_run
/// use std::path::Path;
/// use tauri_kit_diagnostics::{mark_sent, unsent, Sink};
///
/// # fn post(url: &str, body: &[u8]) -> std::io::Result<u16> { Ok(200) }
/// // The connection string is usually baked in at build time; a build without one sends nothing.
/// if let Some(sink) = option_env!("MY_APP_APPINSIGHTS_CONNECTION_STRING").and_then(Sink::parse) {
///     let dir = Path::new("/path/to/logs");
///     let (file, sent) = (dir.join("reports.jsonl"), dir.join("reports.sent"));
///     for batch in unsent(&file, &sent)? {
///         if !batch.reports.is_empty() {
///             let items: Vec<_> = batch.reports.iter().map(|r| sink.envelope(r)).collect();
///             let status = post(&sink.track_url, &serde_json::to_vec(&items)?)?;
///             // Busy, throttled or down: leave this batch and the rest for later.
///             if Sink::retry_later(status) {
///                 break;
///             }
///         }
///         mark_sent(&sent, batch.end)?;
///     }
/// }
/// # Ok::<(), std::io::Error>(())
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

    /// Whether an answer from the ingestion endpoint means the same reports may be taken later:
    /// busy (408), throttled (429) or down (5xx). Any other answer counts as handled — what was not
    /// taken then never will be — and the reports are not sent again.
    pub fn retry_later(status: u16) -> bool {
        matches!(status, 408 | 429) || status >= 500
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameRule, Layer};

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

    #[test]
    fn only_busy_throttled_or_down_answers_are_tried_again() {
        for status in [408, 429, 500, 503] {
            assert!(Sink::retry_later(status), "{status}");
        }
        for status in [200, 206, 400, 401, 404, 413] {
            assert!(!Sink::retry_later(status), "{status}");
        }
    }
}
