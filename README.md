# tauri-kit

Building blocks for desktop apps made with [Tauri](https://tauri.app) — the capabilities most such
apps end up writing for themselves, each in its own small crate so an app takes only what it needs.

## Crates

| Crate | What it gives an app |
|---|---|
| [`tauri-kit-credentials`](crates/credentials) | **Secrets in the OS credential store** — Windows Credential Manager, macOS Keychain, the Secret Service on Linux. Test runs and debug builds get their own entries, decided at compile time where the app writes `build_kind!()`, so running the test suite or a development build never overwrites or clears the secrets of the installed app. |
| [`tauri-kit-sidecar`](crates/sidecar) | **Bundled helper processes that behave.** No console window on Windows; the helper and everything it starts are stopped together — on shutdown, on drop, and on Windows even when the app crashes (job object); a readiness wait that returns the moment the helper exits instead of waiting out its deadline — by probing, or by waiting for the line a helper prints when it is ready (often with the port it chose); stdout/stderr drained so the helper never blocks on a full pipe. |
| [`tauri-kit-fs`](crates/fs) | **Crash-safe file writes.** Write, flush to the device, then rename into place, so a crash or power loss leaves the old content or the new — never a truncated file. A create-only variant that never replaces what is already there. On Windows, the brief refusals caused by virus scanners, indexers and sync clients holding a file open are retried — and the same retry is available for the app's own file operations. Optional staging directory for apps whose folders are watched or synced, and a startup sweep that removes only leftovers named the way the app says. An opt-in `test-hooks` feature lets the app's tests stop a write between its steps, as a crash would. |
| [`tauri-kit-watch`](crates/watch) | **Changes made by other programs, and only those.** A folder watch that gathers the platform's notifications into batches and reports each changed path once, as it now is on disk — written or removed. The app's own writes are left out by their content, however many notifications a write takes; `tauri-kit-fs` temporary files and paths the app names are ignored; and when the platform drops notifications the app is told to read the folder again. |

More capabilities will be added as separate crates.

## Usage

```toml
[dependencies]
tauri-kit-fs = { git = "https://github.com/iyulab/tauri-kit", tag = "v0.2.0" }
tauri-kit-credentials = { git = "https://github.com/iyulab/tauri-kit", tag = "v0.2.0" }
tauri-kit-sidecar = { git = "https://github.com/iyulab/tauri-kit", tag = "v0.2.0" }
```

Take the crates you need in one edit. Cargo re-resolves everything reachable from a git source's
already-locked crates when another crate from the same source is added later, which can move
unrelated entries of `Cargo.lock` — crates that accept a range of versions of a shared dependency
(such as `windows-sys` or `getrandom`) may land on a different one. It is harmless to the build, but
review the `Cargo.lock` diff when adding a crate from this repository to an app that already uses one.

### Versioning

The crates share one version and are released together. Each release is a `vX.Y.Z` tag on `main`, and
the tag always equals the `version` in `Cargo.toml`, and [CHANGELOG.md](CHANGELOG.md) lists what each release changed. While the version is `0.x`, a minor bump (`0.1` → `0.2`)
may break the API; a patch bump does not. Pin a tag, not a branch.

```rust
use std::path::Path;

tauri_kit_fs::write_atomic(Path::new("settings.json"), br#"{"theme":"dark"}"#)?;
// Fails with ErrorKind::AlreadyExists instead of replacing a file that is already there.
tauri_kit_fs::write_atomic_new(Path::new("report.md"), b"# Report")?;
```

```rust
use tauri_kit_credentials::{build_kind, Credentials};

let credentials = Credentials::new("com.example.app", build_kind!());
credentials.set("api-key", "s3cret")?;
```

## License

[MIT](LICENSE)
