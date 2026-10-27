# tauri-kit for .NET

The .NET halves of tauri-kit, released under the same version as the crates: the loopback backend
(below), the credential store for Windows and macOS ([`TauriKit.Credentials`](#tauritkitcredentials)) and declared
diagnostics events and error reports ([`TauriKit.Diagnostics`](#tauritkitdiagnostics)).

## TauriKit.Sidecar.Loopback

The .NET side of a desktop app's loopback backend — the counterpart of the
[`tauri-kit-sidecar`](https://github.com/iyulab/tauri-kit/tree/main/crates/sidecar) crate's
`loopback` feature. Two packages, one namespace:

| Package | For | Holds |
|---|---|---|
| `TauriKit.Sidecar.Loopback` | anyone — no web framework | the discovery file, credentials and callers, the fault record, the caller's `LoopbackDiscoveryHandler` |
| `TauriKit.Sidecar.Loopback.AspNetCore` | the backend (ASP.NET Core) | `LoopbackHost`, authentication, the Host check, `UseFaults` |

The backend listens on 127.0.0.1 only, on a port the system picks, and its callers learn where it is
in one of two ways:

- **Started by the app as its child.** The app passes a fresh random token in an environment
  variable (`LoopbackCredentials.ReadToken`), and the backend announces its port as a line on standard
  output (`RunAnnouncingAsync`), which the app waits for.
  When a web view's origin includes the backend's port (it loads pages from the backend, or a page
  talks to it), `LoopbackPort.StartRememberedAsync` starts it on the port it had last time, kept in a
  file the app names — so what the web view keeps per origin survives a restart. Only when that port
  stays taken does it move, and the readiness line then says `previousPort=…` (`ready_previous_port`
  on the Rust side).
- **Living on its own**, outliving the app's windows. The backend issues its credential itself and
  publishes it with its port in a discovery file (`RunPublishingAsync`). Only the person running the
  backend can read that file: on Windows its access list grants that user and the system and
  nothing else, and elsewhere its mode is 0600. The file is written beside its place and moved in,
  so a reader never sees half of one. It carries the process id and start time, so
  `DiscoveryFile.TryRead` tells a file left by a backend that died (`Stale`) from a missing or
  damaged one. A new credential is issued on every start, so a caller re-reads the file once when
  it gets a 401.

Either way, requests are checked the same way:

- **Every route needs a registered caller unless it is named open.** `AddLoopbackAuthentication`
  makes bearer authentication against `LoopbackCredentials` the fallback policy, so a route added
  later is closed until it is marked `AllowAnonymous()`. A request with no registered credential is
  answered 401, including one to a path no route serves.
- **Each caller is told apart.** A backend that runs less-trusted workers (agents, plugins, helper
  processes) beside an owner-facing shell issues each worker its own credential, tied to a kind and
  optionally a subject: `credentials.Issue("agent-session", sessionId)`. A route admits only the
  kinds it names (`RequireCaller("owner")`, 403 otherwise), and a handler reads
  `context.User.LoopbackCaller()`. `Revoke` stops a credential when its worker is done. Credentials
  live in memory only. A presented credential is looked up by its SHA-256 digest, so the time a
  lookup takes says nothing about how close a guess was.
- **Only this computer's names are answered.** `UseLoopbackHostCheck` answers 400 when the `Host`
  header is not `127.0.0.1`, `localhost` or `[::1]` with the port the request came in on. Optionally
  it also accepts names under `localhost`, which browsers resolve to this computer themselves. A web
  page whose own name was pointed at 127.0.0.1 (DNS rebinding) is refused even on an open route.
- **Failures don't leak, and they read as standard problems.** `UseFaults` answers an unexpected
  failure as an RFC 9457 problem (`application/problem+json`, `type` · `title` · `status`) whose
  `fault` member is its exception type and the frames in the app's own code it passed through,
  innermost first (`"fault":{"type","at","frames"}`). It never includes the message, which can quote
  the data the request was about. An exception the app expects (`StatusFor`) is answered as a problem
  that says only its status. `Fault.Of` reads a failure away from any request the same way. On the
  Rust side, `Response::problem` reads any failed answer as a `Problem` — with the `reason` member an
  app sets on its own problems (`Results.Problem(..., extensions: { ["reason"] = "quota" })`) — and
  `Response::fault` its `fault`.
- **It can be asked to quit, and quits when its app is gone.** `MapShutdown` adds a `POST` route that
  answers 202 and stops the host, which finishes the requests under way — the app asks before it
  stops the process. `AddParentProcessWatch(pid)` stops the host when the app that started it exits,
  or at once when it is already gone: the other half of the job object, for a platform without one
  or an app that was killed. For "is it still answering", ASP.NET Core's own health checks serve
  (`AddHealthChecks`, `MapHealthChecks`) — the Rust side's supervisor probes the route the app names.

The names are the app's: the variable, the line's prefix, the file's path and the kinds.

```csharp
using TauriKit.Sidecar.Loopback;

var credentials = new LoopbackCredentials();
var owner = credentials.Issue("owner");

var builder = LoopbackHost.CreateSlimBuilder(args);
builder.Services.AddLoopbackAuthentication(credentials);
var app = builder.Build();
app.UseLoopbackHostCheck();
app.UseFaults(new FaultOptions
{
    OwnNamespaces = ["MyCompany."],
    StatusFor = e => e is KeyNotFoundException ? StatusCodes.Status404NotFound : null,
});
app.MapHealthChecks("/health").AllowAnonymous(); // with builder.Services.AddHealthChecks()
app.MapShutdown(); // POST /shutdown, for a caller with a credential
app.MapPost("/settings", SaveSettings).RequireCaller("owner");
app.MapPost("/sessions", (HttpContext context) =>
{
    // A worker gets its own credential in its environment, never the owner's.
    var session = Guid.NewGuid().ToString("N");
    return new { session, token = credentials.Issue("agent-session", session) };
}).RequireCaller("owner");
await app.RunPublishingAsync(Path.Combine(stateFolder, "run", "backend.json"), owner);
```

A backend started as a child registers the token its parent passed instead:

```csharp
var token = LoopbackCredentials.ReadToken("HELPER_TOKEN");
if (token is null)
{
    Console.Error.WriteLine("HELPER_TOKEN (32+ characters) is required");
    return 2;
}
var credentials = new LoopbackCredentials();
credentials.Register(token, "app");
// … the same builder and routes, and: stop when the app that started us is gone.
if (int.TryParse(Environment.GetEnvironmentVariable("HELPER_PARENT"), out var parent))
    builder.Services.AddParentProcessWatch(parent);
await app.RunAnnouncingAsync("helper ready port=");
```

A caller that finds the backend by its file — a launcher that supervises it, a command-line tool —
takes only `TauriKit.Sidecar.Loopback`. `LoopbackDiscoveryHandler` sends each request to the port
the file names with the credential it holds, and when the backend answers 401 or cannot be reached
and the file now names another run (it restarted), sends the request once more to that run. Content
sent twice must be readable twice (a string, bytes, a form — not a one-pass stream).

```csharp
using var client = LoopbackDiscoveryHandler.CreateClient(Path.Combine(stateFolder, "run", "backend.json"));
try
{
    using var answer = await client.PostAsync("/shutdown", content: null);
}
catch (LoopbackUnavailableException e) when (e.Problem is DiscoveryProblem.Missing or DiscoveryProblem.Stale)
{
    // Not running: nothing to stop.
}
```

Both libraries are compatible with ahead-of-time compilation, so a backend published with `PublishAot`
can use it. Tests can host the same pipeline with `CreateSlimBuilder(args)` and `StartAsync`, and
reach it over `HttpClient` at `app.Urls`.

## License

[MIT](https://github.com/iyulab/tauri-kit/blob/main/LICENSE)

## TauriKit.Credentials

The .NET twin of the [`tauri-kit-credentials`](https://github.com/iyulab/tauri-kit/tree/main/crates/credentials)
crate, for Windows and macOS: the same entries, and the same separation of builds (`<service>.test`,
`<service>.dev`, `<service>`). A Rust shell and its .NET backend built the same way read and write one secret.

| Platform | Store | Entry |
|---|---|---|
| Windows | Credential Manager | a generic credential named `<account>.<service>`, its user name the account, the secret as UTF-16 (long secrets in parts) |
| macOS | login keychain | a generic password with service `<service>` and account `<account>`, the secret as UTF-8 (one item, never in parts) |

On macOS the keychain answers the program that made an item without asking, and asks the person before any other
program reads it — unless both are signed as the same developer. An app that signs every build with its Developer ID keeps reading its own entries across updates; an
ad-hoc signed development build is a new identity with every build, so keep its secrets in memory instead.

```csharp
var credentials = new Credentials("com.example.app", BuildKinds.Of(typeof(Program).Assembly));
credentials.Set("api-key", "s3cret");
string? key = credentials.Get("api-key"); // null when there is none
credentials.Delete("api-key");            // not there is not an error
```

`BuildKinds.Of` reads whether the app's assembly was compiled without optimization (a Debug build →
`Dev`); a test run passes `BuildKind.Test` itself. Linux is not covered on the .NET side.

## TauriKit.Diagnostics

The .NET side of the [`tauri-kit-diagnostics`](https://github.com/iyulab/tauri-kit/tree/main/crates/diagnostics)
crate's declared events, for a backend process: the same registry JSON (embed it in the app — whoever can
edit it decides what is written), refused by the same rules, and lines in the crate's shape, so the app's
shell verifies them against its registry and sends them (`pending_events`). Fields are only of closed
kinds — a choice among declared values, some of them (`Subset`), a count bucketed or capped at 1000,
true/false, a duration — so none can carry text; what does not fit is dropped and said once, without the
undeclared name or value. Each line carries a random id of its own, never the installation's. One
writer per file: give each process its own segment (below).

Each stream — events, reports — is a folder the shell reads, with one segment per process that writes it:
`Segment.Open(dir)` gives the backend a file of its own and holds a lock beside it until the segment is disposed
or the process goes, so the shell (the crate's `segments` and `tidy`) reads it while it is written and deletes it
only once the backend is gone and every line has been sent. Keep the segment open for as long as anything writes.

```csharp
var registry = Registry.FromJson(embeddedRegistryJson);
var exported = registry.Event("export.finished");          // a misspelling fails here, at start
using var segment = Segment.Open(Path.Combine(logDir, "events"));
var events = new Events(registry, appVersion, segment.File);
events.Record(exported, ("format", Value.Choice("csv")), ("rows", Value.Count(rows)), ("took", Value.Duration(elapsed)));
```

Error reports take the same allowlist as the crate's: the layer's name, the kind when it is a plain identifier, the
method names of the stack (`Namespace.Type.Method` — arguments and file names dropped; a .NET stack's file names are
paths on the machine that built it), and only the details the registry declares for the layer under `reports`, each
of its kind. The exception's message is never read. Each failure — the same layer, kind and first frame — is written
once, at most 50, then one `ReportsCapped`; the shell sends the file with the crate's `pending`.

```csharp
var host = registry.Layer("host");
using var reportSegment = Segment.Open(Path.Combine(logDir, "reports"));
var reports = new Reporter(reportSegment.File);
try { … }
catch (IOException e) { reports.Record(host.Report(e, appVersion, ("status", "timeout"))); }
```

The crate's `tests/cases` hold the registry and the lines both languages must write for the same scene.
