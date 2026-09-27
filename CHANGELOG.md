# Changelog

All notable changes to the crates in this repository are documented here. The crates share one
version. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (while the version is
`0.x`, a minor release may change the API).

## [0.2.0] - 2026-09-27

### Added

- `tauri-kit-fs`: `write_atomic_new` and `write_atomic_new_staged` create a file crash-safely
  but never replace anything: if the target exists when the rename happens — a file, a directory,
  or a symbolic link, even a dangling one — they fail with `ErrorKind::AlreadyExists`, leave it
  untouched and remove their temporary file.
- `tauri-kit-fs`: `patiently` and `patiently_for` retry an operation while it fails with
  `ERROR_ACCESS_DENIED` or `ERROR_SHARING_VIOLATION` on Windows — what a rename, create or delete
  gets while a virus scanner, indexer or sync client briefly holds the file open — for about one
  second by default. `is_transient` tells those errors apart. No retry on other platforms.
- `tauri-kit-fs`: `Writer` holds a write's options — staging directory, temporary-file prefix
  (`temp_prefix`) and patience — with `write` and `write_new`.
- `tauri-kit-fs`: `sweep_staging_prefixed` sweeps leftovers named with any of several prefixes,
  such as the app's own and those earlier versions of it used.
- `tauri-kit-fs`: the `test-hooks` cargo feature adds `Writer::observe_steps`, which calls a hook
  between creating, writing, flushing and renaming the temporary file; a hook error stops the
  write there and leaves the temporary file behind, as a crash would. Meant for
  `[dev-dependencies]`; without the feature nothing of it is compiled.

### Changed

- `tauri-kit-fs`: every write retries transient refusals of the temporary file's creation and of
  the final rename on Windows, instead of failing at once.
- `tauri-kit-sidecar`: `Output::Files` (and the stderr file of `Output::Lines`) creates missing
  parent directories of the log files. Before, a missing directory meant the output was discarded.

## [0.1.1] - 2026-09-27

### Fixed

- `tauri-kit-sidecar`: on macOS, stopping a sidecar whose process group holds only exited
  processes no longer fails with `EPERM`.
- The declared minimum Rust version is now the one the dependencies need (1.89), and CI checks it.

## [0.1.0] - 2026-09-24

### Added

- `tauri-kit-fs`: crash-safe writes (`write_atomic`, `write_atomic_staged`) and `sweep_staging`.
- `tauri-kit-credentials`: secrets in the OS credential store, with separate entries for test runs
  and development builds.
- `tauri-kit-sidecar`: bundled helper processes without a console window, stopped together with
  everything they start, with readiness waits that notice a crash.

[0.2.0]: https://github.com/iyulab/tauri-kit/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/iyulab/tauri-kit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/iyulab/tauri-kit/releases/tag/v0.1.0
