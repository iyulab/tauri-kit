using System.Net;
using System.Security.Cryptography;
using System.Text;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// The host side of a loopback sidecar. The desktop app starts the sidecar with a fresh random
/// token in an environment variable; the sidecar listens on 127.0.0.1 only, on a port the system
/// picks, announces that port as a line on standard output, and answers only requests that carry
/// the token as a bearer credential. The variable's name and the line's prefix are the app's.
/// </summary>
/// <example>
/// <code>
/// var token = LoopbackHost.ReadToken("HELPER_TOKEN");
/// if (token is null) { Console.Error.WriteLine("HELPER_TOKEN (32+ characters) is required"); return 2; }
/// var builder = LoopbackHost.CreateSlimBuilder(args);
/// var app = builder.Build();
/// app.UseBearerToken(token);
/// app.UseFaults(new FaultOptions { OwnNamespaces = ["MyCompany."] });
/// app.MapGet("/health", () => "ok");
/// await app.RunAnnouncingAsync("helper ready port=");
/// return 0;
/// </code>
/// </example>
public static class LoopbackHost
{
    /// <summary>
    /// The token from <paramref name="environmentVariable"/>, or <c>null</c> when it is missing or
    /// shorter than <paramref name="minimumLength"/> — a short token is a mistake, not a secret.
    /// </summary>
    public static string? ReadToken(string environmentVariable, int minimumLength = 32)
    {
        var token = Environment.GetEnvironmentVariable(environmentVariable);
        return string.IsNullOrEmpty(token) || token.Length < minimumLength ? null : token;
    }

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
    /// Answers 401 to every request whose <c>Authorization</c> header is not <c>Bearer &lt;token&gt;</c>,
    /// compared in constant time so the answer's timing says nothing about how much of a guess was
    /// right. Add it before everything else.
    /// </summary>
    public static IApplicationBuilder UseBearerToken(this IApplicationBuilder app, string token)
    {
        ArgumentNullException.ThrowIfNull(app);
        var expected = Encoding.UTF8.GetBytes("Bearer " + token);
        return app.Use(async (context, next) =>
        {
            var given = Encoding.UTF8.GetBytes(context.Request.Headers.Authorization.ToString());
            if (!CryptographicOperations.FixedTimeEquals(given, expected))
            {
                context.Response.StatusCode = StatusCodes.Status401Unauthorized;
                return;
            }
            await next(context).ConfigureAwait(false);
        });
    }

    /// <summary>
    /// Turns an exception escaping a request into an answer: the status
    /// <see cref="FaultOptions.StatusFor"/> gives it with no body, or else 500 with a
    /// <see cref="FaultResponse"/> — the exception's type and the innermost frame in the app's own
    /// code, never the message, which can quote the data the request was about.
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
                    return;
                }
                context.Response.StatusCode = StatusCodes.Status500InternalServerError;
                await context.Response.WriteAsJsonAsync(new FaultResponse(Fault.Of(e, options.OwnNamespaces)), LoopbackJson.Default.FaultResponse).ConfigureAwait(false);
            }
        });
    }

    /// <summary>The port the application listens on, once started.</summary>
    public static int Port(this WebApplication app)
    {
        ArgumentNullException.ThrowIfNull(app);
        return new Uri(app.Urls.First()).Port;
    }

    /// <summary>The readiness line for <paramref name="port"/>: the prefix, then the port.</summary>
    public static string ReadyLine(string readyPrefix, int port) => readyPrefix + port.ToString(System.Globalization.CultureInfo.InvariantCulture);

    /// <summary>
    /// Starts the application, writes the readiness line (to standard output unless
    /// <paramref name="output"/> is given) once it listens, and runs until it is shut down.
    /// </summary>
    public static async Task RunAnnouncingAsync(this WebApplication app, string readyPrefix, TextWriter? output = null)
    {
        ArgumentNullException.ThrowIfNull(app);
        await app.StartAsync().ConfigureAwait(false);
        var writer = output ?? Console.Out;
        await writer.WriteLineAsync(ReadyLine(readyPrefix, app.Port())).ConfigureAwait(false);
        await writer.FlushAsync().ConfigureAwait(false);
        await app.WaitForShutdownAsync().ConfigureAwait(false);
    }
}

/// <summary>How <see cref="LoopbackHost.UseFaults"/> answers an exception.</summary>
public sealed class FaultOptions
{
    /// <summary>
    /// Namespace prefixes of the app's own code (for example <c>"MyCompany."</c>): the innermost stack
    /// frame in one of them is reported as where the failure happened.
    /// </summary>
    public IReadOnlyList<string> OwnNamespaces { get; init; } = [];

    /// <summary>
    /// The status to answer an exception the app expects with (a conflict, say), or <c>null</c> to
    /// report it as a fault.
    /// </summary>
    public Func<Exception, int?>? StatusFor { get; init; }
}
