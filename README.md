# tauri-kit

Building blocks for desktop apps made with [Tauri](https://tauri.app) — the capabilities most such
apps end up writing for themselves, each in its own small crate so an app takes only what it needs.

## Crates

| Crate | What it gives an app |
|---|---|
| [`tauri-kit-credentials`](crates/credentials) | **Secrets in the OS credential store** — Windows Credential Manager, macOS Keychain, the Secret Service on Linux. Test runs and debug builds get their own entries, decided at compile time where the app writes `build_kind!()`, so running the test suite or a development build never overwrites or clears the secrets of the installed app. |
| [`tauri-kit-sidecar`](crates/sidecar) | **Bundled helper processes that behave.** No console window on Windows; the helper and everything it starts are stopped together — on shutdown, on drop, and on Windows even when the app crashes (job object); a readiness wait that returns the moment the helper exits instead of waiting out its deadline; stdout/stderr drained to files so the helper never blocks on a full pipe. |
| [`tauri-kit-fs`](crates/fs) | **Crash-safe file writes.** Write, flush to the device, then rename into place, so a crash or power loss leaves the old content or the new — never a truncated file. Optional staging directory for apps whose folders are watched or synced, and a startup sweep that removes only its own leftovers. |

More capabilities will be added as separate crates.

## Usage

```toml
[dependencies]
tauri-kit-fs = { git = "https://github.com/iyulab/tauri-kit", tag = "..." }
tauri-kit-credentials = { git = "https://github.com/iyulab/tauri-kit", tag = "..." }
tauri-kit-sidecar = { git = "https://github.com/iyulab/tauri-kit", tag = "..." }
```

```rust
use std::path::Path;

tauri_kit_fs::write_atomic(Path::new("settings.json"), br#"{"theme":"dark"}"#)?;
```

```rust
use tauri_kit_credentials::{build_kind, Credentials};

let credentials = Credentials::new("com.example.app", build_kind!());
credentials.set("api-key", "s3cret")?;
```

## License

[MIT](LICENSE)
