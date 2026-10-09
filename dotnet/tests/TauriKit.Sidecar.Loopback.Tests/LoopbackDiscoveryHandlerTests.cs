using System.Net;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;

namespace TauriKit.Sidecar.Loopback.Tests;

public sealed class LoopbackDiscoveryHandlerTests : IAsyncDisposable
{
    private readonly string _folder = Path.Combine(Path.GetTempPath(), "tauri-kit-handler-" + Guid.NewGuid().ToString("N"));
    private readonly List<WebApplication> _backends = [];

    public async ValueTask DisposeAsync()
    {
        foreach (var backend in _backends) await backend.DisposeAsync();
        if (Directory.Exists(_folder)) Directory.Delete(_folder, recursive: true);
    }

    private string FilePath => Path.Combine(_folder, "backend.json");

    private static CancellationToken Cancel => TestContext.Current.CancellationToken;

    // A backend as a caller meets it: authenticated by default, echoing what it was sent.
    private async Task<(WebApplication App, LoopbackCredentials Credentials)> StartBackendAsync()
    {
        var credentials = new LoopbackCredentials();
        var builder = LoopbackHost.CreateSlimBuilder([]);
        builder.Services.AddLoopbackAuthentication(credentials);
        var app = builder.Build();
        app.UseLoopbackHostCheck();
        app.MapPost("/echo", async (HttpRequest request) => await new StreamReader(request.Body).ReadToEndAsync());
        await app.StartAsync(Cancel);
        _backends.Add(app);
        return (app, credentials);
    }

    private static async Task<string> EchoAsync(HttpClient client, string text)
    {
        using var content = new StringContent(text);
        using var answer = await client.PostAsync(new Uri("/echo", UriKind.Relative), content, Cancel);
        answer.EnsureSuccessStatusCode();
        return await answer.Content.ReadAsStringAsync(Cancel);
    }

    [Fact]
    public void The_framework_free_package_takes_no_aspnetcore()
    {
        var references = typeof(DiscoveryFile).Assembly.GetReferencedAssemblies().Select(a => a.Name!).ToList();
        Assert.DoesNotContain(references, name => name.StartsWith("Microsoft.AspNetCore", StringComparison.Ordinal));
        Assert.DoesNotContain(references, name => name.StartsWith("Microsoft.Extensions", StringComparison.Ordinal));
    }

    [Fact]
    public async Task A_caller_reaches_the_backend_its_file_names_and_follows_it_to_a_new_run()
    {
        var (first, firstCredentials) = await StartBackendAsync();
        DiscoveryFile.Publish(FilePath, first.Port(), firstCredentials.Issue("owner"));
        using var client = LoopbackDiscoveryHandler.CreateClient(FilePath);
        Assert.Equal("one", await EchoAsync(client, "one"));

        // The backend restarts: another port, another credential. The first request after it fails to reach the
        // old port, reads the file again, and is sent — body and all — to the new run.
        await first.StopAsync(Cancel);
        var (second, secondCredentials) = await StartBackendAsync();
        DiscoveryFile.Publish(FilePath, second.Port(), secondCredentials.Issue("owner"));
        Assert.Equal("two", await EchoAsync(client, "two"));
    }

    [Fact]
    public async Task A_credential_the_backend_no_longer_accepts_is_read_again_once()
    {
        var (app, credentials) = await StartBackendAsync();
        var old = credentials.Issue("owner");
        DiscoveryFile.Publish(FilePath, app.Port(), old);
        using var client = LoopbackDiscoveryHandler.CreateClient(FilePath);
        Assert.Equal("before", await EchoAsync(client, "before"));

        credentials.Revoke(old);
        DiscoveryFile.Publish(FilePath, app.Port(), credentials.Issue("owner"));
        Assert.Equal("after", await EchoAsync(client, "after"));
    }

    [Fact]
    public async Task A_401_with_nothing_new_in_the_file_is_the_answer()
    {
        var (app, _) = await StartBackendAsync();
        DiscoveryFile.Publish(FilePath, app.Port(), new string('z', 43));
        using var client = LoopbackDiscoveryHandler.CreateClient(FilePath);
        using var content = new StringContent("x");
        using var answer = await client.PostAsync(new Uri("/echo", UriKind.Relative), content, Cancel);
        Assert.Equal(HttpStatusCode.Unauthorized, answer.StatusCode);
    }

    [Fact]
    public async Task No_backend_to_call_says_why()
    {
        using var client = LoopbackDiscoveryHandler.CreateClient(FilePath);
        var missing = await Assert.ThrowsAsync<LoopbackUnavailableException>(() => client.GetAsync(new Uri("/echo", UriKind.Relative), Cancel));
        Assert.Equal(DiscoveryProblem.Missing, missing.Problem);

        // A backend that is gone, its file left behind: the request fails to connect and the file is stale.
        var (app, credentials) = await StartBackendAsync();
        DiscoveryFile.Publish(FilePath, app.Port(), credentials.Issue("owner"));
        Assert.Equal("up", await EchoAsync(client, "up"));
        await app.StopAsync(Cancel);
        File.WriteAllText(FilePath, $$"""{"port":{{app.Port()}},"token":"{{new string('a', 32)}}","pid":{{Environment.ProcessId}},"startedAt":978307200000}""");
        var stale = await Assert.ThrowsAsync<LoopbackUnavailableException>(() => EchoAsync(client, "down"));
        Assert.Equal(DiscoveryProblem.Stale, stale.Problem);
    }
}
