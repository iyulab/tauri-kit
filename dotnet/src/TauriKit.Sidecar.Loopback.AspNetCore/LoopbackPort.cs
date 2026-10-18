using System.Text;
using System.Text.Json;
using Microsoft.AspNetCore.Builder;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// Keeps a loopback backend on the port it had last time. A web view that loads pages from the backend, or talks to
/// it from a page, has an origin that includes the port — and everything the web view keeps per origin (storage,
/// granted permissions) is lost when the port changes. So the port is remembered in a file the app names, usually in
/// its data folder, and taken again at the next start; only when it stays taken does the backend move, and the
/// readiness line says where from (<see cref="PreviousPortField"/>), so the app can tell the person what moved.
/// </summary>
/// <example>
/// <code>
/// var started = await LoopbackPort.StartRememberedAsync(Path.Combine(dataDir, "backend-port.json"), port =>
/// {
///     var builder = LoopbackHost.CreateSlimBuilder(args, port);
///     // … services …
///     var app = builder.Build();
///     // … middleware, routes …
///     return app;
/// });
/// await started.RunAnnouncingAsync("helper ready port=");  // adds previousPort=… when it had to move
/// </code>
/// </example>
public static class LoopbackPort
{
    /// <summary>The readiness-line field naming the port a backend had to give up: <c>previousPort=&lt;port&gt;</c>.</summary>
    public const string PreviousPortField = "previousPort";

    /// <summary>The port remembered in <paramref name="path"/>, or <see langword="null"/> when there is none or it cannot be read.</summary>
    public static int? Recall(string path)
    {
        ArgumentNullException.ThrowIfNull(path);
        try
        {
            using var document = JsonDocument.Parse(File.ReadAllBytes(path));
            if (document.RootElement.ValueKind == JsonValueKind.Object
                && document.RootElement.TryGetProperty("port", out var port)
                && port.TryGetInt32(out var value) && value is > 0 and <= 65535)
                return value;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            // None yet, or one that cannot be used: the system picks a port, which is then remembered.
        }

        return null;
    }

    /// <summary>Remembers <paramref name="port"/> in <paramref name="path"/> — written aside and moved into place, so a reader never sees half a file.</summary>
    public static void Remember(string path, int port)
    {
        ArgumentNullException.ThrowIfNull(path);
        ArgumentOutOfRangeException.ThrowIfLessThan(port, 1);
        ArgumentOutOfRangeException.ThrowIfGreaterThan(port, 65535);
        var folder = Path.GetDirectoryName(Path.GetFullPath(path));
        if (folder is not null) Directory.CreateDirectory(folder);
        var aside = path + ".tmp";
        File.WriteAllText(aside, $"{{ \"port\": {port.ToString(System.Globalization.CultureInfo.InvariantCulture)} }}\n", new UTF8Encoding(false));
        File.Move(aside, path, overwrite: true);
    }

    /// <summary>
    /// Builds and starts the backend on the port remembered in <paramref name="portFile"/> — trying it
    /// <paramref name="attempts"/> times, <paramref name="retryDelay"/> apart (default 500 ms), since its likeliest
    /// holder is the backend's own previous run, still stopping — and otherwise on a port the system picks. The port
    /// it ends up on is remembered.
    /// </summary>
    /// <param name="portFile">Where the port is remembered (the app's; one file per backend).</param>
    /// <param name="build">Builds the backend to listen on the given port (0: any) — for example with
    /// <see cref="LoopbackHost.CreateSlimBuilder"/>. Called again for each attempt; a build whose port was taken is disposed.</param>
    /// <param name="attempts">How many times the remembered port is tried.</param>
    /// <param name="retryDelay">The wait between tries.</param>
    /// <param name="cancellationToken">Cancels waiting.</param>
    public static async Task<RememberedStart> StartRememberedAsync(string portFile, Func<int, WebApplication> build, int attempts = 5,
        TimeSpan? retryDelay = null, CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(portFile);
        ArgumentNullException.ThrowIfNull(build);
        ArgumentOutOfRangeException.ThrowIfLessThan(attempts, 1);
        var delay = retryDelay ?? TimeSpan.FromMilliseconds(500);

        var remembered = Recall(portFile);
        if (remembered is { } port)
        {
            for (var attempt = 0; attempt < attempts; attempt++)
            {
                if (attempt > 0) await Task.Delay(delay, cancellationToken).ConfigureAwait(false);
                var app = build(port);
                try
                {
                    await app.StartAsync(cancellationToken).ConfigureAwait(false);
                    return new RememberedStart(app, null);
                }
                catch (IOException)
                {
                    // Still taken (Kestrel's AddressInUseException is an IOException).
                    await app.DisposeAsync().ConfigureAwait(false);
                }
            }
        }

        var fresh = build(0);
        await fresh.StartAsync(cancellationToken).ConfigureAwait(false);
        Remember(portFile, fresh.Port());
        return new RememberedStart(fresh, remembered);
    }
}

/// <summary>A backend started by <see cref="LoopbackPort.StartRememberedAsync"/>.</summary>
/// <param name="App">The started backend.</param>
/// <param name="PreviousPort">
/// The remembered port, when it could not be taken again and the backend moved — every origin on it moved too.
/// <see langword="null"/> when nothing moved (a first start included).
/// </param>
public sealed record RememberedStart(WebApplication App, int? PreviousPort)
{
    /// <summary>
    /// Writes the readiness line for the started backend — with <see cref="LoopbackPort.PreviousPortField"/> when it
    /// moved, then <paramref name="fields"/> — and runs until it is shut down. See <see cref="LoopbackHost.ReadyLine"/>.
    /// </summary>
    public async Task RunAnnouncingAsync(string readyPrefix, TextWriter? output = null, IEnumerable<KeyValuePair<string, string>>? fields = null)
    {
        IEnumerable<KeyValuePair<string, string>> moved = PreviousPort is { } previous
            ? [new(LoopbackPort.PreviousPortField, previous.ToString(System.Globalization.CultureInfo.InvariantCulture))]
            : [];
        await LoopbackHost.AnnounceStartedAsync(App, readyPrefix, output, moved.Concat(fields ?? [])).ConfigureAwait(false);
    }
}
