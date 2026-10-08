//! What goes out to Azure Application Insights, whichever HTTP client takes it there.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{Report, SessionEvent, SessionKind};

/// A line of a file a [`Sink`] sends: a [`Report`] or a [`SessionEvent`].
pub trait Telemetry: Serialize + DeserializeOwned {
    /// The items Application Insights takes for this line.
    fn items(&self, sink: &Sink) -> Vec<serde_json::Value>;
}

impl Telemetry for Report {
    fn items(&self, sink: &Sink) -> Vec<serde_json::Value> {
        vec![sink.envelope(self)]
    }
}

impl Telemetry for SessionEvent {
    fn items(&self, sink: &Sink) -> Vec<serde_json::Value> {
        sink.session_items(self)
    }
}

/// Whole seconds as Application Insights writes a time span: `[d.]hh:mm:ss.fff`.
fn timespan(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let clock = format!(
        "{:02}:{:02}:{:02}.000",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    );
    if days > 0 {
        format!("{days}.{clock}")
    } else {
        clock
    }
}

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

    /// A session event as Application Insights takes it, adding nothing that is not in the event.
    ///
    /// Every item is tagged with the installation as the user (`ai.user.id`), the session
    /// (`ai.session.id`), the app's version and the platform, so the resource's users, sessions
    /// and page views read them. A start or an unfinished session is one event telemetry item
    /// (`SessionStart` with the language and track as properties, `SessionUnfinished` with when it
    /// started); an end is one event item (`SessionEnd` with its length in seconds as a
    /// measurement) and one page view item for each screen, whose duration is the time on it and
    /// whose `opened` measurement is how many times it was opened.
    pub fn session_items(&self, event: &SessionEvent) -> Vec<serde_json::Value> {
        let (os, arch) = event.platform();
        let mut tags = serde_json::json!({
            "ai.user.id": event.install(),
            "ai.session.id": event.session(),
            "ai.application.ver": event.version(),
            "ai.device.osVersion": format!("{os} {arch}"),
        });
        if let Some(locale) = event.locale() {
            tags["ai.device.locale"] = locale.into();
        }
        let item = |name: &str, base_type: &str, base: serde_json::Value| {
            serde_json::json!({
                "name": name,
                "time": event.time(),
                "iKey": self.instrumentation_key,
                "tags": tags,
                "data": { "baseType": base_type, "baseData": base },
            })
        };
        let custom =
            |name: &str, properties: serde_json::Value, measurements: serde_json::Value| {
                item(
                    "Microsoft.ApplicationInsights.Event",
                    "EventData",
                    serde_json::json!({
                        "ver": 2,
                        "name": name,
                        "properties": properties,
                        "measurements": measurements,
                    }),
                )
            };
        match event.kind() {
            SessionKind::Start => vec![custom(
                "SessionStart",
                serde_json::json!({ "locale": event.locale(), "track": event.track() }),
                serde_json::json!({}),
            )],
            SessionKind::Unfinished => vec![custom(
                "SessionUnfinished",
                serde_json::json!({ "started": event.started() }),
                serde_json::json!({}),
            )],
            SessionKind::End => std::iter::once(custom(
                "SessionEnd",
                serde_json::json!({}),
                serde_json::json!({ "seconds": event.seconds().unwrap_or(0) }),
            ))
            .chain(event.screens().iter().map(|(name, time)| {
                item(
                    "Microsoft.ApplicationInsights.PageView",
                    "PageViewData",
                    serde_json::json!({
                        "ver": 2,
                        "name": name,
                        "duration": timespan(time.seconds),
                        "measurements": { "opened": time.opened },
                    }),
                )
            }))
            .collect(),
        }
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

    fn session_event(line: serde_json::Value) -> SessionEvent {
        let mut base = serde_json::json!({
            "install": "0123456789abcdef0123456789abcdef",
            "session": "fedcba9876543210fedcba9876543210",
            "version": "1.2.3",
            "os": "windows",
            "arch": "x86_64",
            "time": "2026-10-08T09:00:00Z",
        });
        base.as_object_mut()
            .unwrap()
            .extend(line.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    fn sink() -> Sink {
        Sink {
            instrumentation_key: "k".into(),
            track_url: "https://x/v2.1/track".into(),
        }
    }

    #[test]
    fn a_session_start_goes_out_as_one_event_of_its_user_and_session() {
        let start = session_event(serde_json::json!({
            "event": "start", "locale": "ko-KR", "track": "school",
        }));
        let items = sink().session_items(&start);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item["name"], "Microsoft.ApplicationInsights.Event");
        assert_eq!(item["time"], "2026-10-08T09:00:00Z");
        assert_eq!(
            item["tags"]["ai.user.id"],
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            item["tags"]["ai.session.id"],
            "fedcba9876543210fedcba9876543210"
        );
        assert_eq!(item["tags"]["ai.application.ver"], "1.2.3");
        assert_eq!(item["tags"]["ai.device.osVersion"], "windows x86_64");
        assert_eq!(item["tags"]["ai.device.locale"], "ko-KR");
        assert_eq!(item["data"]["baseType"], "EventData");
        assert_eq!(item["data"]["baseData"]["name"], "SessionStart");
        assert_eq!(
            item["data"]["baseData"]["properties"],
            serde_json::json!({ "locale": "ko-KR", "track": "school" })
        );
    }

    #[test]
    fn a_session_end_goes_out_as_an_event_and_a_page_view_for_each_screen() {
        let end = session_event(serde_json::json!({
            "event": "end",
            "seconds": 93_784,
            "screens": {
                "home": { "opened": 2, "seconds": 14 },
                "report": { "opened": 1, "seconds": 93_770 },
            },
        }));
        let items = sink().session_items(&end);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["data"]["baseData"]["name"], "SessionEnd");
        assert_eq!(
            items[0]["data"]["baseData"]["measurements"],
            serde_json::json!({ "seconds": 93_784 })
        );
        let home = &items[1];
        assert_eq!(home["name"], "Microsoft.ApplicationInsights.PageView");
        assert_eq!(
            home["tags"]["ai.session.id"],
            "fedcba9876543210fedcba9876543210"
        );
        assert_eq!(home["data"]["baseType"], "PageViewData");
        assert_eq!(home["data"]["baseData"]["name"], "home");
        assert_eq!(home["data"]["baseData"]["duration"], "00:00:14.000");
        assert_eq!(home["data"]["baseData"]["measurements"]["opened"], 2);
        assert_eq!(items[2]["data"]["baseData"]["duration"], "1.02:02:50.000");
        // Nothing but the event: every value is one of its fields or a fixed name.
        assert!(items.iter().all(|i| i.to_string().is_ascii()));
    }

    #[test]
    fn an_unfinished_session_goes_out_as_one_event_with_when_it_started() {
        let unfinished = session_event(serde_json::json!({
            "event": "unfinished", "started": "2026-10-07T23:00:00Z",
        }));
        let items = sink().session_items(&unfinished);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["data"]["baseData"]["name"], "SessionUnfinished");
        assert_eq!(
            items[0]["data"]["baseData"]["properties"],
            serde_json::json!({ "started": "2026-10-07T23:00:00Z" })
        );
        assert!(items[0]["tags"].get("ai.device.locale").is_none());
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
