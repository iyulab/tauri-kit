# Changelog

All notable changes to the crates in this repository are documented here. The crates share one
version. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (while the version is
`0.x`, a minor release may change the API).

## [Unreleased]

### Documentation

- `tauri-kit-watch`: a file made and removed again within one debounce window is not reported at
  all — the notifications cancel out before the batch is delivered. The crate documentation says
  so, instead of implying every path touched is reported.

## [0.4.0] - 2026-09-29

### Added

- `tauri-kit-fs`: `conflict_copy_of` recognises the copies sync clients make when a file changed on
  two devices — Syncthing's `.sync-conflict-<date>-<time>-<device>`, and the parenthesized
  `(… conflicted copy …)` of Dropbox (including the Korean `충돌된 사본`), Nextcloud, ownCloud and
  Google Drive — and returns the name of the file each is a copy of. The marker goes in front of
  the last extension, so a copy of a file with a compound suffix no longer ends in that suffix;
  an app that filters a listing by it can ask for the original's name instead. OneDrive's
  `name-ComputerName` is not recognised: it cannot be told apart from a name a person chose.

## [0.3.0] - 2026-09-28

### Added

- `tauri-kit-watch`: a new crate that watches a folder for changes made by other programs.
  Notifications are debounced into batches and each path is reported once, as `Written` or
  `Removed` according to what is on disk when the batch is delivered — a rename is the old path
  removed and the new one written, and a removed folder is its files removed. Notifications only
  say where to look: the watcher keeps a listing of the tree and compares the folders a batch
  touches with it, so a file the platform did not announce — one made in a just-made folder on
  Linux, the old name of a rename on macOS — is still reported. `OwnWrites` records what the app
  writes, and a change whose content is what the app last wrote there is left out, however many
  notifications or batches the write takes. Temporary files of `tauri-kit-fs` and paths matched by
  the app's `ignore` rule are left out. When the platform reports lost notifications the app gets
  `Notice::Rescan`. `Watch::probe_liveness` checks that the watch is still running — on Windows a
  platform watch stops without a word when its buffer of changes overflows — by writing a
  short-lived probe file into a folder the app names; two unheard probes in a row restart the watch
  and send `Notice::Rescan`.

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

[0.4.0]: https://github.com/iyulab/tauri-kit/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/iyulab/tauri-kit/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/iyulab/tauri-kit/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/iyulab/tauri-kit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/iyulab/tauri-kit/releases/tag/v0.1.0
