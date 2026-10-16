using System.Net;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// The host side of a loopback backend: it listens on 127.0.0.1 only, on a port the system picks, and tells its
/// callers where it is in one of two ways. Started by the app as its child, it announces the port as a line on
/// standard output (<see cref="RunAnnouncingAsync"/>) and accepts the credential the parent passed in an environment
/// variable (<see cref="LoopbackCredentials.ReadToken"/>). Living on its own, outliving the app's windows, it issues
/// its credential itself and publishes it with the port in an owner-only discovery file
/// (<see cref="RunPublishingAsync"/>). Either way the requests are checked by <see cref="LoopbackAuthentication"/>, and
/// the names — the variable, the line's prefix, the file's path — are the app's.
/// </summary>
/// <example>
/// <code>
/// var token = LoopbackCredentials.ReadToken("HELPER_TOKEN");
/// if (token is null) { Console.Error.WriteLine("HELPER_TOKEN (32+ characters) is required"); return 2; }
/// var credentials = new LoopbackCredentials();
/// credentials.Register(token, "app");
/// var builder = LoopbackHost.CreateSlimBuilder(args);
/// builder.Services.AddLoopbackAuthentication(credentials);
/// var app = builder.Build();
/// app.UseLoopbackHostCheck();
/// app.UseFaults(new FaultOptions { OwnNamespaces = ["MyCompany."] });
/// app.MapHealthChecks("/health").AllowAnonymous(); // with builder.Services.AddHealthChecks()
/// app.MapShutdown();
/// await app.RunAnnouncingAsync("helper ready port=");
/// return 0;
/// </code>
/// </example>
public static class LoopbackHost
{
    /// <summary>
    /// A slim web application builder with no logging providers (a sidecar's console is the app's
    /// pipe, not a log) that listens on 127.0.0.1 only. <paramref name="port"/> 0 lets the system pick;
    /// <c>null</c> leaves the listening address to the caller (an in-memory test server, say).
    /// </summary>
    public static WebApplicationBuilder CreateSlimBuilder(string[] args, int? port = 0)
    {
        var builder = WebApplication.CreateSlimBuilder(args);
        builder.Logging.ClearProviders();
        if (port is { } p) builder.WebHost.ConfigureKestrel(k => k.Listen(IPAddress.Loopback, p));
        return builder;
    }

    /// <summary>
    /// Answers 400 to a request whose <c>Host</c> header is not a loopback name with the port the request came in on —
    /// <c>127.0.0.1</c>, <c>localhost</c> or <c>[::1]</c> (and, with <paramref name="localhostSubdomains"/>, any name
    /// under <c>localhost</c>, which browsers resolve to this computer themselves). A web page whose own name was
    /// pointed at 127.0.0.1 (DNS rebinding) reaches the port but sends its own name, so this refuses it even on a route
    /// open to anonymous callers. A refused name never reaches a route; authentication, which the application runs
    /// first, may already have answered 401 to a request that carries no credential. A request with no socket under it
    /// — an in-memory test server's, whose local port is 0 — has no port to match, and only its name is checked.
    /// </summary>
    public static IApplicationBuilder UseLoopbackHostCheck(this IApplicationBuilder app, bool localhostSubdomains = false)
    {
        ArgumentNullException.ThrowIfNull(app);
        return app.Use(async (context, next) =>
        {
            if (!IsLoopbackHost(context.Request.Host, context.Connection.LocalPort, localhostSubdomains))
            {
                context.Response.StatusCode = StatusCodes.Status400BadRequest;
                return;
            }
            await next(context).ConfigureAwait(false);
        });
    }

    internal static bool IsLoopbackHost(HostString host, int localPort, bool localhostSubdomains)
    {
        if (localPort != 0 && host.Port != localPort) return false;
        var name = host.Host;
        return name is "127.0.0.1" or "[::1]"
            || name.Equals("localhost", StringComparison.OrdinalIgnoreCase)
            || (localhostSubdomains && name.EndsWith(".localhost", StringComparison.OrdinalIgnoreCase) && name.Length > ".localhost".Length);
    }

    /// <summary>
    /// Turns an exception escaping a request into an RFC 9457 problem (<c>application/problem+json</c>):
    /// the status <see cref="FaultOptions.StatusFor"/> gives it, as a <see cref="StatusProblem"/>, or else
    /// 500 with a <see cref="FaultProblem"/> — whose <c>fault</c> member holds the exception's type and the
    /// frames in the app's own code, never the message, which can quote the data the request was about.
    /// </summary>
    public static IApplicationBuilder UseFaults(this IApplicationBuilder app, FaultOptions options)
    {
        ArgumentNullException.ThrowIfNull(app);
        ArgumentNullException.ThrowIfNull(options);
        return app.Use(async (context, next) =>
        {
            try
            {
                await next(context).ConfigureAwait(false);
            }
            catch (Exception e) when (!context.Response.HasStarted)
            {
                if (options.StatusFor?.Invoke(e) is { } status)
                {
                    context.Response.StatusCode = status;
                    var title = Microsoft.AspNetCore.WebUtilities.ReasonPhrases.GetReasonPhrase(status);
                    await context.Response.WriteAsJsonAsync(new StatusProblem("about:blank", title, status), FaultJson.Default.StatusProblem, ProblemJson).ConfigureAwait(false);
                    return;
                }
                context.Response.StatusCode = StatusCodes.Status500InternalServerError;
                var problem = FaultProblem.Of(Fault.Of(e, options.OwnNamespaces, options.MaxFrames));
                await context.Response.WriteAsJsonAsync(problem, FaultJson.Default.FaultProblem, ProblemJson).ConfigureAwait(false);
            }
        });
    }

    private const string ProblemJson = "application/problem+json";

    /// <summary>The port the application listens on, once started.</summary>
    public static int Port(this WebApplication app)
    {
        ArgumentNullException.ThrowIfNull(app);
        return new Uri(app.Urls.First()).Port;
    }

    /// <summary>
    /// The readiness line for <paramref name="port"/>: the prefix, then the port, then each of <paramref name="fields"/>
    /// as a <c>key=value</c> word — what a backend says beyond where it listens (the port it had to give up, say). The
    /// Rust side reads them with <c>ready_fields</c>.
    /// </summary>
    /// <exception cref="ArgumentException">A key or value is empty or holds white space, or a key holds <c>=</c>.</exception>
    public static string ReadyLine(string readyPrefix, int port, IEnumerable<KeyValuePair<string, string>>? fields = null)
    {
        var line = new System.Text.StringBuilder(readyPrefix).Append(port.ToString(System.Globalization.CultureInfo.InvariantCulture));
        foreach (var (key, value) in fields ?? [])
        {
            if (!IsWord(key) || key.Contains('=', StringComparison.Ordinal) || !IsWord(value))
                throw new ArgumentException($"A readiness field is one word each side of '=': '{key}'.", nameof(fields));
            line.Append(' ').Append(key).Append('=').Append(value);
        }

        return line.ToString();

        static bool IsWord(string text) => text.Length > 0 && !text.Any(char.IsWhiteSpace);
    }

    /// <summary>
    /// Starts the application, writes the readiness line (to standard output unless
    /// <paramref name="output"/> is given) once it listens — with <paramref name="fields"/> after the port, see
    /// <see cref="ReadyLine"/> — and runs until it is shut down.
    /// </summary>
    public static async Task RunAnnouncingAsync(this WebApplication app, string readyPrefix, TextWriter? output = null, IEnumerable<KeyValuePair<string, string>>? fields = null)
    {
        ArgumentNullException.ThrowIfNull(app);
        await app.StartAsync().ConfigureAwait(false);
        var line = ReadyLine(readyPrefix, app.Port(), fields);
        var writer = output ?? Console.Out;
        await writer.WriteLineAsync(line).ConfigureAwait(false);
        await writer.FlushAsync().ConfigureAwait(false);
        await app.WaitForShutdownAsync().ConfigureAwait(false);
    }

    /// <summary>
    /// Starts the application, publishes its port and <paramref name="credential"/> in the discovery file at
    /// <paramref name="discoveryPath"/> once it listens (see <see cref="DiscoveryFile"/>), runs until it is shut down,
    /// and then removes the file — unless another run has replaced it since. For a backend that outlives the app's
    /// windows, which callers find rather than start.
    /// </summary>
    public static async Task RunPublishingAsync(this WebApplication app, string discoveryPath, string credential)
    {
        ArgumentNullException.ThrowIfNull(app);
        await app.StartAsync().ConfigureAwait(false);
        var published = DiscoveryFile.Publish(discoveryPath, app.Port(), credential);
        try
        {
            await app.WaitForShutdownAsync().ConfigureAwait(false);
        }
        finally
        {
            DiscoveryFile.RemoveIfStill(discoveryPath, published);
        }
    }
}

/// <summary>How <see cref="LoopbackHost.UseFaults"/> answers an exception.</summary>
public sealed class FaultOptions
{
    /// <summary>
    /// Namespace prefixes of the app's own code (for example <c>"MyCompany."</c>): the stack frames in
    /// them are reported, the innermost as where the failure happened.
    /// </summary>
    public IReadOnlyList<string> OwnNamespaces { get; init; } = [];

    /// <summary>How many of the app's own frames a fault carries, innermost first.</summary>
    public int MaxFrames { get; init; } = Fault.DefaultMaxFrames;

    /// <summary>
    /// The status to answer an exception the app expects with (a conflict, say), as a problem that says
    /// only that status, or <c>null</c> to report it as a fault.
    /// </summary>
    public Func<Exception, int?>? StatusFor { get; init; }
}

[JsonSourceGenerationOptions(JsonSerializerDefaults.Web)]
[JsonSerializable(typeof(FaultProblem))]
[JsonSerializable(typeof(StatusProblem))]
internal sealed partial class FaultJson : JsonSerializerContext;
