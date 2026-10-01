# Changelog

All notable changes to the crates in this repository are documented here. The crates share one
version. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (while the version is
`0.x`, a minor release may change the API).

## [Unreleased]

### Added

- README: how the .NET package is released alongside the crates (the `Publish NuGet` workflow,
  run on the release tag).

## [0.9.0] - 2026-10-01

### Changed

- `TauriKit.Sidecar.Loopback`: a fault carries the app's own frames it passed through, innermost
  first (`FaultView.Frames`, in the 500 body as `fault.frames`), not only the innermost one — the
  same exception type often comes from more than one path. `At` stays the first of them.
  `FaultOptions.MaxFrames` (default 20) caps them; `Fault.Of` takes the cap too, and
  `Fault.OwnFrames` reads them from a rendered trace, a frame repeated right after itself once.
  `FaultView` gained a positional parameter.

### Fixed

- `tauri-kit-diagnostics`: `WebBundle` keeps frames of a bundle served as `tauri://localhost/…`, as
  Tauri serves it on macOS and Linux — they were all dropped. `WebBundle::schemes` names the custom
  schemes only the app serves (default `tauri`), on which any host is the app's own; `http` and
  `https` still go by host.
- `tauri-kit-diagnostics`: `RustSource` keeps frames in folders below the source root
  (`src/commands/open.rs` → `commands/open.rs:12:5`); only a file directly under it was kept. Each
  folder must be a plain ASCII name, never `.` or `..`.

## [0.8.0] - 2026-10-01

### Fixed

- Every crate ships the license text (`LICENSE`) in its package. The license sat at the workspace root
  only, so the published crates carried their license as a name in `Cargo.toml` without its text.

### Added

- `SCOPE.md`: what the crates own (runtime platform behaviour that ships inside an app), what they
  deliberately leave to callers (tools, caller constants and wording, domain meaning, interface
  components), and the questions that decide which side a proposed capability falls on.
- `tauri-kit-sidecar`, new `loopback` feature (off by default — it brings an HTTP client): the
  arrangement for a sidecar that serves HTTP on 127.0.0.1. `Loopback::start(cmd, &options)` makes
  a fresh `Token` (32 random bytes, never shown by `Debug`), hands it over in the environment
  variable the app names, waits for the readiness line that starts with the app's prefix and reads
  the port from it, and answers a `Client` for that port. Every request carries the token as a
  bearer credential and never goes through a proxy; a status outside 2xx comes back as a
  `Response` to read, not an error. A sidecar that exits before announcing, a line without a port,
  and no line in time are each their own `StartError`.
- `dotnet/`: the .NET host side, NuGet package `TauriKit.Sidecar.Loopback` under the crates'
  version. `LoopbackHost.ReadToken`, `CreateSlimBuilder` (127.0.0.1 only, port picked by the
  system, no logging providers), `UseBearerToken` (constant-time comparison, 401 otherwise),
  `UseFaults` (an expected exception gets the status the app gives it; anything else a 500 with the
  exception type and the innermost frame in the app's own namespaces — never the message), and
  `RunAnnouncingAsync` (writes the readiness line once listening). Compatible with ahead-of-time
  compilation. CI tests it on Linux.
- `tauri-kit-webview`, a new crate for the web view runtime under a desktop app's window.
  `runtime()` answers `Installed(version)`, `Missing` or `System`: on Windows it asks the WebView2
  loader, which needs the runtime's files on disk, so a registration left without its files counts
  as missing; on macOS and Linux the system web view is always there. `alert(title, body)` shows
  the app's own message before any window exists (a message box on Windows, standard error
  elsewhere) — the crate supplies no wording. `forget_form_entries(controller)` clears the form
  entries and saved passwords a WebView2 profile remembered, for an app that has turned autofill
  off and must not keep what an earlier version let it remember; it takes the
  `ICoreWebView2Controller` the app already holds (`with_webview` on a Tauri window), so the crate
  does not depend on `tauri`, and its `webview2-com` major follows the one Tauri uses.
  `remove_profile_snapshots(data_dir)` removes the copies of the profile WebView2 takes before
  updating itself, which would carry those entries along; nothing to remove is not an error.

## [0.7.0] - 2026-10-01

### Added

- `tauri-kit-fs`: `replace_if` (and `Writer::replace_if`) replaces a file crash-safely only while it
  still holds what `Expect` says — exactly the bytes the app last read, or with
  `Expect::HoldsOrMissing` those bytes or nothing — so a change another program made since (a sync
  client bringing in another device's version, say) is not overwritten unseen. The content is
  compared before the new content is written out and again right before the rename that lands it.
  A refusal leaves the file as it is and is recognised by `is_changed`.
- `tauri-kit-fs`: `Root` — a folder the app works inside by relative paths. `Root::resolve` refuses
  `..` and leading `.` components, absolute paths, drive and UNC prefixes, empty paths, and paths a
  symbolic link or junction leads outside; `Root::prepare` also creates the folders on the way and
  checks again. Refusals are recognised by `is_outside`. The crate now depends on `dunce`.
- `tauri-kit-fs`: `has_trash` says whether a location has a trash the person can restore a file
  from — on Windows, network shares and removable drives have none, and recycling there deletes for
  good.
- `tauri-kit-fs`: `append_line` appends one line and its newline in one write, flushed to the
  device, creating the file and its folders if needed; a line holding a line break is refused.
- `tauri-kit-diagnostics`, a new crate: error reports that carry no content. `Report::new` keeps
  only the layer's name, a kind that is a plain identifier (anything else becomes `Unrecognized`)
  and the frames a `Layer`'s `FrameRule` recognises as the app's own code — `WebBundle` (scripts
  served from the app's own origin), `RustSource` (the app's `src/`) or `DotNetMethod` (method names
  only) — plus the app's version, the platform and the time; messages are never taken. `Reporter`
  appends one launch's reports to a JSON Lines file, once per failure (layer, kind and first frame)
  and at most `MAX_REPORTS`, and `trim` drops the oldest whole reports once the file passes a size,
  moving the sent offset back with them. With the `appinsights` feature, `Sink` parses an
  Application Insights connection string and `Sink::send_pending` sends what the file gained since
  the last send in batches, stopping on 408, 429 and 5xx answers so those reports go out later.

### Changed

- `tauri-kit-fs`: `rename_new` also moves folders, with the same promise — it never replaces
  what is at the new name. On Linux it is one `renameat2` with `RENAME_NOREPLACE` and on macOS one
  `renamex_np` with `RENAME_EXCL`, so the check and the move are one step for files and folders
  alike. Where the kernel or file system lacks that call, files fall back to link-then-unlink as
  before, and folders to a check followed by a rename. The crate now depends on `libc` on Unix.

## [0.6.0] - 2026-10-01

### Added

- All four crates are published to crates.io.
- `tauri-kit-watch`: `Watch::rescan_on(rule)` names paths whose change means the whole folder may
  have changed — a record kept beside the files that says which set of them is there, such as a
  repository's `.git/HEAD` when another tool switches branches. A batch that touches one is
  delivered as `Notice::Rescan` instead of file by file. The rule is asked before `Watch::ignore`,
  so the path can sit in a folder that is otherwise ignored.

## [0.5.0] - 2026-09-30

### Added

- `tauri-kit-fs`: `rename_new(from, to)` moves a file to a new name only if nothing is there —
  `AlreadyExists` otherwise, with both names left as they were. It is a rename, so the file keeps
  its creation time and a sync client sees a move. On Windows it is one `MoveFileExW` that does not
  replace; elsewhere a link under the new name, then an unlink of the old. Transient refusals are
  retried like the writes.

### Documentation

- `tauri-kit-watch`: a file made and removed again within one debounce window is not reported at
  all — the notifications cancel out before the batch is delivered. The crate documentation says
  so, instead of implying every path touched is reported.
- `tauri-kit-watch`: on Windows the `notify` 8 release underneath does not report an overflow —
  the batch is dropped, or the watch stops. The documentation of `Notice::Rescan` and
  `Watch::probe_liveness` says so, says that a probe catches only a watch that stopped, and says
  that in a synced folder the probe file can reach other devices.

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

[0.9.0]: https://github.com/iyulab/tauri-kit/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/iyulab/tauri-kit/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/iyulab/tauri-kit/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/iyulab/tauri-kit/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/iyulab/tauri-kit/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/iyulab/tauri-kit/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/iyulab/tauri-kit/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/iyulab/tauri-kit/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/iyulab/tauri-kit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/iyulab/tauri-kit/releases/tag/v0.1.0
