# tauri-kit

Building blocks for desktop apps made with [Tauri](https://tauri.app) — the capabilities most such
apps end up writing for themselves, each in its own small crate so an app takes only what it needs.

## Crates

| Crate | What it gives an app |
|---|---|
| [`tauri-kit-credentials`](crates/credentials) | **Secrets in the OS credential store** — Windows Credential Manager, macOS Keychain, the Secret Service on Linux. Test runs and debug builds get their own entries, decided at compile time where the app writes `build_kind!()`, so running the test suite or a development build never overwrites or clears the secrets of the installed app. |
| [`tauri-kit-sidecar`](crates/sidecar) | **Bundled helper processes that behave.** No console window on Windows; the helper and everything it starts are stopped together — on shutdown, on drop, and on Windows even when the app crashes (job object); a readiness wait that returns the moment the helper exits instead of waiting out its deadline — by probing, or by waiting for the line a helper prints when it is ready (often with the port it chose); stdout/stderr drained so the helper never blocks on a full pipe; and for a helper that does one job and exits, a run with a deadline that hands back what it printed and stops its whole tree when time runs out. A supervisor keeps a helper running from the function that starts it: it notices the helper ending (or, with a health probe, no longer answering), starts it again within a limit per time window and then leaves it down, swaps the new start's endpoint in with the process, reports each step, and stops it for good — before an update installs — undoing a start still under way. With the `loopback` feature, how an app talks to a local HTTP backend: a fresh token per run handed over in an environment variable and the port the helper announces on its readiness line — or, for a backend that lives on its own, an owner-only discovery file it publishes — ending in an `Endpoint` (address and authorization header) for whatever HTTP client the app uses; `loopback-client` adds a blocking one that never goes through a proxy. Its .NET side is in [`dotnet/`](dotnet) (NuGet `TauriKit.Sidecar.Loopback.AspNetCore` for the host, and the framework-free `TauriKit.Sidecar.Loopback` for a caller that finds it by its file; same version): callers each with their own credential and kind, every route closed unless named open, a `Host` check, a shutdown route, and a watch that stops the host when the app that started it is gone. |
| [`tauri-kit-fs`](crates/fs) | **Crash-safe file writes.** Write, flush to the device, then rename into place, so a crash or power loss leaves the old content or the new — never a truncated file. A create-only variant that never replaces what is already there. On Windows, the brief refusals caused by virus scanners, indexers and sync clients holding a file open are retried — and the same retry is available for the app's own file operations. Optional staging directory for apps whose folders are watched or synced, and a startup sweep that removes only leftovers named the way the app says. A compare-and-replace write that refuses to overwrite a file changed since the app read it. Paths kept inside a folder the app works in (no `..`, absolute paths or links leading out). Whether a location has a trash to restore from, and crash-safe line appends for logs. New files and folders under the next free name (`name (1)`), without racing another program for it. Recognises the conflict copies sync clients leave beside a file changed on two devices, and names the file each is a copy of. An opt-in `test-hooks` feature lets the app's tests stop a write between its steps, as a crash would. |
| [`tauri-kit-state`](crates/state) | **State the app keeps for each folder a person opens — outside that folder.** A directory name for each folder that is recognisable, unambiguous and the same from one release to the next; a format number with the steps between formats, so older state is brought up to date and state written by a newer release is left alone rather than rewritten; a file that cannot be read set aside instead of lost; and an outbox that keeps writes the app could not make yet as files, so they survive the app closing. |
| [`tauri-kit-watch`](crates/watch) | **Changes made by other programs, and only those.** A folder watch that gathers the platform's notifications into batches and reports each changed path once, as it now is on disk — written or removed. The app's own writes are left out by their content, however many notifications a write takes; `tauri-kit-fs` temporary files and paths the app names are ignored (a folder and all it holds, or files alone); and when the platform drops notifications — or a path the app names changes, such as the branch a repository has checked out — the app is told to read the folder again. |
| [`tauri-kit-diagnostics`](crates/diagnostics) | **Error reports that carry no content.** A report is built from an allowlist only — the layer that failed, the kind of failure when it is a plain identifier, details the app names when they are plain identifiers or whole numbers (a status code, say), and the frames of the app's own code (scripts of its web bundle, its Rust source, .NET method names) — never from a message, so a path, a file name or a value the person typed cannot leave the device in one. Each launch writes each failure once and caps how many it writes, to a JSON Lines file the person can read — exactly what would be sent — kept from growing without end. An opt-in `appinsights` feature sends what the file gained since the last send to Azure Application Insights, through the OS's TLS and certificate store and system proxy (or `appinsights-rustls`: rustls against the same certificate store, for an app that keeps OpenSSL out), and keeps for the next launch what the endpoint could not take yet. Without that feature it still makes the items and reads what is unsent, for an app that sends with an HTTP client of its own. The same path carries **sessions**: when the app starts and ends, a session that did not end, and time on each of a fixed list of the app's screens — under a random installation id made on the device, never from its hardware, with no field for what a screen showed — as Application Insights users, sessions and page views. |
| [`tauri-kit-webview`](crates/webview) | **The web view runtime under the window.** Tells whether a WebView2 runtime that can start is installed — asked of the runtime's own loader, so a registration whose files are gone counts as missing — and shows the app's own message, in the person's words, before any window exists. Clears the form entries and saved passwords a web view profile remembered, and removes the copies of the profile WebView2 keeps from before its updates, which would carry those entries along. For test runs of a debug build, gives the window the browser arguments a test harness put in `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` — which current runtimes do not apply on their own to an app that passes arguments of its own, as a Tauri app always does. No `tauri` dependency: the app hands over the web view controller it already holds. |
| [`tauri-kit-update`](crates/update) | **Updates that land without getting in the way.** Looks for a newer release on a schedule kept in a small state file, so it survives launches (and a clock that went backwards); announces each version once; downloads its installer in the background, once — a download that failed waits for the next look; installs it when the app closes, or now when the person asks, after a `before_install` where the app stops its helpers; takes back an installer kept from an earlier launch and lets go of one already installed. Where releases come from is a `Source` of three blocking methods — Tauri's updater plugin in a few lines (shown in the docs), or an app's own — run on the crate's thread. Turning it off is the app's choice to keep. Ships `nsis/wait-for-exit.nsh`, an installer hook that waits in update mode for the closing app and a named helper to let go of their files. |

More capabilities will be added as separate crates. What belongs here, and what deliberately does not, is in [SCOPE.md](SCOPE.md).

## Usage

```toml
[dependencies]
tauri-kit-fs = "0.21"
tauri-kit-credentials = "0.21"
tauri-kit-sidecar = "0.21"
tauri-kit-watch = "0.21"
tauri-kit-diagnostics = "0.21"
tauri-kit-webview = "0.21"
tauri-kit-state = "0.21"
tauri-kit-update = "0.21"
```

The crates are published to crates.io. Depending on them from this repository instead
(`{ git = "https://github.com/iyulab/tauri-kit", tag = "v0.21.0" }`) also works, but take the crates
you need in one edit: Cargo re-resolves everything reachable from a git source's already-locked
crates when another crate from the same source is added later, which can move unrelated entries of
`Cargo.lock` — crates that accept a range of versions of a shared dependency (such as `windows-sys`
or `getrandom`) may land on a different one.

### Versioning

The crates share one version and are released together. Each release is a `vX.Y.Z` tag on `main`, and
the tag always equals the `version` in `Cargo.toml`, and [CHANGELOG.md](CHANGELOG.md) lists what each release changed. While the version is `0.x`, a minor bump (`0.1` → `0.2`)
may break the API; a patch bump does not. Pin a tag, not a branch. The .NET package in `dotnet/` carries the same version: after the crates are
published with `cargo publish --workspace`, the `Publish NuGet` workflow, run on the release tag
(`gh workflow run publish-nuget.yml --ref vX.Y.Z`), tests, packs and pushes it.

```rust
use std::path::Path;

tauri_kit_fs::write_atomic(Path::new("settings.json"), br#"{"theme":"dark"}"#)?;
// Fails with ErrorKind::AlreadyExists instead of replacing a file that is already there.
tauri_kit_fs::write_atomic_new(Path::new("report.md"), b"# Report")?;
// Renames a file or folder only if the new name is free; a sync client sees a move.
tauri_kit_fs::rename_new(Path::new("report.md"), Path::new("2026 report.md"))?;
```

```rust
use tauri_kit_credentials::{build_kind, Credentials};

let credentials = Credentials::new("com.example.app", build_kind!());
credentials.set("api-key", "s3cret")?;
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) — start with the scope.

## License

[MIT](LICENSE)
