using System.Diagnostics;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Routing;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// The host side of a supervised sidecar's life: a way for the app to ask it to quit
/// (<see cref="MapShutdown"/>), and a watch on the app's own process so the sidecar quits when the app
/// is gone without asking (<see cref="AddParentProcessWatch"/>) — on a platform where the app could not
/// tie the sidecar to itself, or when it was killed. For "is it still answering", use ASP.NET Core's own
/// health checks: <c>builder.Services.AddHealthChecks()</c> and <c>app.MapHealthChecks("/health")</c>,
/// with a check per thing the sidecar needs before it can do its work.
/// </summary>
public static class SidecarLifecycle
{
    /// <summary>
    /// A <c>POST</c> route that asks the host to stop: it answers 202 and then stops the application, which
    /// finishes the requests already under way. Like every other route it requires the caller's credential
    /// unless it is opened by name. The app calls it before it stops the process, giving the sidecar the
    /// chance to finish its writes.
    /// </summary>
    public static IEndpointConventionBuilder MapShutdown(this IEndpointRouteBuilder endpoints, string pattern = "/shutdown")
    {
        ArgumentNullException.ThrowIfNull(endpoints);
        ArgumentException.ThrowIfNullOrWhiteSpace(pattern);
        // A request delegate, not a lambda with parameters: that overload reflects on the delegate, which an
        // ahead-of-time compiled sidecar cannot do.
        return endpoints.MapPost(pattern, (RequestDelegate)(context =>
        {
            context.RequestServices.GetRequiredService<IHostApplicationLifetime>().StopApplication();
            context.Response.StatusCode = StatusCodes.Status202Accepted;
            return Task.CompletedTask;
        }));
    }

    /// <summary>
    /// Stops the application when the process with id <paramref name="parentProcessId"/> — the app that
    /// started this sidecar — exits, or at once when it is already gone. How the id arrives (an argument,
    /// an environment variable) is the app's. A process with that id that started after this one is not
    /// the parent but a later process that was given its id, and counts as gone.
    /// </summary>
    public static IServiceCollection AddParentProcessWatch(this IServiceCollection services, int parentProcessId)
    {
        ArgumentNullException.ThrowIfNull(services);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(parentProcessId);
        return services.AddHostedService(provider => new ParentProcessWatch(parentProcessId, provider.GetRequiredService<IHostApplicationLifetime>()));
    }
}

// `find` looks the parent up: Find, unless a test hands in a process of its own.
internal sealed class ParentProcessWatch(int parentProcessId, IHostApplicationLifetime lifetime, Func<int, Process?>? find = null) : BackgroundService
{
    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        using var parent = (find ?? Find)(parentProcessId);
        if (parent is not null)
        {
            try
            {
                await parent.WaitForExitAsync(stoppingToken).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                return;
            }
        }

        if (!stoppingToken.IsCancellationRequested) lifetime.StopApplication();
    }

    /// <summary>The parent, or <c>null</c> when it is gone — no process has the id, or the one that has it
    /// started after this process did and so cannot be the one that started it.</summary>
    internal static Process? Find(int id)
    {
        Process process;
        try
        {
            process = Process.GetProcessById(id);
        }
        catch (Exception e) when (e is ArgumentException or InvalidOperationException)
        {
            return null; // no process has the id
        }

        if (StartedAfterThis(process))
        {
            process.Dispose();
            return null;
        }

        return process;
    }

    /// <summary>Whether <paramref name="process"/> started after this one. A start time that cannot be read
    /// does not make it a stranger: stopping because of what could not be seen would end a sidecar whose app
    /// is still there.</summary>
    private static bool StartedAfterThis(Process process)
    {
        try
        {
            using var self = Process.GetCurrentProcess();
            return process.StartTime > self.StartTime;
        }
        catch (Exception e) when (e is InvalidOperationException or System.ComponentModel.Win32Exception or NotSupportedException)
        {
            return false;
        }
    }
}
