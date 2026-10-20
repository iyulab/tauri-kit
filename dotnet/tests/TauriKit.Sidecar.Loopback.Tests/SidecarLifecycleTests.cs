using System.Diagnostics;
using System.Net;
using System.Net.Http.Headers;
using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;

namespace TauriKit.Sidecar.Loopback.Tests;

public sealed class SidecarLifecycleTests
{
    private const string Token = "0123456789abcdef0123456789abcdef-test";

    private static async Task<(WebApplication App, HttpClient Http)> StartHostAsync()
    {
        var credentials = new LoopbackCredentials();
        credentials.Register(Token, "app");
        var builder = LoopbackHost.CreateSlimBuilder([]);
        builder.Services.AddLoopbackAuthentication(credentials);
        var app = builder.Build();
        app.MapShutdown();
        await app.StartAsync(TestContext.Current.CancellationToken);
        return (app, new HttpClient { BaseAddress = new Uri(app.Urls.First()) });
    }

    private static Task Stopping(IHostApplicationLifetime lifetime)
    {
        var stopping = new TaskCompletionSource();
        lifetime.ApplicationStopping.Register(() => stopping.TrySetResult());
        return stopping.Task;
    }

    [Fact]
    public async Task A_shutdown_request_is_accepted_and_stops_the_host()
    {
        var (app, http) = await StartHostAsync();
        await using var _ = app;
        using var __ = http;
        var stopping = Stopping(app.Lifetime);
        http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", Token);

        using var answer = await http.PostAsync("/shutdown", null, TestContext.Current.CancellationToken);

        Assert.Equal(HttpStatusCode.Accepted, answer.StatusCode);
        await stopping.WaitAsync(TimeSpan.FromSeconds(10), TestContext.Current.CancellationToken);
    }

    [Fact]
    public async Task A_shutdown_request_without_the_credential_is_refused_and_the_host_keeps_running()
    {
        var (app, http) = await StartHostAsync();
        await using var _ = app;
        using var __ = http;

        using var answer = await http.PostAsync("/shutdown", null, TestContext.Current.CancellationToken);

        Assert.Equal(HttpStatusCode.Unauthorized, answer.StatusCode);
        Assert.False(app.Lifetime.ApplicationStopping.IsCancellationRequested);
    }

    [Fact]
    public async Task The_host_stops_when_its_parent_exits()
    {
        using var parent = LongRunning();
        using var host = WatchingHost(parent.Id, id => Process.GetProcessById(id));
        await host.StartAsync(TestContext.Current.CancellationToken);
        var stopping = Stopping(host.Services.GetRequiredService<IHostApplicationLifetime>());
        Assert.False(stopping.IsCompleted);

        parent.Kill(entireProcessTree: true);

        await stopping.WaitAsync(TimeSpan.FromSeconds(10), TestContext.Current.CancellationToken);
        await host.StopAsync(TestContext.Current.CancellationToken);
    }

    [Fact]
    public async Task The_host_stops_at_once_when_its_parent_is_already_gone()
    {
        using var host = WatchingHost(12345, _ => null);
        var stopping = Stopping(host.Services.GetRequiredService<IHostApplicationLifetime>());

        await host.StartAsync(TestContext.Current.CancellationToken);

        await stopping.WaitAsync(TimeSpan.FromSeconds(10), TestContext.Current.CancellationToken);
        await host.StopAsync(TestContext.Current.CancellationToken);
    }

    [Fact]
    public async Task A_web_host_whose_parent_is_already_gone_starts_and_then_stops_without_failing()
    {
        // Registered before the web host's own service, as an app adds it: the watch starts first.
        var credentials = new LoopbackCredentials();
        credentials.Register(Token, "app");
        var builder = LoopbackHost.CreateSlimBuilder([]);
        builder.Services.AddLoopbackAuthentication(credentials);
        builder.Services.AddHostedService(provider => new ParentProcessWatch(12345, provider.GetRequiredService<IHostApplicationLifetime>(), _ => null));
        await using var app = builder.Build();
        var started = false;
        app.Lifetime.ApplicationStarted.Register(() => started = true);
        using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(10));

        await app.RunAsync(limit.Token);

        Assert.True(started, "the server started before it stopped");
        Assert.False(limit.IsCancellationRequested, "it stopped on its own");
    }

    [Fact]
    public void A_process_that_started_after_this_one_is_not_its_parent()
    {
        using var later = LongRunning();
        try
        {
            Assert.Null(ParentProcessWatch.Find(later.Id));
            using var self = ParentProcessWatch.Find(Environment.ProcessId);
            Assert.NotNull(self);
        }
        finally
        {
            later.Kill(entireProcessTree: true);
        }
    }

    [Fact]
    public async Task A_host_watching_a_parent_that_is_running_keeps_running()
    {
        var builder = Host.CreateApplicationBuilder();
        builder.Services.AddParentProcessWatch(Environment.ProcessId);
        using var host = builder.Build();
        await host.StartAsync(TestContext.Current.CancellationToken);
        await Task.Delay(300, TestContext.Current.CancellationToken);

        Assert.False(host.Services.GetRequiredService<IHostApplicationLifetime>().ApplicationStopping.IsCancellationRequested);
        await host.StopAsync(TestContext.Current.CancellationToken);
        Assert.Throws<ArgumentOutOfRangeException>(() => new ServiceCollection().AddParentProcessWatch(0));
    }

    [Fact]
    public void An_id_no_process_has_is_gone()
    {
        Assert.Null(ParentProcessWatch.Find(int.MaxValue - 7));
    }

    private static IHost WatchingHost(int parentId, Func<int, Process?> find)
    {
        var builder = Host.CreateApplicationBuilder();
        builder.Services.AddHostedService(provider => new ParentProcessWatch(parentId, provider.GetRequiredService<IHostApplicationLifetime>(), find));
        return builder.Build();
    }

    private static Process LongRunning()
    {
        var start = OperatingSystem.IsWindows()
            ? new ProcessStartInfo("cmd", "/c ping -n 30 127.0.0.1 >NUL")
            : new ProcessStartInfo("sleep", "30");
        start.UseShellExecute = false;
        start.CreateNoWindow = true;
        return Process.Start(start)!;
    }
}
