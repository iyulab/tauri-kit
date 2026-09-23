# tauri-kit

Building blocks for desktop apps made with [Tauri](https://tauri.app) — the capabilities most such
apps end up writing for themselves, each in its own small crate so an app takes only what it needs.

## Crates

| Crate | What it gives an app |
|---|---|
| [`tauri-kit-fs`](crates/fs) | **Crash-safe file writes.** Write, flush to the device, then rename into place, so a crash or power loss leaves the old content or the new — never a truncated file. Optional staging directory for apps whose folders are watched or synced, and a startup sweep that removes only its own leftovers. |

More capabilities will be added as separate crates.

## Usage

```toml
[dependencies]
tauri-kit-fs = { git = "https://github.com/iyulab/tauri-kit", tag = "..." }
```

```rust
use std::path::Path;

tauri_kit_fs::write_atomic(Path::new("settings.json"), br#"{"theme":"dark"}"#)?;
```

## License

[MIT](LICENSE)
