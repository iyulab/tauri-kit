//! Events the app declares: a name and a few fields of closed kinds, nothing else.
//!
//! An app says what it wants to count — a feature used, a step that failed and which of a few ways
//! it failed — in a [`Registry`] it writes as JSON and compiles in. [`Events`] writes only what the
//! registry declares, each field checked against its kind, to a JSON Lines file the person can read,
//! as reports and sessions are; what does not fit is dropped and counted, never written as it came.
//! No kind can carry text: a count is bucketed or capped, a choice is one of the declared values, a
//! set is some of them, and a duration is milliseconds under a cap.
//!
//! ```
//! use tauri_kit_diagnostics::{Registry, Value};
//!
//! // Usually `include_str!("diagnostics.json")`: the registry is part of the app, not a file it reads.
//! let registry = Registry::from_json(r#"{ "events": {
//!     "export.finished": { "fields": {
//!         "format": { "kind": "choice", "values": ["csv", "xlsx"] },
//!         "rows": { "kind": "count", "bucket": [0, 10, 100, 1000] },
//!         "took": { "kind": "duration" } } } } }"#)?;
//! let exported = registry.event("export.finished")?; // a name the registry does not declare fails here, at start
//! # Ok::<(), tauri_kit_diagnostics::RegistryError>(())
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::queue::append_line;
use crate::report::{is_frame, plain_kind, utc};
use crate::session::{is_id, is_platform, is_time, is_version, random_id};
use crate::{pending, Pending, Report, Session, MAX_DETAILS, MAX_FRAMES};

/// The event [`Events`] writes when it drops one: `reason` (`unknown_field`, `bad_value`, `capped`),
/// the declared `event`, and for a bad value the declared `field` — never a name or a value the
/// registry does not declare.
pub const DROPPED_EVENT: &str = "kit.events.dropped";

/// How many fields an event may declare.
pub const MAX_FIELDS: usize = 8;

/// How many values a choice or a set may declare.
pub const MAX_VALUES: usize = 64;

/// How many times one event is written in one session by default ([`Events::max_per_event`]).
pub const MAX_PER_EVENT: usize = 100;

/// The largest count cap ([`FieldKind::Count`] `max`) a registry may declare: a count written exactly
/// stays small. A larger range goes in buckets, each bound at least twice the one before.
pub const MAX_COUNT: u64 = 1_000;

/// The longest duration a field may declare, and the default: one day.
pub const MAX_DURATION: Duration = Duration::from_secs(86_400);

/// Why a registry was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError(String);

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegistryError {}

fn refuse<T>(why: impl Into<String>) -> Result<T, RegistryError> {
    Err(RegistryError(why.into()))
}

/// The kind of one field, as the registry declares it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum FieldKind {
    /// A whole number, written as the lower bound of its `bucket` (bounds starting at 0, each bound
    /// after the first non-zero one at least twice the one before), or as itself when at most `max`
    /// (at most [`MAX_COUNT`]). One of the two is required: an exact large number (a file's size) can
    /// tell which file it was.
    #[non_exhaustive]
    Count {
        #[serde(default)]
        bucket: Vec<u64>,
        #[serde(default)]
        max: Option<u64>,
    },
    /// True or false.
    Bool,
    /// One of the declared values.
    #[non_exhaustive]
    Choice { values: Vec<String> },
    /// Some of the declared values, written sorted.
    #[non_exhaustive]
    Set { values: Vec<String> },
    /// How long something took, in milliseconds, at most `max_ms` (default and most: one day).
    #[non_exhaustive]
    Duration {
        #[serde(default)]
        max_ms: Option<u64>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryJson {
    #[serde(default)]
    events: Unique<EventJson>,
    #[serde(default)]
    reports: Unique<Unique<FieldKind>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventJson {
    #[serde(default)]
    fields: Unique<FieldKind>,
}

/// A JSON object read into a map, refusing a name given twice (serde keeps the last one silently).
struct Unique<V>(BTreeMap<String, V>);

impl<V> Default for Unique<V> {
    fn default() -> Self {
        Unique(BTreeMap::new())
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Unique<V> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visit<V>(std::marker::PhantomData<V>);
        impl<'de, V: Deserialize<'de>> serde::de::Visitor<'de> for Visit<V> {
            type Value = Unique<V>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Unique<V>, A::Error> {
                let mut out = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, V>()? {
                    if out.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!("'{key}' is given twice")));
                    }
                }
                Ok(Unique(out))
            }
        }
        deserializer.deserialize_map(Visit(std::marker::PhantomData))
    }
}

/// The events an app may write, each with its fields — see the [module](self) and [`Events`]. The
/// default declares nothing: for an app with no events, whose reports carry no details.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    events: Arc<BTreeMap<String, BTreeMap<String, FieldKind>>>,
    reports: Arc<BTreeMap<String, Arc<BTreeMap<String, FieldKind>>>>,
}

/// A name a [`Registry`] declares, from [`Registry::event`] — so a misspelled name fails when the app
/// starts, not silently in the field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventName(String);

impl EventName {
    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Registry {
    /// Reads a registry: `{ "events": { "<name>": { "fields": { "<field>": { "kind": … } } } },
    /// "reports": { "<layer>": { "<detail>": { "kind": … } } } }` — both sections optional. `reports`
    /// declares the details a report of each layer may carry ([`Registry::layer`]), of the same kinds.
    ///
    /// Refused, with what is wrong, when a name is not a plain lowercase identifier (`export.finished`),
    /// starts with `kit.` (the crate's own), an event has more than [`MAX_FIELDS`] fields, a count has
    /// neither a bucket nor a cap (or both, or a cap over [`MAX_COUNT`]), a choice or set has no values,
    /// more than [`MAX_VALUES`], repeats or values that are not plain identifiers, or a duration cap is
    /// over [`MAX_DURATION`].
    pub fn from_json(json: &str) -> Result<Registry, RegistryError> {
        let parsed: RegistryJson = serde_json::from_str(json)
            .map_err(|e| RegistryError(format!("not a registry: {e}")))?;
        for (name, event) in &parsed.events.0 {
            if !is_event_name(name) {
                return refuse(format!("'{name}' is not a plain lowercase event name"));
            }
            if name.starts_with("kit.") {
                return refuse(format!("'{name}': names under 'kit.' are the crate's own"));
            }
            if event.fields.0.len() > MAX_FIELDS {
                return refuse(format!("'{name}' has more than {MAX_FIELDS} fields"));
            }
            for (field, kind) in &event.fields.0 {
                if !is_field_name(field) {
                    return refuse(format!(
                        "'{name}.{field}' is not a plain lowercase field name"
                    ));
                }
                check_kind(kind)
                    .map_err(|why| RegistryError(format!("'{name}.{field}': {why}")))?;
            }
        }
        for (layer, details) in &parsed.reports.0 {
            if details.0.len() > MAX_FIELDS {
                return refuse(format!(
                    "layer '{layer}' has more than {MAX_FIELDS} details"
                ));
            }
            for (detail, kind) in &details.0 {
                if !is_field_name(detail) {
                    return refuse(format!(
                        "'{layer}.{detail}' is not a plain lowercase detail name"
                    ));
                }
                check_kind(kind)
                    .map_err(|why| RegistryError(format!("'{layer}.{detail}': {why}")))?;
            }
        }
        Ok(Registry {
            events: Arc::new(
                parsed
                    .events
                    .0
                    .into_iter()
                    .map(|(n, e)| (n, e.fields.0))
                    .collect(),
            ),
            reports: Arc::new(
                parsed
                    .reports
                    .0
                    .into_iter()
                    .map(|(layer, details)| (layer, Arc::new(details.0)))
                    .collect(),
            ),
        })
    }

    /// A [`Layer`](crate::Layer) named `name` whose frames `rule` picks, carrying the details this
    /// registry declares for it under `reports` — none, if it declares none.
    pub fn layer(
        &self,
        name: impl Into<String>,
        rule: impl Into<crate::FrameRule>,
    ) -> crate::Layer {
        let name = name.into();
        let details = self.reports.get(&name).cloned().unwrap_or_default();
        crate::Layer::new(name, rule).with_details(details)
    }

    /// The declared event `name`, or an error naming it.
    pub fn event(&self, name: &str) -> Result<EventName, RegistryError> {
        if self.events.contains_key(name) {
            Ok(EventName(name.to_string()))
        } else {
            refuse(format!("the registry does not declare '{name}'"))
        }
    }

    /// Whether `line` is an event this registry would write — every field declared and of its kind —
    /// or the crate's own [`DROPPED_EVENT`] about one of its events. What a sender reads back from the
    /// file goes through this, so a line edited into the file is never sent.
    pub fn verify(&self, line: EventLine) -> Option<Event> {
        let head_ok = is_id(&line.session)
            && is_version(&line.version)
            && is_platform(&line.os)
            && is_platform(&line.arch)
            && is_time(&line.time);
        let fields_ok = if line.name == DROPPED_EVENT {
            self.dropped_ok(&line.fields)
        } else {
            let declared = self.events.get(&line.name)?;
            line.fields.len() == declared.len()
                && line.fields.iter().all(|(field, value)| {
                    declared
                        .get(field)
                        .is_some_and(|kind| written_ok(kind, value))
                })
        };
        (head_ok && fields_ok).then_some(Event(line))
    }

    /// Whether `report` is one this registry's layers would write: the layer and kind plain identifiers
    /// (the kind maybe [`UNRECOGNIZED_KIND`](crate::UNRECOGNIZED_KIND) or [`CAPPED_KIND`](crate::CAPPED_KIND)),
    /// at most [`MAX_FRAMES`] frames, each as a [`FrameRule`](crate::FrameRule) writes one, each detail
    /// declared for the layer under `reports` and written as its kind writes it, and the version, platform
    /// and time well formed. What a sender reads back goes through this ([`pending_reports`]), so a line
    /// edited into a file — or written by a process holding another registry — is never sent.
    pub fn verify_report(&self, report: Report) -> Option<Report> {
        let plain = |raw: &str| plain_kind(raw) == raw;
        let head_ok = plain(&report.layer)
            && plain(&report.kind)
            && is_version(&report.version)
            && is_platform(&report.os)
            && is_platform(&report.arch)
            && is_time(&report.time);
        let frames_ok =
            report.frames.len() <= MAX_FRAMES && report.frames.iter().all(|f| is_frame(f));
        let declared = self.reports.get(&report.layer);
        let details_ok = report.details.len() <= MAX_DETAILS
            && report.details.iter().all(|(name, value)| {
                declared
                    .and_then(|d| d.get(name))
                    .and_then(|kind| detail_text(kind, value))
                    .is_some_and(|text| &text == value)
            });
        (head_ok && frames_ok && details_ok).then_some(report)
    }

    fn dropped_ok(&self, fields: &BTreeMap<String, serde_json::Value>) -> bool {
        let text = |key: &str| fields.get(key).and_then(|v| v.as_str());
        let (Some(reason), Some(event)) = (text("reason"), text("event")) else {
            return false;
        };
        let Some(declared) = self.events.get(event) else {
            return false;
        };
        match (reason, text("field")) {
            ("bad_value", Some(field)) => fields.len() == 3 && declared.contains_key(field),
            ("unknown_field" | "capped", None) => fields.len() == 2,
            _ => false,
        }
    }
}

fn check_kind(kind: &FieldKind) -> Result<(), String> {
    match kind {
        FieldKind::Count { bucket, max } => match (bucket.is_empty(), max) {
            (true, None) => Err("a count needs a bucket or a max".into()),
            (false, Some(_)) => Err("a count takes a bucket or a max, not both".into()),
            (true, Some(max)) if !(1..=MAX_COUNT).contains(max) => {
                Err(format!("max must be 1 to {MAX_COUNT}"))
            }
            (false, None) if bucket[0] != 0 || !bucket.windows(2).all(|w| w[0] < w[1]) => {
                Err("bucket bounds must start at 0 and ascend".into())
            }
            (false, None) if !bucket.windows(2).all(|w| w[0] == 0 || w[1] >= w[0].saturating_mul(2)) => {
                Err("each bucket bound after the first non-zero one must be at least twice the one before".into())
            }
            (false, None) if bucket.len() > MAX_VALUES => {
                Err(format!("at most {MAX_VALUES} bucket bounds"))
            }
            _ => Ok(()),
        },
        FieldKind::Choice { values } | FieldKind::Set { values } => {
            let unique: BTreeSet<_> = values.iter().collect();
            if values.is_empty() || values.len() > MAX_VALUES {
                Err(format!("1 to {MAX_VALUES} values"))
            } else if unique.len() != values.len() {
                Err("a value is repeated".into())
            } else if !values.iter().all(|v| is_value(v)) {
                Err("values are plain identifiers of at most 32 characters".into())
            } else {
                Ok(())
            }
        }
        FieldKind::Duration { max_ms } => match max_ms {
            Some(ms) if *ms == 0 || *ms > MAX_DURATION.as_millis() as u64 => {
                Err("max_ms must be 1 ms to one day".into())
            }
            _ => Ok(()),
        },
        FieldKind::Bool => Ok(()),
    }
}

/// The value as it is written, if `value` fits `kind`.
fn written(kind: &FieldKind, value: &Value) -> Option<serde_json::Value> {
    match (kind, value) {
        (FieldKind::Count { bucket, max }, Value::Count(n)) => match max {
            Some(max) => (n <= max).then(|| (*n).into()),
            None => bucket.iter().rev().find(|b| *b <= n).map(|b| (*b).into()),
        },
        (FieldKind::Bool, Value::Bool(b)) => Some((*b).into()),
        (FieldKind::Choice { values }, Value::Choice(v)) => {
            values.contains(v).then(|| v.clone().into())
        }
        (FieldKind::Set { values }, Value::Set(set)) => {
            let set: BTreeSet<&String> = set.iter().collect();
            set.iter()
                .all(|v| values.contains(v))
                .then(|| set.into_iter().cloned().collect::<Vec<_>>().into())
        }
        (FieldKind::Duration { max_ms }, Value::Duration(d)) => {
            let ms = u64::try_from(d.as_millis()).ok()?;
            (ms <= max_ms.unwrap_or(MAX_DURATION.as_millis() as u64)).then(|| ms.into())
        }
        _ => None,
    }
}

/// A report detail as it is written, if `raw` — untrusted text from where the failure was caught — is
/// a value of `kind`: a count, a duration in milliseconds, `true`/`false`, a declared choice, or declared
/// values joined with `,`. The written text is the [`written`] value's, so a count lands on its bucket.
pub(crate) fn detail_text(kind: &FieldKind, raw: &str) -> Option<String> {
    let number = || {
        (!raw.is_empty() && raw.len() <= 20 && raw.bytes().all(|b| b.is_ascii_digit()))
            .then(|| raw.parse::<u64>().ok())
            .flatten()
    };
    let value = match kind {
        FieldKind::Count { .. } => Value::Count(number()?),
        FieldKind::Duration { .. } => Value::Duration(Duration::from_millis(number()?)),
        FieldKind::Bool => Value::Bool(match raw {
            "true" => true,
            "false" => false,
            _ => return None,
        }),
        FieldKind::Choice { .. } => Value::Choice(raw.to_string()),
        FieldKind::Set { .. } => Value::Set(
            raw.split(',')
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect(),
        ),
    };
    Some(match written(kind, &value)? {
        serde_json::Value::String(text) => text,
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    })
}

/// Whether a value read back from the file is one [`written`] could have written for `kind`.
fn written_ok(kind: &FieldKind, value: &serde_json::Value) -> bool {
    match kind {
        FieldKind::Count { bucket, max } => value.as_u64().is_some_and(|n| match max {
            Some(max) => n <= *max,
            None => bucket.contains(&n),
        }),
        FieldKind::Bool => value.is_boolean(),
        FieldKind::Choice { values } => value
            .as_str()
            .is_some_and(|v| values.iter().any(|d| d == v)),
        FieldKind::Set { values } => value.as_array().is_some_and(|items| {
            let texts: Vec<_> = items.iter().filter_map(|i| i.as_str()).collect();
            texts.len() == items.len()
                && texts.windows(2).all(|w| w[0] < w[1])
                && texts.iter().all(|t| values.iter().any(|d| d == t))
        }),
        FieldKind::Duration { max_ms } => value
            .as_u64()
            .is_some_and(|ms| ms <= max_ms.unwrap_or(MAX_DURATION.as_millis() as u64)),
    }
}

/// `export.finished`: lowercase letters, digits and `_`, in parts joined by `.`, the first starting
/// with a letter — at most 64 characters.
fn is_event_name(name: &str) -> bool {
    name.len() <= 64
        && name.split('.').enumerate().all(|(i, part)| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                && (i > 0 || part.as_bytes()[0].is_ascii_lowercase())
        })
}

/// `rows`: a lowercase letter, then lowercase letters, digits and `_` — at most 32 characters.
fn is_field_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A declared value: ASCII letters, digits, `_`, `-` and `.`, starting with a letter or digit — at
/// most 32 characters, so a value never carries a comma or a space.
fn is_value(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// One field's value, as the app hands it to [`Events::record`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Value {
    /// For a [`FieldKind::Count`].
    Count(u64),
    /// For a [`FieldKind::Bool`].
    Bool(bool),
    /// For a [`FieldKind::Choice`]: one of its declared values.
    Choice(String),
    /// For a [`FieldKind::Set`]: some of its declared values.
    Set(Vec<String>),
    /// For a [`FieldKind::Duration`].
    Duration(Duration),
}

/// What [`Events::record`] did.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Recorded {
    /// The event was written.
    Written,
    /// The event was not written. [`DROPPED_EVENT`] says so once per reason, event and field — except
    /// for [`Why::UnknownEvent`], whose name the registry does not declare and so is written nowhere.
    Dropped(Why),
    /// The event was written its most for this [`Events`] ([`Events::max_per_event`]); [`DROPPED_EVENT`]
    /// says so once.
    Capped,
}

/// Why an event was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Why {
    /// A field the registry does not declare for this event (its name is not written anywhere).
    UnknownField,
    /// A declared field whose value is not of its kind, a declared field left out, or one given twice.
    BadValue,
    /// An [`EventName`] from another registry — not written anywhere, not even as [`DROPPED_EVENT`].
    UnknownEvent,
}

/// One line of an events file, as read back — its shape only. [`Registry::verify`] makes an [`Event`]
/// of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventLine {
    name: String,
    session: String,
    version: String,
    os: String,
    arch: String,
    time: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, serde_json::Value>,
}

/// An event line a [`Registry`] verified: what goes out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event(EventLine);

impl Event {
    /// The event's name.
    pub fn name(&self) -> &str {
        &self.0.name
    }
    /// The random id of the events it was written among ([`Events::id`]) — not a session's.
    pub fn session(&self) -> &str {
        &self.0.session
    }
    /// The app's version.
    pub fn version(&self) -> &str {
        &self.0.version
    }
    /// The operating system and architecture.
    pub fn platform(&self) -> (&str, &str) {
        (&self.0.os, &self.0.arch)
    }
    /// When it was written, in UTC.
    pub fn time(&self) -> &str {
        &self.0.time
    }
    /// The fields, as written.
    pub fn fields(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.0.fields
    }
}

/// The events of one session, appended to a JSON Lines file the person can read — exactly what would
/// be sent. Each line carries a random id of these events' own — not the session's, which session lines
/// send beside the installation, so events cannot be joined to the installation through it — the app's
/// version, the platform and the time. Make one per session, from its [`Session`]; the cap per event
/// counts within one `Events`.
#[derive(Debug)]
pub struct Events {
    file: PathBuf,
    registry: Registry,
    head: (String, String, String, String),
    max_per_event: usize,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    written: HashMap<String, usize>,
    told: HashSet<(Why, String, Option<String>)>,
    capped: HashSet<String>,
}

impl Events {
    /// The events of `session`, declared by `registry`, appended to `file`.
    pub fn open(
        session: &Session,
        registry: Registry,
        file: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let (_, version, os, arch) = session.event_head();
        Ok(Events {
            file: file.into(),
            registry,
            head: (random_id()?, version, os, arch),
            max_per_event: MAX_PER_EVENT,
            state: Mutex::default(),
        })
    }

    /// The random id each line of these events carries.
    pub fn id(&self) -> &str {
        &self.head.0
    }

    /// Writes each event at most `max` times instead of [`MAX_PER_EVENT`] — so a chatty event cannot
    /// crowd the file.
    pub fn max_per_event(mut self, max: usize) -> Self {
        self.max_per_event = max;
        self
    }

    /// The file it appends to.
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Writes `event` with `fields` — every declared field, each of its kind — or drops it and says so.
    /// All or nothing: one field that does not fit drops the event. Errors are only the file's.
    pub fn record(&self, event: &EventName, fields: &[(&str, Value)]) -> io::Result<Recorded> {
        let Some(declared) = self.registry.events.get(&event.0) else {
            return Ok(Recorded::Dropped(Why::UnknownEvent));
        };
        let mut line = BTreeMap::new();
        let mut problem = None;
        for (field, value) in fields {
            match declared.get(*field) {
                None => {
                    problem.get_or_insert((Why::UnknownField, None));
                }
                Some(kind) => match written(kind, value) {
                    Some(v) if !line.contains_key(*field) => {
                        line.insert((*field).to_string(), v);
                    }
                    _ => {
                        problem.get_or_insert((Why::BadValue, Some((*field).to_string())));
                    }
                },
            }
        }
        if problem.is_none() {
            if let Some(missing) = declared.keys().find(|k| !line.contains_key(*k)) {
                problem = Some((Why::BadValue, Some(missing.clone())));
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // What this session has said is marked only once it is in the file: a write that fails is said
        // again next time.
        if let Some((why, field)) = problem {
            let key = (why, event.0.clone(), field.clone());
            if !state.told.contains(&key) {
                let reason = if why == Why::UnknownField {
                    "unknown_field"
                } else {
                    "bad_value"
                };
                let mut fields = BTreeMap::from([
                    ("reason".to_string(), reason.into()),
                    ("event".to_string(), event.0.clone().into()),
                ]);
                if let Some(field) = field {
                    fields.insert("field".to_string(), field.into());
                }
                append_line(&self.file, &self.line(DROPPED_EVENT, fields))?;
                state.told.insert(key);
            }
            return Ok(Recorded::Dropped(why));
        }
        let count = state.written.get(&event.0).copied().unwrap_or(0);
        if count >= self.max_per_event {
            if !state.capped.contains(&event.0) {
                let fields = BTreeMap::from([
                    ("reason".to_string(), "capped".into()),
                    ("event".to_string(), event.0.clone().into()),
                ]);
                append_line(&self.file, &self.line(DROPPED_EVENT, fields))?;
                state.capped.insert(event.0.clone());
            }
            return Ok(Recorded::Capped);
        }
        append_line(&self.file, &self.line(&event.0, line))?;
        state.written.insert(event.0.clone(), count + 1);
        Ok(Recorded::Written)
    }

    fn line(&self, name: &str, fields: BTreeMap<String, serde_json::Value>) -> EventLine {
        let (session, version, os, arch) = self.head.clone();
        EventLine {
            name: name.to_string(),
            session,
            version,
            os,
            arch,
            time: utc(std::time::SystemTime::now()),
            fields,
        }
    }
}

/// The reports `file` has gained since the offset `sent` records, each verified by `registry`
/// ([`Registry::verify_report`]) — a line that does not verify is passed over and counted as handled.
/// Read it as [`pending`].
pub fn pending_reports(
    registry: &Registry,
    file: &Path,
    sent: &Path,
) -> io::Result<Vec<Pending<Report>>> {
    Ok(pending::<Report>(file, sent)?
        .into_iter()
        .map(|p| Pending {
            items: p
                .items
                .into_iter()
                .filter_map(|report| registry.verify_report(report))
                .collect(),
            end: p.end,
        })
        .collect())
}

/// The events `file` has gained since the offset `sent` records, each verified by `registry` — a line
/// that does not verify (edited into the file, or from an older registry) is passed over and counted
/// as handled. Read it as [`pending`].
pub fn pending_events(
    registry: &Registry,
    file: &Path,
    sent: &Path,
) -> io::Result<Vec<Pending<Event>>> {
    Ok(pending::<EventLine>(file, sent)?
        .into_iter()
        .map(|p| Pending {
            items: p
                .items
                .into_iter()
                .filter_map(|line| registry.verify(line))
                .collect(),
            end: p.end,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstallId;

    const REGISTRY: &str = r#"{ "events": {
        "export.finished": { "fields": {
            "format": { "kind": "choice", "values": ["csv", "xlsx"] },
            "rows": { "kind": "count", "bucket": [0, 10, 100, 1000] },
            "took": { "kind": "duration" } } },
        "search.used": { "fields": {
            "filters": { "kind": "set", "values": ["date", "kind", "size"] },
            "results": { "kind": "count", "max": 50 },
            "empty": { "kind": "bool" } } },
        "app.opened": {} } }"#;

    fn setup() -> (tempfile::TempDir, Events, Registry) {
        let dir = tempfile::tempdir().unwrap();
        let install = InstallId::load_or_create(&dir.path().join("id")).unwrap();
        let session = Session::builder(&install, "1.2.3")
            .start(dir.path().join("s.jsonl"), dir.path().join("s.running"))
            .unwrap();
        let registry = Registry::from_json(REGISTRY).unwrap();
        let events =
            Events::open(&session, registry.clone(), dir.path().join("events.jsonl")).unwrap();
        assert_ne!(events.id(), session.id(), "events carry an id of their own");
        (dir, events, registry)
    }

    fn lines(events: &Events) -> Vec<serde_json::Value> {
        std::fs::read_to_string(events.file())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_declared_event_is_written_with_its_fields_in_their_kinds() {
        let (_dir, events, registry) = setup();
        let export = registry.event("export.finished").unwrap();
        let recorded = events
            .record(
                &export,
                &[
                    ("format", Value::Choice("csv".into())),
                    ("rows", Value::Count(437)),
                    ("took", Value::Duration(Duration::from_millis(1250))),
                ],
            )
            .unwrap();
        assert_eq!(recorded, Recorded::Written);
        let search = registry.event("search.used").unwrap();
        let _ = events
            .record(
                &search,
                &[
                    ("filters", Value::Set(vec!["size".into(), "date".into()])),
                    ("results", Value::Count(7)),
                    ("empty", Value::Bool(false)),
                ],
            )
            .unwrap();
        let _ = events
            .record(&registry.event("app.opened").unwrap(), &[])
            .unwrap();

        let written = lines(&events);
        assert_eq!(written[0]["name"], "export.finished");
        assert_eq!(
            written[0]["fields"],
            serde_json::json!({ "format": "csv", "rows": 100, "took": 1250 }),
            "437 rows → its bucket"
        );
        assert_eq!(
            written[1]["fields"],
            serde_json::json!({ "filters": ["date", "size"], "results": 7, "empty": false })
        );
        assert!(written[2].get("fields").is_none());
        assert!(
            written.iter().all(|l| l.get("install").is_none()),
            "no installation id on an event"
        );
    }

    #[test]
    fn what_does_not_fit_is_dropped_and_said_once_without_its_name_or_value() {
        let (_dir, events, registry) = setup();
        let export = registry.event("export.finished").unwrap();
        let all = |format: &str| {
            vec![
                ("format", Value::Choice(format.into())),
                ("rows", Value::Count(3)),
                ("took", Value::Duration(Duration::from_millis(5))),
            ]
        };
        assert_eq!(
            events
                .record(&export, &all("C:/Users/someone/salaries.xlsx"))
                .unwrap(),
            Recorded::Dropped(Why::BadValue)
        );
        assert_eq!(
            events.record(&export, &all("pdf")).unwrap(),
            Recorded::Dropped(Why::BadValue)
        );
        let mut extra = all("csv");
        extra.push(("filename", Value::Choice("salaries".into())));
        assert_eq!(
            events.record(&export, &extra).unwrap(),
            Recorded::Dropped(Why::UnknownField)
        );
        assert_eq!(
            events.record(&export, &all("csv")[..2]).unwrap(),
            Recorded::Dropped(Why::BadValue),
            "a declared field left out"
        );
        let search = registry.event("search.used").unwrap();
        let big = [
            ("filters", Value::Set(vec![])),
            ("results", Value::Count(51)),
            ("empty", Value::Bool(true)),
        ];
        assert_eq!(
            events.record(&search, &big).unwrap(),
            Recorded::Dropped(Why::BadValue),
            "over the cap"
        );
        let twice = [
            ("filters", Value::Set(vec![])),
            ("results", Value::Count(1)),
            ("results", Value::Count(2)),
            ("empty", Value::Bool(true)),
        ];
        assert_eq!(
            events.record(&search, &twice).unwrap(),
            Recorded::Dropped(Why::BadValue),
            "a field given twice — the same notice as over the cap, said once"
        );

        let written = lines(&events);
        let text = serde_json::to_string(&written).unwrap();
        assert!(
            !text.contains("salaries")
                && !text.contains("C:/")
                && !text.contains("filename")
                && !text.contains("pdf")
        );
        assert_eq!(
            written.len(),
            4,
            "format twice → said once; unknown field; took left out; results over cap"
        );
        assert!(written.iter().all(|l| l["name"] == DROPPED_EVENT));
        assert_eq!(
            written[0]["fields"],
            serde_json::json!({ "reason": "bad_value", "event": "export.finished", "field": "format" })
        );
        assert_eq!(
            written[1]["fields"],
            serde_json::json!({ "reason": "unknown_field", "event": "export.finished" })
        );
    }

    #[test]
    fn a_chatty_event_is_capped_and_said_once() {
        let (_dir, events, registry) = setup();
        let events = events.max_per_event(2);
        let opened = registry.event("app.opened").unwrap();
        let got: Vec<_> = (0..4)
            .map(|_| events.record(&opened, &[]).unwrap())
            .collect();
        assert_eq!(
            got,
            [
                Recorded::Written,
                Recorded::Written,
                Recorded::Capped,
                Recorded::Capped
            ]
        );
        let written = lines(&events);
        assert_eq!(written.len(), 3);
        assert_eq!(
            written[2]["fields"],
            serde_json::json!({ "reason": "capped", "event": "app.opened" })
        );
    }

    #[test]
    fn a_registry_that_could_carry_content_is_refused() {
        let refused = |json: &str| Registry::from_json(json).unwrap_err().to_string();
        assert!(
            refused(r#"{"events":{"x":{"fields":{"size":{"kind":"count"}}}}}"#)
                .contains("bucket or a max")
        );
        assert!(
            refused(r#"{"events":{"x":{"fields":{"size":{"kind":"count","max":1001}}}}}"#)
                .contains("max must be")
        );
        assert!(refused(
            r#"{"events":{"x":{"fields":{"size":{"kind":"count","bucket":[0,482113906,482113907]}}}}}"#
        )
        .contains("twice"));
        assert!(Registry::from_json(
            r#"{"events":{"x":{"fields":{"size":{"kind":"count","bucket":[0,1,2,4,1000000]}}}}}"#
        )
        .is_ok());
        assert!(refused(r#"{"events":{"x":{},"x":{}}}"#).contains("given twice"));
        assert!(refused(
            r#"{"events":{"x":{"fields":{"a":{"kind":"bool"},"a":{"kind":"bool"}}}}}"#
        )
        .contains("given twice"));
        assert!(
            refused(r#"{"events":{"x":{"fields":{"n":{"kind":"count","bucket":[1,10]}}}}}"#)
                .contains("start at 0")
        );
        assert!(refused(
            r#"{"events":{"x":{"fields":{"who":{"kind":"choice","values":["a b"]}}}}}"#
        )
        .contains("plain identifiers"));
        assert!(
            refused(r#"{"events":{"x":{"fields":{"who":{"kind":"choice","values":[]}}}}}"#)
                .contains("values")
        );
        assert!(
            refused(r#"{"events":{"x":{"fields":{"name":{"kind":"text"}}}}}"#)
                .contains("not a registry")
        );
        assert!(refused(r#"{"events":{"kit.mine":{}}}"#).contains("crate's own"));
        assert!(refused(r#"{"events":{"Export":{}}}"#).contains("plain lowercase"));
        assert!(refused(
            r#"{"events":{"x":{"fields":{"t":{"kind":"duration","max_ms":999999999999}}}}}"#
        )
        .contains("one day"));
        assert!(
            Registry::from_json(REGISTRY)
                .unwrap()
                .event("export.finshed")
                .is_err(),
            "a misspelled name fails at start"
        );
    }

    #[test]
    fn only_lines_the_registry_would_write_are_read_back_for_sending() {
        let (dir, events, registry) = setup();
        let opened = registry.event("app.opened").unwrap();
        let _ = events.record(&opened, &[]).unwrap();
        let _ = events
            .record(
                &registry.event("search.used").unwrap(),
                &[("results", Value::Count(99))],
            )
            .unwrap();
        let good = std::fs::read_to_string(events.file()).unwrap();
        let first: serde_json::Value = serde_json::from_str(good.lines().next().unwrap()).unwrap();
        let mut edited = first.clone();
        edited["name"] = "export.finished".into();
        edited["fields"] = serde_json::json!({ "format": "my secret.docx", "rows": 0, "took": 1 });
        let mut odd_bucket = first.clone();
        odd_bucket["name"] = "export.finished".into();
        odd_bucket["fields"] = serde_json::json!({ "format": "csv", "rows": 437, "took": 1 });
        std::fs::write(events.file(), format!("{good}{edited}\n{odd_bucket}\n")).unwrap();

        let read =
            pending_events(&registry, events.file(), &dir.path().join("events.sent")).unwrap();
        let names: Vec<_> = read
            .iter()
            .flat_map(|p| p.items.iter().map(|e| e.name().to_string()))
            .collect();
        assert_eq!(
            names,
            ["app.opened", DROPPED_EVENT],
            "the edited and off-bucket lines are not sent"
        );
    }
}
