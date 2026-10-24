# Changelog

All notable changes to the crates in this repository are documented here. The crates share one
version. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (while the version is
`0.x`, a minor release may change the API).

## [Unreleased]

### Added

- `tauri-kit-webview`: `pdf::print_to_pdf(controller, source, &PdfSettings, out, on_done)` (Windows) — prints the window's
  document (`Source::Current`), or HTML the app hands over (`Source::Html`, put in with `NavigateToString`: a data URL
  ends the document at its first `#`), to a PDF file with no dialog, through WebView2's `PrintToPdf`. It waits for the
  page itself to be ready — loaded, fonts in — by asking the page (DevTools `Runtime.evaluate` with `awaitPromise`), up
  to `PdfSettings::ready_timeout`, so an app picks no settle delay; a document put in as a string is not seen by a
  framework's page-load event. Called on the web view's thread, it starts the work and returns; `on_done` gets the
  outcome once. `PdfSettings` (paper in inches held upright, `Orientation`, `Margins`, backgrounds; `a4()` · `letter()`)
  is checked first — margins that meet, a non-positive paper, a zero timeout are refused, as is HTML past WebView2's
  2 MB. The window stays the app's: a hidden printing window has to be made with the app's own browser arguments.
  `examples/print_to_pdf.rs` prints a three-page report through a hidden window.
- `tauri-kit-webview`: `taskbar::set_taskbar_identity(window, &TaskbarIdentity)` (Windows) — gives a window its
  AppUserModelID and, with `Relaunch`, the command, display name and icon a taskbar pin of it starts
  (`System.AppUserModel.ID`, `RelaunchCommand`, `RelaunchDisplayNameResource`, `RelaunchIconResource` on the window's
  property store). Without a relaunch command Windows pins the process that drew the window — for an app that runs from
  a versioned folder an updater replaces, or that starts through a launcher of its own, the pin then starts the wrong
  file. An empty ID, a relaunch without a command or display name, and a handle that is not a window are refused with
  `InvalidInput`. Elsewhere it does nothing.
- `tauri-kit-update`: `verify::developer_id(path, &DeveloperId)` (feature `developer-id`) — the macOS counterpart of
  `verify::authenticode`. A downloaded bundle passes when it is validly signed (every architecture, nested code, strict)
  with a Developer ID Application certificate of one of the named Apple team IDs, and notarized — the stapled ticket
  is read, so a stapled release passes with no network. A refusal says who signed (`Refused::OtherDeveloper` with the
  identifier and team) or carries the Security framework's status (`Refused::Untrusted`). Elsewhere it refuses every
  bundle with `Refused::Unsupported`. No new dependency: the Security framework is called directly.
- `tauri-kit-sidecar`, `tauri-kit-fs`: case tables that pin down what the supervision and file-retry words mean, for
  any implementation to run. `crates/sidecar/tests/cases/supervision.json` lists start attempts against the start or
  stay-down `RestartPolicy` gives each (at most `limit` starts within any `window`, failed starts included; no growing
  pause, no reset after a stable run). `crates/fs/tests/cases/file-retry.json` states `patiently_for`'s window
  without its pauses: a refusal that ends within the patience is outlasted, the last attempt is made when the window
  closes, and nothing waits past it. The rustdoc of both says so, and that a host's own worker supervision is a
  different policy at another layer.

### Changed

- `tauri-kit-update`: the `verify` module is built with either `authenticode` or `developer-id`; `Refused` has a new
  variant (`OtherDeveloper`), and `Refused::Unsupported` no longer names Windows in its message.

## [0.28.0] - 2026-10-23

### Added

- `tauri-kit-diagnostics`: a session that goes without ending now says where it was. Each `Session::screen` change
  rewrites the running marker with the screen open and the time on screens so far, and the `unfinished` line the next
  session records carries them as `last_screen` (one of the app's `Screens` or `unrecognized`) and `screens` — up to the
  last screen change. `Sink::session_items` sends `lastScreen` with `SessionUnfinished` and a page view for each screen,
  as for an end. A session that never named a screen writes the same lines as before.
- `tauri-kit-diagnostics`: `SessionBuilder::os_version()` adds the operating system's version to the session's lines as
  `os_version` — numbers only (`10.0.26100` from `RtlGetVersion` on Windows, `26.1` from `kern.osproductversion` on
  macOS, the leading numbers of the kernel release on Linux) — and `Sink` puts it in `ai.device.osVersion` between the
  system and the architecture. Off unless the app asks: it is one more fact about the device.

### Changed

- `tauri-kit-diagnostics`: a running marker written by this version is not read by an earlier one (it has fields the
  earlier version does not know), so going back to an earlier version after a crash loses that one unfinished line.

### Fixed

- `tauri-kit-webview`: `lock_navigation` (Windows) answered every request it saw outside the allowed origins with a bare
  403, not only documents. WebView2 raises `WebResourceRequested` for any request matching any filter on the web view, and
  a framework that serves its own custom protocols registers filters for all resource kinds — so the lock also refused the
  app's scripts, fetches and CORS preflights to such a scheme. A Tauri app's IPC is one: every `invoke` preflight to
  `http://ipc.localhost` failed its CORS check (a 403 with no `Access-Control-Allow-Origin`), and Tauri fell back to its
  postMessage interface for the rest of the session, after about two dozen failed calls at every start. The lock now judges
  document requests only (the window's and its frames'); navigation is refused exactly as before.

## [0.27.0] - 2026-10-23

### Added

- `tauri-kit-webview`: `context_menu::add_page_item(webview, label, on_pick)` puts an item of the app's own at the top
  of the menu the web view shows for the page itself — right-clicked where there is no selection, link, picture, video or
  field to type in — for what acts on the whole page. Windows: `ContextMenuRequested` with a page target that has no
  selection, link or editable field; macOS: the menu with the page's Reload entry and none that a selection, a link, a
  picture or a field adds. A web view may have both a selection item and a page item.
- `tauri-kit-update`: `github::Releases` — releases published on GitHub as the place to look. `latest(fetch, channel)`
  reads the newest version: on the stable channel from the `…/releases/latest` redirect (one request, no API call and so
  no API rate limit, pre-releases and drafts skipped), on the beta channel from the API's release list (the highest
  version in semver order, drafts skipped). `asset_url(version)` names the installer from a template
  (`Example_{version}_x64-setup.exe`) under tags `v<version>` (`tag_prefix`), and `download(fetch, version, dest)` saves it
  beside `dest` and renames it into place only when it arrived whole. The requests go through `github::Fetch`, so an app
  keeps the HTTP client its users' networks need (a proxy from a configuration script, say); the `github` feature adds
  `github::Ureq`, over ureq with the OS's TLS and the PC's proxy settings. Checking what arrived and running it stay
  with the app's `Source`.
- `tauri-kit-update` (`authenticode` feature): `verify::authenticode(path, &Expect)` checks that a downloaded installer
  carries a valid Authenticode signature (a chain Windows trusts, revocation not asked so an offline network does not block
  updates) and that its signer is the expected one — by subject, issuer and thumbprint (SHA-1 or SHA-256), each a list of
  accepted values, rather than by display name alone, which any similarly named certificate passes. An expectation that
  names nothing is refused. It answers the signer it found (`Signer`) or why the file was refused (`Refused`). Windows;
  elsewhere every file is refused with `Refused::Unsupported`.

## [0.26.0] - 2026-10-23

### Added

- `tauri-kit-update`: a release channel and the kind of a release. `Channel::Stable` (the default) offers plain releases
  only, `Channel::Beta` pre-releases too (`1.3.0-beta.2`); `UpdateOptions::channel` sets it and `Updates::set_channel`
  changes it, letting go of the release found and looking again. The channel is the app's to remember, like looking on
  or off. `Kind::Fix` (only the patch number rose) or `Kind::Feature` (the minor or major did) is counted from the
  versions and carried by `Event::Found` and `Status::kind`, so an app can install a fix quietly and tell the person
  about a feature; `kind_of` answers it for two versions.
- `tauri-kit-webview`: `context_menu::add_selection_item(webview, label, on_pick)` puts an item of the app's own at the
  top of the menu the web view shows for selected text, and calls the app with that text when the person picks it. It
  extends the web view's own menu — WebView2's `ContextMenuRequested` on Windows, the web view's `willOpenMenu:withEvent:`
  on macOS — so copy, paste and the platform's other entries stay; a page that wants an entry of its own otherwise has to
  replace the whole menu. The label is read each time the menu opens, so it follows the app's language. On macOS the
  framework's web view class takes no menu hook, so the method is added to that class at run time (the web view's own
  implementation runs first); a class that already has its own is refused with `Unsupported`. Windows and macOS.
- `tauri-kit-sidecar` (`relay` feature): a web page's way to a loopback backend through the app, without the page ever
  holding the backend's credential. `Relay::builder(source)` takes where the endpoint comes from (`move || discover(&path)`
  for a backend that publishes a discovery file), `.origin(..)` the page origins it answers, `.allow(method, template)` and
  `.allow_stream(method, template)` the routes (`{name}` is one segment — never empty, `.` or `..`, encoded or not; a
  `?key&key` tail names the query keys a route takes) and `.timeout_for(method, template, duration)` a route's own time
  limit. `relay.handle(http::Request<Vec<u8>>)` answers `http::Response<Body>`: the backend's status, `Content-Type` and
  body as bytes (`Body::Full`), or for a stream route a 2xx answer read as it arrives (`Body::Stream`, chunk by chunk, with
  a closer for another thread); a preflight from a named origin is answered, every answer to a named origin carries the
  CORS headers, and none is cached. A 401 reads the source again and repeats the call once; a call that reached nothing
  is not repeated, and the next one reads the source again. The relay's own refusals are RFC 9457 problems with a
  `reason`: `origin-refused` and `route-refused` (403, before any call), `backend-unreachable` (503), `backend-timeout`
  (504). It holds no web view type, so an app connects it to its URI scheme handler and carries streams the way its web
  view allows.
- `tauri-kit-webview`: `drop::receive_dropped_files(controller, prefix, on_files)` says where the files dropped onto a page
  are. A page gets dropped `File` objects but never their location, and a framework's own drop handler, which would report
  it, takes HTML5 drag and drop away from the page on Windows. The page posts the files back with
  `chrome.webview.postMessageWithAdditionalObjects(prefix + …, event.dataTransfer.files)`; the host receives each file's path
  (`ICoreWebView2File`), the message text and the sending document's URL as `DroppedFiles`. Only messages starting with the
  app's (non-empty) prefix are read — the framework's own messages on the same channel are left alone. Windows only.
- `tauri-kit-sidecar` (`loopback-client`): `TransportError::is_timeout` tells a backend that answered too late from one
  that was not reached; `Stream::headers` and `Stream::header` carry a streamed answer's headers, as `Response` does.
- `tauri-kit-fs`: `conflict_copy_of_on(file_name, devices)` also recognises OneDrive's conflict copy, `<original>-<device>`
  before the last extension, for the devices it is given (compared without regard to case) — the device being the computer
  whose change lost, so this computer's own name recognises the copies made here. By name alone such a copy cannot be told
  from a name a person chose, which is why `conflict_copy_of` still leaves it out.
- `tauri-kit-watch`: `Watch::sweep_every(every)` lists the watched tree again on an interval and reports a file that
  arrived or left without a notification — and a folder that arrived whole, its files — as `Written` or `Removed`, in
  the same `Notice::Changed` as an announced one. Some changes notify nothing: on Windows, a cloud sync client built on
  the Cloud Files API (OneDrive and others) brings a file made on another device down as a placeholder, and creating
  one raises no directory notification, so the file went unheard until something else touched its folder. A sweep
  compares names only; content changes are still heard through notifications. The interval must be longer than twice
  the debounce window.

### Changed

- `tauri-kit-update` (breaking): `Source::check` takes the channel to look on (`fn check(&self, channel: Channel)`) — a
  source with one manifest per channel picks it there; a source with one channel ignores it. `is_newer` takes the
  channel too. `Event::Found` has a `kind` field (match it with `..` if you do not need it).

### Fixed

- `tauri-kit-update`: versions are compared in semver order, pre-releases included. An app running a pre-release (a beta
  installed by hand) was offered nothing at all — its own version did not read as a version — and is now offered the
  release that follows it; a version with a leading zero (`01.2.0`) is not a version.

## [0.25.0] - 2026-10-22

### Added

- `tauri-kit-webview`: `caption::MaximizeButton` keeps Windows 11's Snap Layouts on a maximize button the web page draws, in
  a window without the system caption. The flyout appears only when the window under the cursor answers the non-client hit
  test as a maximize button, and in a WebView2 window that window belongs to the browser process; the block places an
  input-sink child window of the app's own window over the button's rectangle (client pixels, given and kept current by the
  caller): it answers `HTMAXBUTTON`, a click on it maximizes or restores the window, a right-click opens the window's system
  menu, the window's top resize band is passed on, and it reports `ButtonEvent`s (enter, leave, down, up) so the page can
  draw hover and pressed. Windows only; elsewhere it does nothing.
  (`tauri-kit-webview` 0.25.0 on crates.io was published from the commit after the `v0.25.0` tag and carries this
  block; every other crate and the NuGet packages match the tag.)

- `tauri-kit-sidecar` (`pipe` feature): a client of a supervisor's local pipe, for a shell whose backend is started and kept
  by another process rather than by the shell. One JSON object per line over a named pipe (Windows) or a Unix socket:
  `PipeClient::request(cmd, fields)` sends a line carrying a fresh `id` on a connection of its own and returns the line
  that echoes it (pushed lines before it are skipped; a deadline that passes is `TimedOut` on every platform), and
  `PipeClient::events(retry, on_signal)` reads the supervisor's pushed lines on one held connection, reporting
  `Connected`, each `Event` and `Disconnected`, and connecting again with a back-off until stopped. `PipeAddress::dotnet(name)`
  is where a .NET `NamedPipeServerStream` of that name listens on each platform. The pipe name, commands, fields and event
  shapes are the caller's. A supervisor must accept more than one connection.
- `tauri-kit-sidecar` (`loopback-client`): `Client::send_with_headers(method, path, json, headers)` sends a request with
  headers of the caller's — for a protocol over HTTP that carries its own both ways, such as a session id the backend hands
  out and expects back, or an `Accept` list — and `Response::headers` / `Response::header(name)` give the answer's. The
  `Authorization` header stays the client's: a header of that name, or a name or value HTTP does not allow, is a
  `TransportError` and nothing is sent.

### Changed

- `tauri-kit-sidecar` (`loopback-client`): `Response` has a `headers` field. Code that builds a `Response` itself (a test
  double) adds `headers: Vec::new()`.

### Fixed

- `tauri-kit-sidecar` (`loopback-client`): a streamed answer (`Client::get_stream`, `Client::post_json_stream`) is no
  longer cut off when the client's request timeout passes while it is still being read. The timeout covered the whole
  exchange, body included, so an event feed held open for longer than it (120 seconds by default) or a long answer — a
  model's reply, a download's progress — ended with `timeout: global` mid-way. The timeout now holds until the status and
  headers arrive; reading the body has no deadline and the stream is ended with `Stream::closer`. `Client::within` bounds
  the same wait for a stream.

### Security

- The workspace lockfile moves `rustls-webpki` to 0.103.16 and `webpki-roots` / `webpki-root-certs` to 1.0.10
  (RUSTSEC-2026-0381, -0382, -0383: distrusted intermediates could be accepted). Only the repository's own builds and tests
  read this lockfile; an application resolves these crates in its own lockfile.

## [0.24.0] - 2026-10-20

### Added

- `tauri-kit-sidecar` (`loopback-client`): `Client::send(method, path, json)` sends any HTTP method — `PUT`,
  `DELETE`, `PATCH` and the rest — with the token, an optional JSON body and the client's timeout, so a shell
  that relays its page's requests to the backend is no longer limited to GET and POST. A name that is not an
  HTTP method is a `TransportError` and nothing is sent.

### Fixed

- `TauriKit.Sidecar.Loopback.AspNetCore`: `AddParentProcessWatch` in a web host whose parent is already gone when it
  starts no longer fails the host. The watch stopped the application while the host was still starting its services,
  which cancelled the server's start and ended the process with an unhandled exception; it now stops it once the host
  has started.
- `tauri-kit-update`: `nsis/wait-for-exit.nsh` showed including it as `!include "wait-for-exit.nsh"`, which makensis
  cannot find from the installer script Tauri generates in its build folder; it now shows
  `!include "${__FILEDIR__}\wait-for-exit.nsh"`, the hooks file's own folder. The macro itself is unchanged.

## [0.23.0] - 2026-10-19

### Added

- `tauri-kit-state`: state for the device, beside the per-folder state. `Settings<T>` reads and writes an
  app's settings as one JSON object, crash-safely, and writes back the fields a newer release added instead
  of dropping them (a field `T` knows stays `T`'s); a file that cannot be read is moved aside, not lost.
  `InstallId` (moved here, with `InstallId::in_dir` and `INSTALL_ID_FILE`) is the one identifier of the
  installation. `relocate(old, new)` carries the data directory from where an earlier app identifier kept
  it: a rename, or a copy and then removal when the rename is refused, never over anything already at the
  new place, and safe to run again.

### Fixed

- `tauri-kit-credentials` and `TauriKit.Credentials`: on Windows a secret longer than one Credential Manager
  entry holds (2,560 bytes — 1,280 UTF-16 units, which a signed-in OIDC session passes) is no longer refused.
  It is kept in parts, in one layout both languages read and write: the first entry carries a marker and the
  part count, further parts sit under `<account>/<i>`; reading joins them, and a missing part is an error,
  never a cut secret. Writing a shorter secret or deleting removes the parts left behind. A secret that fits
  is one entry exactly as before, so existing entries read unchanged. A secret may no longer begin with
  `'\0'` (the marker's first character).

### Changed

- `tauri-kit-diagnostics`: `InstallId` is `tauri_kit_state::InstallId`, re-exported — same name and
  methods, one type, so an app's own records and its reports name the installation with the same value.

## [0.22.0] - 2026-10-18

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
- `tauri-kit-diagnostics` (**breaking**): reports are verified on the way back, as events are. `pending_reports` and
  `unsent` take the registry and pass over a line `Registry::verify_report` refuses — a layer or kind that is not a
  plain identifier, a frame no `FrameRule` writes, a detail the registry does not declare for the layer or not
  written as its kind writes it, a malformed version, platform or time — so a line edited into a segment, or
  written by a process holding another registry, is never sent. `Sink::send_pending(agent, registry, dir)`;
  `Registry::default()` declares nothing, for an app whose reports carry no details. A layer's name is a plain
  identifier (`Layer::new` checks it in debug builds; `TauriKit.Diagnostics` throws).
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

[0.28.0]: https://github.com/iyulab/tauri-kit/compare/v0.27.0...v0.28.0
[0.27.0]: https://github.com/iyulab/tauri-kit/compare/v0.26.0...v0.27.0
[0.26.0]: https://github.com/iyulab/tauri-kit/compare/v0.25.0...v0.26.0
[0.25.0]: https://github.com/iyulab/tauri-kit/compare/v0.24.0...v0.25.0
[0.24.0]: https://github.com/iyulab/tauri-kit/compare/v0.23.0...v0.24.0
[0.23.0]: https://github.com/iyulab/tauri-kit/compare/v0.22.0...v0.23.0
[0.22.0]: https://github.com/iyulab/tauri-kit/compare/v0.21.0...v0.22.0
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
