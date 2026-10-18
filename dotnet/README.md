# TauriKit.Sidecar.Loopback

The .NET side of a desktop app's loopback backend — the counterpart of the
[`tauri-kit-sidecar`](https://github.com/iyulab/tauri-kit/tree/main/crates/sidecar) crate's
`loopback` feature, released under the same version. Two packages, one namespace:

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
