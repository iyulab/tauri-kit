# Changelog

All notable changes to the crates in this repository are documented here. The crates share one
version. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (while the version is
`0.x`, a minor release may change the API).

## [Unreleased]

### Added

- `tauri-kit-webview`: keep a window that shows untrusted content on its own origins. `NavigationLock`
  names the origins (exact scheme, host and port — or one `file:` document) and the other schemes a
  window may go to; `decide` answers for one URL on every platform. On Windows, `lock_navigation`
  holds a web view to it: navigations and redirects are cancelled, frames too, the document request
  of a refused navigation gets an empty 403 before it leaves (cancelling alone can let the request
  through), new windows are never created and are reported with whether the person started them,
  external schemes are refused, and a window that ends up on a refused document anyway stops loading
  and says so. It returns what the runtime let it hold (`Enforced`). `web_address` checks a URL from a
  page before it is handed to the system browser. URLs are read strictly — user info, backslashes,
  non-ASCII hosts and dot segments are refused, not guessed at. The lock does not stop data leaving
  by other means (images, `fetch`, WebSocket); pair it with a Content-Security-Policy.
- `TauriKit.Sidecar.Loopback.AspNetCore`: `LoopbackPort.StartRememberedAsync` — start a loopback backend on
  the port it had last time, remembered in a file the app names, so an origin that includes the port
  (a web view loading pages from the backend) survives a restart. The remembered port is tried a few
  times (its likeliest holder is the backend's own previous run, still stopping); only when it stays
  taken does the backend move, and `RememberedStart.RunAnnouncingAsync` adds `previousPort=<port>` to the
  readiness line. `tauri-kit-sidecar`: `PREVIOUS_PORT_FIELD` and `ready_previous_port` read it.
- New crate `tauri-kit-locale`: `preferred_languages` — the UI languages the person prefers, most preferred
  first, as BCP 47 tags, read before any window exists (Windows `GetUserPreferredUILanguages`, macOS
  `CFLocaleCopyPreferredLanguages`, `LANGUAGE`/`LC_ALL`/`LC_MESSAGES`/`LANG` elsewhere); `pick` — the best of
  the languages an app ships (RFC 4647 lookup, then the same language in another region).
- New NuGet package `TauriKit.Credentials`: the .NET twin of `tauri-kit-credentials` on Windows — the same
  Credential Manager entries (`<account>.<service>`, user name = account, UTF-16 secret) and the same build
  separation (`.test`, `.dev`), checked by writing from one language and reading from the other.
  `BuildKinds.Of(assembly)` tells a Debug build of the app.
- `tauri-kit-diagnostics`: an unfinished session says what is known of how it ended — `Ended::{Unfinished,
  OsSessionEnd, Updated}` in a new `how` field, from `Session::os_ending()` (call it from the platform's
  notice that the person's session is ending) and `Session::updating()` (before handing over to an
  installer), which the session writes into its marker. A different version alone is not taken for an
  update: `version_changed` says it beside `unfinished`, so a crash followed by reinstalling stays a crash.
  Lines queued before this read as before (no `how`). Application Insights gets `how` and `versionChanged`
  as properties of `SessionUnfinished`.
- `tauri-kit-diagnostics`: events the app declares. `Registry::from_json` reads the app's registry (compile it in):
  each event's name and up to 8 fields of closed kinds — `choice` (one of the declared values), `set`
  (some of them), `count` (bucketed, each bound at least twice the one before, or exact up to a cap of at
  most 1000 — an exact large number can identify what it counted), `bool`, `duration` (milliseconds, at
  most a day). A registry that could carry text, or that names an event or field twice, is refused at
  load. `Registry::event` turns a name into an `EventName` at start, so a misspelling fails there.
  `Events::open(&Session, registry, file)` writes all-or-nothing lines under a random id of the events'
  own — not the session id, which session lines send beside the installation — caps each event
  (`max_per_event`, default 100), and drops what does not fit (an undeclared field, a value not of its
  kind, a field left out or given twice), writing `kit.events.dropped` once per reason, event and field
  without the undeclared name or value. `pending_events` and `Sink::send_pending_events` read lines
  back through `Registry::verify`; `Sink::event_item` makes the Application Insights event, with no
  location derived from the connection.
- New NuGet package `TauriKit.Diagnostics`: declared events from a .NET backend — the same registry JSON, refused by
  the same rules, written to the crate's line shape (`Events.Record`), so the shell verifies and sends them with
  `pending_events`. The crate's `tests/cases` hold the shared registry and the lines both languages write for one
  scene; a test on each side checks them. Error reports too: `Registry.Layer` reads the registry's `reports` (the
  details a layer's reports may carry, refused by the crate's rules), `Layer.Report` builds a report from a kind and a
  stack — or from an exception: its type's full name and the method names of its stack, never its message or file
  names — keeping only declared details of their kind, and `Reporter.Record` appends it in the crate's line shape,
  each failure once and capped (`ReportsCapped`), so the shell reads and sends it with `pending`. `Segment.Open(dir)`
  gives the backend its own segment of a stream folder, as the crate's `Segment` does: hand `Segment.File` to
  `Events` or `Reporter`; the lock beside it is held (the lock file opened with `FileShare.None`) until the segment
  is disposed or the process goes, so the shell's `tidy` never deletes lines the backend is still writing.

### Changed

- `tauri-kit-diagnostics` (**breaking**): a stream — reports, session events, events — is kept in a folder,
  one segment per process that writes it. `Segment::open(dir)` gives a process a file of its own (hand
  `Segment::file` to `Reporter::new`, `Events::open` or `SessionBuilder::start`) beside a lock it holds while
  the `Segment` lives, which the operating system lets go when the process goes. `segments(dir)` lists every
  segment for a sender — live ones too, since only whole lines are read — and `Sink::send_pending`,
  `send_pending_of` and `send_pending_events` now take the folder and send from each. `tidy(dir, max_bytes)`
  replaces `trim`: it deletes only segments whose lock it can take, once their whole lines are sent, then the
  oldest ended ones past the limit (`MAX_STREAM_BYTES`, was `MAX_FILE_BYTES`); a segment still written is never
  rewritten, so two instances, or a shell and its backend, no longer lose each other's lines. `adopt(dir, file,
  sent)` takes in the single file a stream was kept in, with what it had sent, as one ended segment. `pending`,
  `unsent` and `mark_sent` read and record one segment, as before.
- `tauri-kit-diagnostics` (**breaking**): `SessionBuilder::start(file, running)` takes a folder of running
  sessions instead of one marker file. Each session keeps its own marker there beside a lock it holds while it
  runs, and a starting session records as unfinished only those whose lock nobody holds — their process is
  gone. Two instances of the app running at once no longer take each other for unfinished, and the first to end
  no longer removes the other's marker (which hid the other's crash). A marker file at that path, as earlier
  versions kept, is read once as a session that may not have ended and replaced by the folder — pass the same
  path.
- `tauri-kit-diagnostics` (**breaking**, a privacy fix): a report's details are only those the app's registry
  declares for the layer, each of its kind. `Report::detail` is gone — it took any plain identifier or a
  number of up to 18 digits under any name, which could carry an identifier from the data. A layer now comes
  from `Registry::layer(name, rule)` (the registry's new `reports` section: `{ "<layer>": { "<detail>": {
  "kind": … } } }`) and makes reports with `Layer::report(kind, stack, version, &[(name, value)])`: a value
  (untrusted text) is kept when it is of the declared kind — a count lands on its bucket — and left out whole
  otherwise. A layer made with `Layer::new` carries no details. The report line keeps its shape (details are
  still text), so queued reports read as before.

## [0.21.0] - 2026-10-17

### Added

- `tauri-kit-update`: `Status::last_answered` — when the source last answered a look, kept across
  launches. A look the source did not answer leaves it, so an app can tell «up to date» (answered,
  nothing newer) from «not known yet» (never answered); before, both read the same.

### Changed

- `tauri-kit-update`: a download that failed is tried again for the same version only once `every`
  has passed since the failure, or when a newer release is found — no longer at every look. An app
  that also looks at launch (`check_now`) no longer downloads a broken release at every launch, which
  the crate already promised. `Status::failed` keeps naming that version until then.

## [0.20.0] - 2026-10-17

### Added
- `tauri-kit-update`: `Status::failed` — the version whose download failed since the last look, kept across launches
  with a state file. It is not downloaded again until the next look, so this is when an app offers the person another
  way to get it (a download link). `Status` gains a field: code that builds a `Status` value itself needs it.

## [0.19.0] - 2026-10-17

### Changed
- `TauriKit.Sidecar.Loopback.AspNetCore` (breaking): `UseFaults` answers as an RFC 9457 problem, served as
  `application/problem+json`. An unexpected failure is `{"type":"about:blank","title":"Internal Server Error","status":500,
  "fault":{…}}` — the fault is unchanged, now the problem's `fault` extension member — and an exception `StatusFor` maps is
  `{"type":"about:blank","title":<the status's phrase>,"status":<it>}` instead of an empty body. `FaultResponse` is now
  `FaultProblem` (`Type`, `Title`, `Status`, `Fault`; `FaultProblem.Of(fault)`), and `StatusProblem` is the status-only
  shape. To move: a caller that deserializes `FaultResponse` takes `FaultProblem` — its `Fault` is where it was. A Rust
  caller using `Response::fault` / `Fault::from_answer` needs no change: they read the `fault` member as before.
- `SCOPE.md`: the fourth admission question no longer counts callers. It asks whether the shape is general — a primitive
  for every app that hits the same cause, read from the root cause rather than from the shape one caller wrote down — and
  an unclear request is answered by asking what the caller was trying to do, not by waiting for a second caller.

### Added
- `tauri-kit-sidecar` (`loopback`): `Problem::from_answer(status, body)` and `Response::problem()` read any answer outside
  2xx as an RFC 9457 problem — the status, `type` (as `kind`), `title`, `detail`, a `reason` extension member an app sets
  for its shell to act on, and the `fault` member. A body that is not a problem gives one with only the status.
- `tauri-kit-update`, a new crate: `Updates::start(source, UpdateOptions, on_event)` looks for a newer release when one is
  due — `every` (twelve hours) after the last look, kept in `state_file` across launches, or at once when the clock went
  backwards — and downloads its installer in the background. `Event::Found { first_time }` announces a version once,
  across launches; `Ready` when its installer is downloaded and checked; `Failed { stage }` for a look or a download, and a
  download that failed waits for the next look. `install_on_close(before_install)` installs a ready installer without
  starting the app again; `install_now(before_install)` downloads first if it must and asks for a restart.
  `set_enabled(false)` stops looking and lets go of what was found; `check_now` looks by hand either way. Releases come
  from a `Source` (`check`, `download`, `install`, and optionally `restore` and `forget` for installers kept on disk);
  the crate does not depend on Tauri, and the docs show the updater plugin as a source. `is_newer` compares plain
  `major.minor.patch` versions; a pre-release is never offered. `nsis/wait-for-exit.nsh`: an installer hook that, in
  update mode, waits up to 20 s for the app and a named helper to let go of their files before the template's
  running-app check.
- `tauri-kit-sidecar`: `supervise::Supervisor` keeps a sidecar running from a function that starts it once. It checks the
  process every `check_every`, and with a `Health` probe stops one that fails it `misses` times in a row; it starts an
  ended one again with the same function while a `RestartPolicy` allows (`limit` starts within `window`, failed ones
  included; `RestartPolicy::NEVER` leaves it to the app), then leaves it down until `restart`. What a start produced (an
  `Endpoint`, say — `Loopback` converts into a `Started<Endpoint>`) is swapped in together with the process and read
  with `current`. `Event::Restarted { before, after }`, `RestartFailed` and `Down` report each step. `stop(grace, ask)`
  asks with the running value, then stops the process for good; a start under way when it is called is undone when it
  finishes, not left running. The watcher locks the process only to check on it, never across a probe, a wait or a
  start.
- `TauriKit.Sidecar.Loopback.AspNetCore`: `MapShutdown(pattern = "/shutdown")` — a `POST` route that answers 202 and stops
  the host, behind the caller's credential like every route. `AddParentProcessWatch(pid)` stops the host when the app
  that started it exits, or at once when it is gone; a process with that id that started after the host is not its
  parent. For health, ASP.NET Core's `MapHealthChecks` is the route to use (the README shows it).
- `tauri-kit-sidecar`: `run(cmd, deadline)` runs a helper that does one job and exits — a converter, a renderer — and
  hands back `Ran::Exited(std::process::Output)` or `Ran::TimedOut`. Same care as `Sidecar`: no console window, stdin
  closed, both streams read while it is waited for. When the deadline passes the whole process tree is stopped, not only
  the child, and the call returns at once instead of waiting on output pipes a grandchild still holds open.

## [0.18.0] - 2026-10-16

### Changed
- `tauri-kit-state` (breaking): a format step is handed what the app knows about the state directory — the folder it is
  kept for, say, which the directory's name (a key) does not give back. `Step<C = ()>` is now `fn(&Path, &C)`,
  `Versioned<'s, C = ()>` keeps steps of that kind, and `bring_up_to_date(dir, context, write)` hands `context` to every
  step it runs. A step that needs nothing takes `()`: write steps as `|dir, _| …` instead of `|dir| …`, and call
  `bring_up_to_date(dir, &(), write)`. `read`, `writable` and `current` are unchanged.

### Fixed
- `TauriKit.Sidecar.Loopback.AspNetCore`: a request refused for want of a credential is answered with
  `WWW-Authenticate: Bearer` (RFC 6750), and `Bearer error="invalid_token"` when the credential it sent is not one the
  backend accepts — before, a 401 carried no hint of what was asked.

## [0.17.0] - 2026-10-15

### Changed
- `tauri-kit-sidecar` (breaking): where a sidecar's stderr goes when the kit reads its stdout line by line is now a
  `Sink` — `Discard`, `File(path)` or `Inherit` — instead of an optional path: `Output::Lines { stderr: Sink }` and
  `LoopbackOptions::stderr: Sink` (default `Discard`, as before). `Sink` converts from `PathBuf` and from
  `Option<PathBuf>` (`None` is `Discard`), so moving is `options.stderr = path.into()`.

### Added
- `Sink::Inherit`: the sidecar's stderr goes where the app's own goes — the terminal the app was started from while
  developing, so a helper's log is in front of the developer instead of in a file. An app built without a console (a
  released GUI app) has nowhere to write it; there it is as good as discarded, so keep `Sink::File` for release builds.

### Fixed
- `TauriKit.Sidecar.Loopback`: `DiscoveryFile.Publish` no longer fails when a caller has the discovery file open at the
  moment it is replaced. On Windows a file cannot be replaced while any handle to it is open, whatever share mode the
  reader asked for — and the backend's callers are exactly the processes that read the file, often polling it (and a
  stale one left by an earlier run) while the backend starts. The publish threw `UnauthorizedAccessException`, so a
  backend that logged and carried on was running where no caller could find it. The replace is now retried for up to
  three seconds; a reader that keeps the file open longer still fails the publish, and no temporary file is left behind.

### Security
- The repository's lock file moves `rustls` from 0.23.45 to 0.23.46, which fixes RUSTSEC-2026-0346 (session
  `Debug` output exposes TLS secrets), RUSTSEC-2026-0347 (buffered data sent after an ECH rejection),
  RUSTSEC-2026-0348 (up to 1 GiB allocated by certificate decompression before authentication) and
  RUSTSEC-2026-0349 (an unauthenticated ServerHello downgrades later connections). Only the
  `tauri-kit-diagnostics` feature `appinsights-rustls` reaches `rustls` (through `ureq`). A library's lock
  file does not bind its consumers: an app that enables that feature runs `cargo update -p rustls` in its
  own workspace.

## [0.16.1] - 2026-10-14

### Fixed
- `TauriKit.Sidecar.Loopback.AspNetCore`: `UseLoopbackHostCheck` refused every request an in-memory test
  server (`WebApplicationFactory`, `TestServer`) sends: its client names `localhost` with no port, and its
  connection's local port is 0, so the port never matched. A request with no socket under it (local port 0)
  is now judged by its name alone; a request over a socket still has to name the port it came in on.
- `tauri-kit-watch`: a file the app wrote (recorded with `OwnWrites::record`) and another program removed
  within one debounce window was never reported removed — the watch had not yet listed it, so its removal
  looked like a file that never was. The app shows what it writes, so it went on showing a file that was gone.
  It is now reported as `Removed`. A file another program made and removed within one window is still not
  reported.

## [0.16.0] - 2026-10-11

### Added

- `tauri-kit-sidecar` (`loopback-client`): `Client::get_stream(path)` reads a GET answer as it arrives —
  an event feed, the shape a page's `EventSource` opens — with the same token, `within` timeout and body
  limit as `post_json_stream`.
- `tauri-kit-sidecar` (`loopback-client`): `Stream::closer()` returns a `StreamCloser` (`Send + Sync`,
  `close()` may be called any number of times) that ends a stream from another thread. The backend sees
  the disconnect at once, and a read blocked on a quiet stream returns as the end of the body within
  about 50 ms instead of waiting for the next line. Dropping a `Stream` also shuts its connection.

## [0.15.0] - 2026-10-10

### Added

- `tauri-kit-webview`: `forwarded_browser_arguments(base)` and `ADDITIONAL_BROWSER_ARGUMENTS_VAR`. A test
  harness opens WebView2's debugging port through `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, but current
  runtimes do not apply that variable to an app that passes browser arguments of its own — which a Tauri
  app always does — so the port never opened. In a debug build this answers the arguments to give the
  window itself: `base` (the web view library's defaults, which giving any arguments replaces) followed by
  the variable's. In a release build, and without the variable, it answers `None`.

## [0.14.0] - 2026-10-09

### Changed

- **Breaking (.NET):** the loopback package is split in two. `TauriKit.Sidecar.Loopback` no longer
  references ASP.NET Core: it holds the discovery file (`DiscoveryFile`, `LoopbackDiscovery`,
  `DiscoveryProblem`), `LoopbackCredentials` and `LoopbackCaller`, and the fault record (`Fault`,
  `FaultView`, `FaultResponse`), so a caller that only finds and calls a backend (a supervisor, a
  command-line tool, a test) takes no web framework. The host half moves to the new
  `TauriKit.Sidecar.Loopback.AspNetCore` package: `LoopbackHost` (`CreateSlimBuilder`,
  `UseLoopbackHostCheck`, `UseFaults`, `ReadyLine`, `RunAnnouncingAsync`, `RunPublishingAsync`),
  `FaultOptions`, and `AddLoopbackAuthentication` / `RequireCaller` / `LoopbackCaller()`. The
  namespace is unchanged. **Migration:** a backend adds a reference to
  `TauriKit.Sidecar.Loopback.AspNetCore`, and `LoopbackHost.ReadToken` becomes
  `LoopbackCredentials.ReadToken`.

### Added

- `TauriKit.Sidecar.Loopback`: `LoopbackDiscoveryHandler`, the caller's side of a backend found by
  its discovery file. Each request goes to the port the file names, on 127.0.0.1, with the
  credential as its bearer. When the backend answers 401 or cannot be reached and the file now
  names another run, the request is sent once more to that run. `LoopbackDiscoveryHandler.CreateClient(path)`
  returns an `HttpClient` for relative addresses, and a file that names no running backend throws
  `LoopbackUnavailableException` (an `HttpRequestException`) with its `DiscoveryProblem`.
- Loopback readiness lines can say more after the port, as `key=value` words (the port the backend
  had to give up, say). Rust: `ready_fields(line, prefix)` reads them, and `ready_port` now takes
  the first word after the prefix, so it accepts such a line. .NET: `ReadyLine` and
  `RunAnnouncingAsync` take `fields`, one word each side of `=`.

## [0.13.0] - 2026-10-09

### Added

- `tauri-kit-sidecar` (`loopback`): `Endpoint` is where a backend listens and the token it
  accepts, separate from any client. `base()` and `authorization()` serve an app that calls with
  its own HTTP client (async, say), and its `Debug` shows the port only. `Loopback::endpoint()`
  returns it for a started backend, and `Loopback::ready_line()` the whole readiness line, for a
  backend that announces more than its port.
- `tauri-kit-sidecar` (`loopback`): `discover(path)` reads the discovery file a backend that lives
  on its own publishes (`DiscoveryFile.Publish` on the .NET side) into an `Endpoint`, or
  `DiscoveryError::{Missing, Unreadable, Stale}`. A file is stale when its process is gone; on
  Windows the process's start time must match too.
- `tauri-kit-sidecar` (`loopback`): `Token::from_secret` for a secret made elsewhere. It must be at
  least 32 visible ASCII characters. `Fault::from_answer(status, body)` reads a host's failure
  answer for any client.

- `TauriKit.Sidecar.Loopback`: a backend that lives on its own. `DiscoveryFile.Publish` writes the
  port, the credential, the process id and its start time to a file only the person running the
  backend can read (Windows: an access list of that user and the system, inheriting nothing;
  elsewhere mode 0600), written beside its place and moved in. `DiscoveryFile.TryRead` tells a
  missing, a damaged and a stale file (its process gone) apart. `RunPublishingAsync` publishes once
  the app listens and removes the file on shutdown, unless a later run has replaced it.
- `TauriKit.Sidecar.Loopback`: callers told apart. `LoopbackCredentials` issues credentials (32
  random bytes) or registers one made elsewhere, each tied to a `LoopbackCaller` (a kind and,
  optionally, a subject), and revokes them. Credentials are held in memory and looked up by their
  SHA-256 digest. `AddLoopbackAuthentication` authenticates bearer requests against them and makes
  that the fallback policy: every route requires a registered caller unless it is marked
  `AllowAnonymous()`. `RequireCaller(kinds)` admits only the named kinds (403 otherwise), and
  `ClaimsPrincipal.LoopbackCaller()` reads the caller in a handler.
- `TauriKit.Sidecar.Loopback`: `UseLoopbackHostCheck` answers 400 to a request whose `Host` is not
  `127.0.0.1`, `localhost` or `[::1]` with the port it came in on. Optionally it also accepts names
  under `localhost`. This refuses a page whose own name was pointed at 127.0.0.1, even on an open
  route.

### Changed

- `tauri-kit-sidecar`: the blocking `Client` (with `Response` and `Stream`) moves behind a new
  feature, `loopback-client`. `loopback` alone no longer brings in `ureq`, so an app with its own
  HTTP client does not ship a second one. Migration: enable `loopback-client` where `client()` is
  used. `Client::new(&Endpoint, &ClientOptions)` replaces `Client::new(port, &Token,
  &LoopbackOptions)`. `request_timeout` and `body_limit` move from `LoopbackOptions` to
  `LoopbackOptions::client` (`ClientOptions`). `Loopback::into_parts()` returns
  `(Sidecar, Endpoint)`; make a `Client` from the endpoint where one is needed.

### Removed

- `TauriKit.Sidecar.Loopback`: `UseBearerToken(token)`. Register the token instead, then use the
  authentication above:
  `var c = new LoopbackCredentials(); c.Register(token, "app"); builder.Services.AddLoopbackAuthentication(c);`.
  Routes meant to stay open, such as a health check, need `AllowAnonymous()`.

## [0.12.1] - 2026-10-08

### Added

- `tauri-kit-diagnostics`: sessions. A `Session` appends to a JSON Lines file when the app starts,
  when it ends as it should (its length and the time on each screen), and, on the next start, for a
  session that did not end — told by a marker file the running session keeps. Every field comes
  from an allowlist: a random `InstallId` made on the device from the OS random generator (never
  from the hardware), a random session id, the version, the platform, the time, the interface's
  language tag, a plain-identifier name of the app's own for its edition, and time on the app's
  `Screens`, a fixed list of names — any other name is recorded as `Unrecognized`, whole. A line
  read back that breaks a rule is not a session event. `Sink::session_items` makes event items
  (`SessionStart`, `SessionEnd`, `SessionUnfinished`) and one page view per screen, tagged with
  the installation as `ai.user.id` and the session as `ai.session.id`.
- `tauri-kit-diagnostics`: one send path for every kind of line. `Telemetry` (implemented by
  `Report` and `SessionEvent`) says what items a line becomes; `pending::<T>` reads what a file of
  `T` gained since the last send, and `Sink::send_pending_of::<T>` sends it. `unsent`, `Batch` and
  `Sink::send_pending` are the same for reports, unchanged.

### Changed

- `tauri-kit-sidecar`: the `loopback` feature draws its secret with `getrandom` 0.4 (was 0.3). On
  Windows, 0.4.2 checks the result of the system random generator it calls in every build; 0.3
  checked it only in debug builds.

## [0.12.0] - 2026-10-03

### Added

- `tauri-kit-fs`: `NameKind::Suffix(ending)` numbers a name before an ending of more than one
  extension — `notes.fd.md` with `.fd.md` → `notes (1).fd.md` — for files whose kind an app tells by
  that whole ending. `NameKind::File` numbers before the last extension only, which gave
  `notes.fd (1).md`, a name the app no longer reads as its kind. `NameKind` now has a lifetime;
  a `match` over it needs the new arm.

### Documentation

- `tauri-kit-sidecar`: stop the sidecar in the updater's `on_before_exit` hook — on Windows the
  installer replaces the sidecar's program while the app is still running, before the job object
  ends the sidecar with it.

## [0.11.0] - 2026-10-02

### Added

- `tauri-kit-sidecar` (`loopback`): `Client::within(timeout)` — the same client, giving up on each
  request after `timeout` instead of `LoopbackOptions::request_timeout` (a quick health check, a long
  streamed answer). `Client::post(path)` posts with no body. `Client::post_json_stream(path, json)`
  reads the answer as it arrives: a `Stream` with the status, read line by line through `BufRead`
  (server-sent events). `LoopbackOptions::stdout` keeps the sidecar's stdout in a file, the
  readiness line included. `Loopback::into_parts()` hands over the process and the client apart, for an app
  that keeps them in different places.
- `tauri-kit-diagnostics`: sending with an HTTP client of the app's own. `Sink` — `parse`,
  `envelope` and the new `Sink::retry_later(status)` — is there without either sending feature, and
  `unsent(file, sent)` reads what a report file gained since the last send, in the `Batch`es one
  request takes, each with the `end` offset to record with `mark_sent(sent, end)` once it is
  handled. `Sink::send_pending` is now these three over `ureq`, and behaves as before. An app that
  already has an HTTP stack, or sends from its web view, no longer needs a second one to report.
- `tauri-kit-watch`: `Watch::ignore_files(rule)` leaves out files the rule matches and is never asked about a
  folder, so every folder stays watched. An app that hears only its own kind of file says so with it: a rule like
  "not a `.md` file" given to `Watch::ignore` matches every folder too, and the files of a folder moved in or removed
  whole then went unreported. `Watch::ignore` keeps its meaning — a folder it matches is not looked into — and its
  documentation now says so.
- `tauri-kit-diagnostics`: an `appinsights-rustls` feature — the same `Sink`, with rustls doing the
  TLS handshake against the OS's certificate store (rustls-platform-verifier) and the system proxy
  as before. `appinsights` hands TLS to the OS's own stack, which is OpenSSL on Linux, so its lock
  file lists `openssl-sys` whatever the target; an app that keeps OpenSSL out of its dependency graph
  can now send reports. With both features on, `appinsights` decides.
- `tauri-kit-fs`: `free_path(dir, name, kind)` finds the first free name in a folder — `notes.md`,
  then `notes (1).md`, `notes (2).md`, … — numbering a file before its last extension and a folder
  at the end (`NameKind`). `claim_free_path(dir, name, kind, create)` creates something under it
  and moves on to the next free name when `create` reports the name was taken in the meantime
  (`AlreadyExists`), up to `CLAIM_TRIES` times; what to create stays the caller's choice
  (`File::create_new`, `create_dir`, `write_atomic_new`, `rename_new`). `is_taken(path)` is the
  test both use: a symbolic link that points nowhere counts as taken, since creating or moving
  onto it is refused.
- `tauri-kit-state`, a new crate for the state an app keeps for each folder a person opens,
  outside that folder. `folder_key(folder)` names a directory for it: the folder's own name and a
  digest of its resolved path, ignoring case on Windows and macOS; the key does not change between
  releases. `Versioned::new(&steps)` keeps a format number (`format`) in a state directory:
  `bring_up_to_date` runs the steps from the format found and writes the number last, so a step cut
  short runs again; a format newer than the steps, or a format file that is not a number, is
  `Format::Unknown` and is neither upgraded nor `writable`. `set_aside(dir, rel, now_secs)` moves a
  file that cannot be read to `<rel>.unreadable-<secs>` without replacing anything. `Outbox` keeps
  writes the app could not make yet as files (`<when>-<n>.<extension>`, oldest first) until it
  forgets them; a file that cannot be read is left in place.

### Changed

- `tauri-kit-sidecar`: `Output::Lines` has a `stdout` file as well as a `stderr` one — every stdout
  line is appended to it, the ones `Sidecar::wait_line` takes included; before, stdout after the
  accepted line was drained and dropped. Breaking for code that builds the variant: write
  `Output::Lines { stdout: None, stderr }` to keep the old behavior.
- `tauri-kit-watch` builds on `notify` directly and gathers notifications itself, each delivered
  once every path it names has been quiet for the debounce window, as before. `notify-debouncer-full`
  is no longer a dependency.

### Fixed

- `tauri-kit-watch`: on macOS, a folder that was there before the watch and was removed whole went
  unreported. The platform hands a removed path's earlier history over with the removal — each file
  "created, removed, modified" — and the debouncer cancelled the creation against the removal,
  leaving no notification at all. Notifications are now only gathered, and the disk decides what
  changed; a file made and removed again within one window is still not reported.

## [0.10.0] - 2026-10-01

### Added

- `tauri-kit-diagnostics`: `Report::detail(name, value)` adds one more fact about a failure — an
  HTTP status, the app's own error code beside the type it came with — under a name the app
  gives (an ASCII letter, then letters, digits or `_`, at most 32 characters). The value is
  untrusted like a kind: a plain identifier or a whole number, else the detail is left out
  whole. At most `MAX_DETAILS` (8) per report; a name given again replaces its value. Details
  are written in the report's JSON line only when there are some (older lines still read) and
  go out as the exception item's `properties`.
- `tauri-kit-sidecar` (`loopback`): `Response::fault` reads the host side's failure answer
  (`{"fault":{…}}` from `UseFaults` in `TauriKit.Sidecar.Loopback`) as a `Fault` — its type, the
  innermost frame of the app's own code and the frames it passed through. `Fault` also reads a list
  of them, for a host that hands over failures of work no request waited on. The feature now brings
  `serde` and `serde_json` along.

### Fixed

- `TauriKit.Sidecar.Loopback`: the package now carries the `LICENSE` text, not only the MIT
  expression — MIT asks that the copyright and permission notice travel with each copy.

## [0.9.1] - 2026-10-01

### Fixed

- `tauri-kit-watch`: a file another program made and removed again quickly could be reported as
  written and never as removed, when a change beside it in the same folder made the watch list
  that folder before the file's own notification came. The platform watcher's creation and
  removal of such a file cancel out, so no notification ever corrected it, and the app kept a
  file that was gone. A file new to a listed folder is now reported only when a notification
  names it. Also, a folder listed for the first time no longer reports every file in it as
  written — its files were already there.

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

[0.21.0]: https://github.com/iyulab/tauri-kit/compare/v0.20.0...v0.21.0
[0.20.0]: https://github.com/iyulab/tauri-kit/compare/v0.19.0...v0.20.0
[0.19.0]: https://github.com/iyulab/tauri-kit/compare/v0.18.0...v0.19.0
[0.18.0]: https://github.com/iyulab/tauri-kit/compare/v0.17.0...v0.18.0
[0.17.0]: https://github.com/iyulab/tauri-kit/compare/v0.16.1...v0.17.0
[0.16.1]: https://github.com/iyulab/tauri-kit/compare/v0.16.0...v0.16.1
[0.16.0]: https://github.com/iyulab/tauri-kit/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/iyulab/tauri-kit/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/iyulab/tauri-kit/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/iyulab/tauri-kit/compare/v0.12.1...v0.13.0
[0.12.1]: https://github.com/iyulab/tauri-kit/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/iyulab/tauri-kit/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/iyulab/tauri-kit/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/iyulab/tauri-kit/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/iyulab/tauri-kit/compare/v0.9.0...v0.9.1
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
