//! What a report holds, and the rules that pick a layer's frames out of a stack.

use serde::{Deserialize, Serialize};

/// Enough of a stack to tell one failure from another: the frames a [`Layer`] keeps by default.
pub const MAX_FRAMES: usize = 20;

/// The kind a report gets when what it was handed is not a plain identifier.
pub const UNRECOGNIZED_KIND: &str = "Unrecognized";

/// One error report, exactly as it is written and sent.
///
/// Every field comes from an allowlist: the layer is the app's own name, the kind is a plain
/// identifier or [`UNRECOGNIZED_KIND`], the frames are frames of the app's own code, and the rest
/// is the app's version, the platform and the time. Serialized, it is one JSON object, and the
/// layer is the name the app gave it, as a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// The name of the [`Layer`] that failed.
    pub layer: String,
    /// What failed: a type or class name, or a code of the app's own.
    pub kind: String,
    /// Frames of the app's own code that led to the failure, innermost first, as the layer's
    /// [`FrameRule`] writes them.
    pub frames: Vec<String>,
    /// The app's version, as the app passed it.
    pub version: String,
    /// The operating system, as [`std::env::consts::OS`] names it.
    pub os: String,
    /// The CPU architecture, as [`std::env::consts::ARCH`] names it.
    pub arch: String,
    /// When it failed, in UTC to the second (ISO 8601) — reports are often sent on a later launch.
    pub time: String,
}

impl Report {
    /// A report from what a layer says about a failure.
    ///
    /// `kind` and `stack` are untrusted: a `kind` that is not a plain identifier (an ASCII letter,
    /// then letters, digits, `_`, `.`, `:` or `-`, at most 100 characters) becomes
    /// [`UNRECOGNIZED_KIND`] — whole, never in part — and only the lines of `stack` that the
    /// layer's [`FrameRule`] recognises as the app's own code are kept, at most
    /// [`Layer::max_frames`] of them. Nothing else of either is taken. `version` is the app's
    /// version, typically `env!("CARGO_PKG_VERSION")` of the app's crate.
    pub fn new(layer: &Layer, kind: &str, stack: &str, version: &str) -> Self {
        Report {
            layer: layer.name.clone(),
            kind: plain_kind(kind),
            frames: stack
                .lines()
                .filter_map(|line| layer.rule.frame(line))
                .take(layer.max_frames)
                .collect(),
            version: version.to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            time: utc(std::time::SystemTime::now()),
        }
    }

    /// A report of the same layer and version with only `kind`, and no frames.
    pub(crate) fn of_kind(&self, kind: &str) -> Self {
        Report {
            layer: self.layer.clone(),
            kind: kind.to_string(),
            frames: Vec::new(),
            version: self.version.clone(),
            os: self.os.clone(),
            arch: self.arch.clone(),
            time: self.time.clone(),
        }
    }
}

/// A part of the app that fails on its own — its UI, its Rust process, a helper process — and
/// the rule that tells its own frames from everything else in a stack it hands over.
///
/// The name is written into every report of the layer as it is given, so it should be a fixed
/// name of the app's own, such as `"ui"` or `"host"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    name: String,
    rule: FrameRule,
    max_frames: usize,
}

impl Layer {
    /// A layer named `name`, whose frames `rule` picks, keeping at most [`MAX_FRAMES`] of them.
    pub fn new(name: impl Into<String>, rule: impl Into<FrameRule>) -> Self {
        Layer {
            name: name.into(),
            rule: rule.into(),
            max_frames: MAX_FRAMES,
        }
    }

    /// Keeps at most `max` frames of a stack (the innermost ones) instead of [`MAX_FRAMES`].
    pub fn max_frames(mut self, max: usize) -> Self {
        self.max_frames = max;
        self
    }

    /// The name reports of this layer carry.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The rule that picks this layer's frames.
    pub fn rule(&self) -> &FrameRule {
        &self.rule
    }
}

/// How a layer tells a frame of the app's own code from anything else in a stack, and what it
/// keeps of it. A line no rule recognises is dropped whole.
///
/// Each line may start with `at ` and `async ` (as JavaScript and .NET print them), and may be
/// `function (location)` or `function in location`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameRule {
    /// Scripts served from the app's own web bundle. See [`WebBundle`].
    WebBundle(WebBundle),
    /// Source files of the app's own Rust crate. See [`RustSource`].
    RustSource(RustSource),
    /// .NET methods, by name only — `Namespace.Type.Method` as a .NET stack trace or a list of
    /// method names gives it, with the arguments dropped. File names are never kept: in a .NET
    /// stack they are paths on the machine that built it.
    DotNetMethod,
}

impl FrameRule {
    /// [`FrameRule::WebBundle`] with [`WebBundle::default`].
    pub fn web_bundle() -> Self {
        FrameRule::WebBundle(WebBundle::default())
    }

    /// [`FrameRule::RustSource`] with [`RustSource::default`].
    pub fn rust_source() -> Self {
        FrameRule::RustSource(RustSource::default())
    }

    /// [`FrameRule::DotNetMethod`].
    pub fn dotnet_method() -> Self {
        FrameRule::DotNetMethod
    }

    /// One frame of the app's own code, or nothing.
    fn frame(&self, line: &str) -> Option<String> {
        let line = line.trim();
        let line = line.strip_prefix("at ").unwrap_or(line);
        let line = line.strip_prefix("async ").unwrap_or(line);
        let (function, location) = match line.rsplit_once(" (") {
            Some((function, rest)) if rest.ends_with(')') => {
                (Some(function), &rest[..rest.len() - 1])
            }
            _ => match line.split_once(" in ") {
                Some((function, _)) => (Some(function), ""),
                None => (None, line),
            },
        };
        match self {
            FrameRule::DotNetMethod => {
                // `Type.Method` as listed, or `Type.Method(args) in file:line N` as .NET prints it.
                let function = function.unwrap_or(line);
                let function = function.split('(').next().unwrap_or(function);
                function_name(function).map(str::to_string)
            }
            FrameRule::WebBundle(rule) => {
                let (url, at) = position(location)?;
                let file = rule.own_path(url)?;
                let file = rule.strip_prefix(file)?;
                let file = source_file(file, &rule.extensions)?;
                let function = function.and_then(function_name).unwrap_or("?");
                Some(format!("{function} {file}:{at}"))
            }
            FrameRule::RustSource(rule) => {
                let (path, at) = position(location)?;
                let rest = rule.strip_root(path)?;
                let mut segments: Vec<&str> = rest.split(['/', '\\']).collect();
                let file = source_file(segments.pop()?, &rule.extensions)?;
                // Folders inside the crate's own source are kept, each a plain name as a file is —
                // never `.` or `..`, so a frame cannot climb out of the source root.
                let folders_plain = segments
                    .iter()
                    .all(|s| plain_name(s) && !s.starts_with('.'));
                if !folders_plain {
                    return None;
                }
                segments.push(file);
                Some(format!("{}:{at}", segments.join("/")))
            }
        }
    }
}

impl From<WebBundle> for FrameRule {
    fn from(rule: WebBundle) -> Self {
        FrameRule::WebBundle(rule)
    }
}

impl From<RustSource> for FrameRule {
    fn from(rule: RustSource) -> Self {
        FrameRule::RustSource(rule)
    }
}

/// Frames of scripts the app itself serves, kept as `function file:line:col`.
///
/// A frame is kept only when its script URL is `http://` or `https://` on one of the app's own
/// hosts (by default `tauri.localhost`, where Tauri serves the bundle on Windows) or, with
/// [`WebBundle::dev_server`] on (the default), `localhost:<port>` — the dev server while
/// developing — or is on one of the app's own schemes, whatever the host (by default `tauri`, as
/// in `tauri://localhost`, where Tauri serves the bundle on macOS and Linux). The path must start with one of the prefixes (by default `assets/`, where a
/// bundler puts built scripts, or `src/`, where a dev server serves sources), which is dropped,
/// and what is left must be a plain file name with one of the extensions (by default `.js`,
/// `.mjs`, `.ts`). A function name that is not a plain identifier path is written as `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebBundle {
    hosts: Vec<String>,
    schemes: Vec<String>,
    dev_server: bool,
    prefixes: Vec<String>,
    extensions: Vec<String>,
}

impl Default for WebBundle {
    fn default() -> Self {
        WebBundle {
            hosts: vec!["tauri.localhost".into()],
            schemes: vec!["tauri".into()],
            dev_server: true,
            prefixes: vec!["assets/".into(), "src/".into()],
            extensions: vec![".js".into(), ".mjs".into(), ".ts".into()],
        }
    }
}

impl WebBundle {
    /// The hosts the app's bundle is served from, matched exactly (with any port), replacing
    /// `tauri.localhost`.
    pub fn hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// The URL schemes only the app serves (a custom protocol), on which every host is the app's
    /// own, replacing `tauri`. `http` and `https` are never among them — those go by host.
    pub fn schemes<I, S>(mut self, schemes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.schemes = schemes
            .into_iter()
            .map(Into::into)
            .filter(|s| s != "http" && s != "https")
            .collect();
        self
    }

    /// Whether `localhost:<port>` counts as the app's own (on by default).
    pub fn dev_server(mut self, on: bool) -> Self {
        self.dev_server = on;
        self
    }

    /// The path prefixes a script must be under, dropped from the frame. A script under none of
    /// them is not kept. An empty list requires no prefix.
    pub fn prefixes<I, S>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.prefixes = prefixes.into_iter().map(Into::into).collect();
        self
    }

    /// The file extensions of scripts, with the dot.
    pub fn extensions<I, S>(mut self, extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extensions = extensions.into_iter().map(Into::into).collect();
        self
    }

    /// The path of a script the app itself serves.
    fn own_path<'a>(&self, url: &'a str) -> Option<&'a str> {
        let (scheme, rest) = url.split_once("://")?;
        if self.schemes.iter().any(|s| s == scheme) {
            return rest.split_once('/').map(|(_, path)| path);
        }
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        let port_ok = port.is_none_or(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        let own = port_ok
            && (self.hosts.iter().any(|h| h == host)
                || (self.dev_server && host == "localhost" && port.is_some()));
        own.then_some(path)
    }

    fn strip_prefix<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.prefixes.is_empty() {
            return Some(path);
        }
        self.prefixes
            .iter()
            .find_map(|prefix| path.strip_prefix(prefix.as_str()))
    }
}

/// Frames of the app's own Rust source, kept as `file.rs:line:col`.
///
/// A frame is kept only when its location is a path under one of the source roots (by default
/// `src`, separated by `/` or `\` — a panic's location is relative to the crate) followed by
/// plain folder names, if any, and a plain file name with one of the extensions (by default
/// `.rs`); it is written with `/` (`commands/open.rs:12:5`). Frames of other crates, which carry
/// absolute paths into the build machine's registry, are dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustSource {
    roots: Vec<String>,
    extensions: Vec<String>,
}

impl Default for RustSource {
    fn default() -> Self {
        RustSource {
            roots: vec!["src".into()],
            extensions: vec![".rs".into()],
        }
    }
}

impl RustSource {
    /// The source roots, relative and without a trailing separator, replacing `src`.
    pub fn roots<I, S>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.roots = roots.into_iter().map(Into::into).collect();
        self
    }

    /// The file extensions of source files, with the dot, replacing `.rs`.
    pub fn extensions<I, S>(mut self, extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extensions = extensions.into_iter().map(Into::into).collect();
        self
    }

    fn strip_root<'a>(&self, path: &'a str) -> Option<&'a str> {
        self.roots
            .iter()
            .find_map(|root| path.strip_prefix(root.as_str())?.strip_prefix(['/', '\\']))
    }
}

/// `time` as ISO 8601 in UTC, to the second.
pub(crate) fn utc(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since the epoch to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// A type or class name, or an app-owned code — whole, or not at all: a kind that is not a plain
/// identifier may be text from anywhere, and a part of it is still that text.
fn plain_kind(raw: &str) -> String {
    let mut chars = raw.chars();
    let plain = raw.len() <= 100
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'));
    if plain {
        raw.to_string()
    } else {
        UNRECOGNIZED_KIND.to_string()
    }
}

/// A function name as a stack prints it, if it is only an identifier path.
fn function_name(raw: &str) -> Option<&str> {
    let plain = !raw.is_empty()
        && raw.len() <= 200
        && raw.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '<' | '>' | '+' | '`')
        });
    plain.then_some(raw)
}

/// A file's name, if it is a plain ASCII file name with one of `extensions`.
fn source_file<'a>(raw: &'a str, extensions: &[String]) -> Option<&'a str> {
    (plain_name(raw) && extensions.iter().any(|ext| raw.ends_with(ext.as_str()))).then_some(raw)
}

/// A file or folder name of ASCII letters, digits, `_`, `.` and `-` only.
fn plain_name(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// `file:line:col` split into the file and `line:col` (digits only).
fn position(location: &str) -> Option<(&str, &str)> {
    let (rest, col) = location.rsplit_once(':')?;
    let (file, line) = rest.rsplit_once(':')?;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    (digits(line) && digits(col)).then(|| (file, &location[file.len() + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: &str = "1.2.3";

    fn ui() -> Layer {
        Layer::new("ui", FrameRule::web_bundle())
    }

    fn host() -> Layer {
        Layer::new("host", FrameRule::dotnet_method())
    }

    fn shell() -> Layer {
        Layer::new("shell", FrameRule::rust_source())
    }

    /// Strings no report may carry, whatever a layer hands over: paths of the person's files
    /// (Windows, the `\\?\` form, POSIX, relative), file and folder names, a person's name, a
    /// template reference and field values.
    const FORBIDDEN: &[&str] = &[
        "C:\\Users",
        "\\\\?\\",
        "/home/",
        "/Users/",
        "someone",
        "Documents",
        "notes",
        "client",
        ".md",
        "Jane",
        "Müller",
        "invoice@1",
        "invoice",
        "salary",
        "85000",
        "secret",
    ];

    fn assert_clean(report: &Report) {
        let wire = serde_json::to_string(report).unwrap();
        for bad in FORBIDDEN {
            assert!(!wire.contains(bad), "{bad:?} reached the report: {wire}");
        }
        assert!(
            wire.is_ascii(),
            "only identifiers and file names of the app's own code: {wire}"
        );
    }

    #[test]
    fn no_field_can_carry_a_path_a_name_a_template_ref_or_a_field_value() {
        let corpus = [
            (
                "Error",
                "at open (C:\\Users\\someone\\Documents\\notes\\client.md:1:1)",
            ),
            (
                "IOException: C:\\Users\\someone\\Documents\\notes\\2026-09-29 client.md",
                "",
            ),
            (
                "Error",
                "at read (\\\\?\\C:\\Users\\someone\\Documents\\notes\\a.md:3:9)",
            ),
            (
                "Error",
                "at read (/home/someone/notes/a.md:3:9)\n    at x (/Users/someone/Documents/b.md:1:1)",
            ),
            (
                "invoice@1",
                "at suggest (http://tauri.localhost/notes/client.md:2:2)",
            ),
            (
                "RecordKeyException notes/client.md",
                "at Example.Store.Accept(RecordKey key) in notes/client.md:line 3",
            ),
            (
                "{\"salary\":\"85000\",\"manager\":\"Jane Müller\"}",
                "{\"secret\":1}",
            ),
            (
                "Error",
                "at open (http://tauri.localhost/assets/notes/client.md:1:1)",
            ),
            (
                "Error",
                "at Jane Müller (http://tauri.localhost/assets/index-a.js:1:1)\n/home/someone/Müller/client.rs:4:2",
            ),
            ("Jane Müller", "Jane Müller\nsrc/Müller.rs:1:1"),
            (
                "Error",
                "at open (tauri://localhost/assets/notes/client.md:1:1)\nsrc/../someone/client.rs:1:1\nsrc/Jane Müller/a.rs:1:1",
            ),
        ];
        for (kind, stack) in corpus {
            for layer in [ui(), host(), shell()] {
                assert_clean(&Report::new(&layer, kind, stack, VERSION));
            }
        }
    }

    #[test]
    fn keeps_a_plain_kind_and_the_frames_of_the_apps_own_bundle() {
        let report = Report::new(
            &ui(),
            "TypeError",
            "TypeError: x is undefined\n    at Editor.save (http://tauri.localhost/assets/index-Bx12.js:3:1204)\n    at async open (http://tauri.localhost/assets/index-Bx12.js:9:55)\n    at http://tauri.localhost/assets/vendor-9a.js:1:2\n    at load (https://example.com/assets/index.js:1:1)",
            VERSION,
        );
        assert_eq!(report.layer, "ui");
        assert_eq!(report.kind, "TypeError");
        assert_eq!(report.version, VERSION);
        assert_eq!(
            report.frames,
            [
                "Editor.save index-Bx12.js:3:1204",
                "open index-Bx12.js:9:55",
                "? vendor-9a.js:1:2"
            ]
        );
    }

    #[test]
    fn keeps_frames_from_the_dev_server() {
        let report = Report::new(
            &ui(),
            "TypeError",
            "at render (http://localhost:1420/src/main.ts:4:7)\nat render (http://localhost/src/main.ts:4:7)\nat render (http://localhost:x/src/main.ts:4:7)",
            VERSION,
        );
        assert_eq!(report.frames, ["render main.ts:4:7"]);

        let no_dev = Layer::new("ui", WebBundle::default().dev_server(false));
        let report = Report::new(
            &no_dev,
            "TypeError",
            "at render (http://localhost:1420/src/main.ts:4:7)",
            VERSION,
        );
        assert!(report.frames.is_empty());
    }

    #[test]
    fn keeps_frames_of_the_bundle_served_on_the_apps_own_scheme() {
        let report = Report::new(
            &ui(),
            "TypeError",
            "at save (tauri://localhost/assets/index-a.js:3:4)\nat b (asset://localhost/assets/x.js:1:1)\nat c (file:///home/someone/assets/x.js:1:1)",
            VERSION,
        );
        assert_eq!(report.frames, ["save index-a.js:3:4"]);

        let custom = Layer::new("ui", WebBundle::default().schemes(["app", "https"]));
        let report = Report::new(
            &custom,
            "TypeError",
            "at a (app://bundle/assets/a.js:1:1)\nat b (tauri://localhost/assets/b.js:1:1)\nat c (https://example.com/assets/c.js:1:1)\nat d (http://tauri.localhost/assets/d.js:1:1)",
            VERSION,
        );
        assert_eq!(
            report.frames,
            ["a a.js:1:1", "d d.js:1:1"],
            "https still goes by host"
        );
    }

    #[test]
    fn a_web_bundle_rule_takes_its_own_hosts_prefixes_and_extensions() {
        let rule = WebBundle::default()
            .hosts(["app.local"])
            .prefixes(["static/js/"])
            .extensions([".js"]);
        let layer = Layer::new("web", rule);
        let report = Report::new(
            &layer,
            "TypeError",
            "at a (https://app.local/static/js/main.js:1:2)\nat b (http://tauri.localhost/assets/main.js:1:2)\nat c (https://app.local/static/js/main.ts:1:2)\nat d (https://app.local/assets/main.js:1:2)",
            VERSION,
        );
        assert_eq!(report.frames, ["a main.js:1:2"]);

        let anywhere = Layer::new("web", WebBundle::default().prefixes(Vec::<String>::new()));
        let report = Report::new(
            &anywhere,
            "TypeError",
            "at a (http://tauri.localhost/main.js:1:2)\nat b (http://tauri.localhost/x/main.js:1:2)",
            VERSION,
        );
        assert_eq!(report.frames, ["a main.js:1:2"]);
    }

    #[test]
    fn keeps_an_app_owned_code_and_a_dotted_type_name() {
        assert_eq!(
            Report::new(&ui(), "outside-root", "", VERSION).kind,
            "outside-root"
        );
        assert_eq!(
            Report::new(
                &host(),
                "Example.Core.Errors.StoreUnavailableException",
                "",
                VERSION
            )
            .kind,
            "Example.Core.Errors.StoreUnavailableException"
        );
    }

    #[test]
    fn a_kind_that_is_not_an_identifier_is_not_kept_in_part() {
        for raw in ["invoice@1", "IOException: notes/a.md", "", "a b", "Müller"] {
            assert_eq!(
                Report::new(&ui(), raw, "", VERSION).kind,
                UNRECOGNIZED_KIND,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn keeps_the_method_names_a_host_failure_lists() {
        let report = Report::new(
            &host(),
            "System.ArgumentException",
            "Example.Host.Projection.IngestAsync
Example.Host.Projection+<IngestAsync>d__12.MoveNext
   at Example.Host.Store.Load(String path) in C:\\build\\Store.cs:line 42
notes/client.md",
            VERSION,
        );
        assert_eq!(
            report.frames,
            [
                "Example.Host.Projection.IngestAsync",
                "Example.Host.Projection+<IngestAsync>d__12.MoveNext",
                "Example.Host.Store.Load"
            ]
        );
    }

    #[test]
    fn keeps_a_rust_frame_only_from_the_apps_own_source() {
        let report = Report::new(
            &shell(),
            "Panic",
            "src\\host.rs:120:9\nC:\\Users\\someone\\.cargo\\registry\\src\\ureq-3.0\\src\\lib.rs:5:1\nsrc/store.rs:88:13\nsrc/commands/open.rs:1:1\nsrc\\commands\\vault\\save.rs:2:3",
            VERSION,
        );
        assert_eq!(
            report.frames,
            [
                "host.rs:120:9",
                "store.rs:88:13",
                "commands/open.rs:1:1",
                "commands/vault/save.rs:2:3"
            ]
        );

        // A folder is a plain name, never `.`/`..` or a path of its own.
        let report = Report::new(
            &shell(),
            "Panic",
            "src/../../Users/someone/a.rs:1:1\nsrc/./a.rs:1:1\nsrc/.hidden/a.rs:1:1\nsrc/Müller/a.rs:1:1\nsrc/my notes/a.rs:1:1\nsrc//a.rs:1:1\nsrc/C:/a.rs:1:1",
            VERSION,
        );
        assert!(report.frames.is_empty(), "{:?}", report.frames);

        let custom = Layer::new("shell", RustSource::default().roots(["app/src"]));
        let report = Report::new(
            &custom,
            "Panic",
            "app/src/main.rs:3:4\nsrc/main.rs:3:4",
            VERSION,
        );
        assert_eq!(report.frames, ["main.rs:3:4"]);
    }

    #[test]
    fn keeps_at_most_the_frames_a_layer_allows() {
        let stack: String = (0..30).map(|n| format!("src/f{n}.rs:1:1\n")).collect();
        assert_eq!(
            Report::new(&shell(), "Panic", &stack, VERSION).frames.len(),
            MAX_FRAMES
        );
        let report = Report::new(&shell().max_frames(2), "Panic", &stack, VERSION);
        assert_eq!(report.frames, ["f0.rs:1:1", "f1.rs:1:1"]);
    }

    #[test]
    fn the_layer_is_written_as_its_name() {
        let report = Report::new(&ui(), "TypeError", "", VERSION);
        let wire: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(wire["layer"], "ui");
        let line = r#"{"layer":"host","kind":"TypeError","frames":["a"],"version":"0.1.0","os":"windows","arch":"x86_64","time":"2026-09-29T09:26:31Z"}"#;
        let read: Report = serde_json::from_str(line).unwrap();
        assert_eq!(read.layer, "host");
        assert_eq!(read.frames, ["a"]);
    }

    #[test]
    fn writes_utc_time() {
        let at = |s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s);
        assert_eq!(utc(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(utc(at(951_782_400)), "2000-02-29T00:00:00Z");
    }
}
