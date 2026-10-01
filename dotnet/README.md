# TauriKit.Sidecar.Loopback

The .NET host side of a desktop app's loopback sidecar — the counterpart of the
[`tauri-kit-sidecar`](https://github.com/iyulab/tauri-kit/tree/main/crates/sidecar) crate's
`loopback` feature, released under the same version.

The app starts the sidecar with a fresh random token in an environment variable. The sidecar:

- listens on 127.0.0.1 only, on a port the system picks;
- announces that port as a line on standard output, which the app waits for;
- answers only requests carrying the token as `Authorization: Bearer <token>`, compared in
  constant time;
- reports an unexpected failure as its exception type and the innermost frame in the app's own
  code — never the message, which can quote the data the request was about.

The variable's name and the line's prefix are the app's, and must match what the Rust side is
given.

```csharp
using TauriKit.Sidecar.Loopback;

var token = LoopbackHost.ReadToken("HELPER_TOKEN");
if (token is null)
{
    Console.Error.WriteLine("HELPER_TOKEN (32+ characters) is required");
    return 2;
}

var app = LoopbackHost.CreateSlimBuilder(args).Build();
app.UseBearerToken(token);
app.UseFaults(new FaultOptions
{
    OwnNamespaces = ["MyCompany."],
    StatusFor = e => e is KeyNotFoundException ? StatusCodes.Status404NotFound : null,
});
app.MapGet("/health", () => "ok");
await app.RunAnnouncingAsync("helper ready port=");
return 0;
```

The library is ahead-of-time compilation compatible: a sidecar published with `PublishAot`
can use it. Tests can host the same pipeline with `CreateSlimBuilder(args)` and `StartAsync`, and
reach it over `HttpClient` at `app.Urls`.

## License

[MIT](https://github.com/iyulab/tauri-kit/blob/main/LICENSE)
