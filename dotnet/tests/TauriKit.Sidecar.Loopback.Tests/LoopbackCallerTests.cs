using System.Net;
using System.Net.Http.Headers;
using System.Security.AccessControl;
using System.Security.Principal;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;

namespace TauriKit.Sidecar.Loopback.Tests;

public sealed class LoopbackCallerTests : IAsyncLifetime
{
    private readonly LoopbackCredentials _credentials = new();
    private WebApplication _app = null!;
    private string _owner = null!;

    public async ValueTask InitializeAsync()
    {
        _owner = _credentials.Issue("owner");
        var builder = LoopbackHost.CreateSlimBuilder([]);
        builder.Services.AddLoopbackAuthentication(_credentials);
        _app = builder.Build();
        _app.UseLoopbackHostCheck(localhostSubdomains: true);
        _app.MapGet("/health", () => "ok").AllowAnonymous();
        _app.MapGet("/who", (HttpContext c) => c.User.LoopbackCaller() is { } who ? $"{who.Kind}|{who.Subject}" : "nobody");
        _app.MapPost("/settings", () => "saved").RequireCaller("owner");
        await _app.StartAsync();
    }

    public async ValueTask DisposeAsync() => await _app.DisposeAsync();

    private HttpClient Client(string? credential = null, string? host = null)
    {
        var client = new HttpClient { BaseAddress = new Uri(_app.Urls.First()) };
        if (credential is not null) client.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", credential);
        if (host is not null) client.DefaultRequestHeaders.Host = host;
        return client;
    }

    private static CancellationToken Cancel => TestContext.Current.CancellationToken;

    [Fact]
    public async Task Every_route_needs_a_registered_caller_unless_it_is_named_open()
    {
        using var anonymous = Client();
        using (var health = await anonymous.GetAsync(new Uri("/health", UriKind.Relative), Cancel)) Assert.Equal(HttpStatusCode.OK, health.StatusCode);
        using (var who = await anonymous.GetAsync(new Uri("/who", UriKind.Relative), Cancel)) Assert.Equal(HttpStatusCode.Unauthorized, who.StatusCode);
        // A path no route serves says nothing about which paths exist.
        using (var none = await anonymous.GetAsync(new Uri("/no-such-route", UriKind.Relative), Cancel)) Assert.Equal(HttpStatusCode.Unauthorized, none.StatusCode);
        using var stranger = Client(new string('x', 43));
        using (var refused = await stranger.GetAsync(new Uri("/who", UriKind.Relative), Cancel)) Assert.Equal(HttpStatusCode.Unauthorized, refused.StatusCode);
    }

    [Fact]
    public async Task Each_worker_is_told_apart_and_holds_only_what_its_kind_admits()
    {
        var agent = _credentials.Issue("agent-session", "s-41");
        using var owner = Client(_owner);
        using var worker = Client(agent);

        Assert.Equal("owner|", await owner.GetStringAsync(new Uri("/who", UriKind.Relative), Cancel));
        Assert.Equal("agent-session|s-41", await worker.GetStringAsync(new Uri("/who", UriKind.Relative), Cancel));
        using (var saved = await owner.PostAsync(new Uri("/settings", UriKind.Relative), null, Cancel)) Assert.Equal(HttpStatusCode.OK, saved.StatusCode);
        using (var forbidden = await worker.PostAsync(new Uri("/settings", UriKind.Relative), null, Cancel)) Assert.Equal(HttpStatusCode.Forbidden, forbidden.StatusCode);
    }

    [Fact]
    public async Task A_revoked_credential_is_refused_and_the_others_are_not()
    {
        var agent = _credentials.Issue("agent-session", "s-7");
        using var worker = Client(agent);
        Assert.Equal(HttpStatusCode.OK, (await worker.GetAsync(new Uri("/who", UriKind.Relative), Cancel)).StatusCode);

        Assert.True(_credentials.Revoke(agent));
        Assert.False(_credentials.Revoke(agent));
        Assert.Equal(HttpStatusCode.Unauthorized, (await worker.GetAsync(new Uri("/who", UriKind.Relative), Cancel)).StatusCode);
        using var owner = Client(_owner);
        Assert.Equal(HttpStatusCode.OK, (await owner.GetAsync(new Uri("/who", UriKind.Relative), Cancel)).StatusCode);
    }

    [Theory]
    [InlineData("evil.example:{port}")]
    [InlineData("127.0.0.1:1")]
    [InlineData("127.0.0.1")]
    [InlineData("localhost.example:{port}")]
    public async Task A_host_name_that_is_not_this_computers_own_is_refused_even_on_an_open_route(string host)
    {
        using var client = Client(host: host.Replace("{port}", _app.Port().ToString(System.Globalization.CultureInfo.InvariantCulture), StringComparison.Ordinal));
        using var response = await client.GetAsync(new Uri("/health", UriKind.Relative), Cancel);
        Assert.Equal(HttpStatusCode.BadRequest, response.StatusCode);
    }

    [Theory]
    [InlineData("127.0.0.1")]
    [InlineData("localhost")]
    [InlineData("LocalHost")]
    [InlineData("[::1]")]
    [InlineData("app-1.localhost")]
    public async Task Loopback_host_names_with_the_listening_port_are_accepted(string name)
    {
        using var client = Client(host: $"{name}:{_app.Port()}");
        using var response = await client.GetAsync(new Uri("/health", UriKind.Relative), Cancel);
        Assert.Equal(HttpStatusCode.OK, response.StatusCode);
    }

    [Fact]
    public void Subdomains_of_localhost_are_refused_unless_allowed()
    {
        Assert.False(LoopbackHost.IsLoopbackHost(new HostString("app-1.localhost", 5100), 5100, localhostSubdomains: false));
        Assert.True(LoopbackHost.IsLoopbackHost(new HostString("app-1.localhost", 5100), 5100, localhostSubdomains: true));
        Assert.False(LoopbackHost.IsLoopbackHost(new HostString(".localhost", 5100), 5100, localhostSubdomains: true));
    }

    [Fact]
    public void A_request_with_no_socket_under_it_is_judged_by_its_name_alone()
    {
        // An in-memory test server's connection has local port 0 and its client sends a name with no port.
        Assert.True(LoopbackHost.IsLoopbackHost(new HostString("localhost"), 0, localhostSubdomains: false));
        Assert.True(LoopbackHost.IsLoopbackHost(new HostString("127.0.0.1", 5100), 0, localhostSubdomains: false));
        Assert.False(LoopbackHost.IsLoopbackHost(new HostString("evil.example"), 0, localhostSubdomains: false));
    }
}

public sealed class LoopbackCredentialsTests
{
    [Fact]
    public void An_issued_credential_is_long_random_and_resolves_to_its_caller()
    {
        var credentials = new LoopbackCredentials();
        var first = credentials.Issue("owner");
        var second = credentials.Issue("plugin", "spell-check");
        Assert.NotEqual(first, second);
        Assert.True(first.Length >= LoopbackCredentials.MinimumLength);
        Assert.Equal(new LoopbackCaller("owner"), credentials.Resolve(first));
        Assert.Equal(new LoopbackCaller("plugin", "spell-check"), credentials.Resolve(second));
        Assert.Null(credentials.Resolve(first + "x"));
        Assert.Null(credentials.Resolve(null));
    }

    [Fact]
    public void A_short_or_repeated_credential_cannot_be_registered()
    {
        var credentials = new LoopbackCredentials();
        Assert.Throws<ArgumentException>(() => credentials.Register("short", "app"));
        credentials.Register(new string('a', 32), "app");
        Assert.Throws<InvalidOperationException>(() => credentials.Register(new string('a', 32), "other"));
        Assert.Throws<ArgumentException>(() => credentials.Register(new string('b', 32), ""));
    }
}

public sealed class DiscoveryFileTests : IDisposable
{
    private readonly string _folder = Path.Combine(Path.GetTempPath(), "tauri-kit-discovery-" + Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        if (Directory.Exists(_folder)) Directory.Delete(_folder, recursive: true);
    }

    private string FilePath => Path.Combine(_folder, "run", "backend.json");

    [Fact]
    public void A_published_backend_is_read_back_as_published()
    {
        var token = new LoopbackCredentials().Issue("owner");
        var published = DiscoveryFile.Publish(FilePath, 5100, token);

        Assert.True(DiscoveryFile.TryRead(FilePath, out var read, out _));
        Assert.Equal(published, read);
        Assert.Equal((5100, token, Environment.ProcessId), (read.Port, read.Token, read.Pid));
        // Only the file is left — the one written beside it was moved into place.
        Assert.Equal(["backend.json"], Directory.GetFiles(Path.GetDirectoryName(FilePath)!).Select(Path.GetFileName));
    }

    [Fact]
    public void The_file_has_the_shape_the_rust_side_reads()
    {
        var published = DiscoveryFile.Publish(FilePath, 5100, new string('a', 32));
        var json = System.Text.Json.Nodes.JsonNode.Parse(File.ReadAllText(FilePath))!.AsObject();
        Assert.Equal(["port", "token", "pid", "startedAt"], json.Select(p => p.Key));
        Assert.Equal(published.StartedAt.ToUnixTimeMilliseconds(), (long)json["startedAt"]!);
    }

    [Fact]
    public void Publishing_again_replaces_the_file_whole()
    {
        DiscoveryFile.Publish(FilePath, 5100, new string('a', 32));
        DiscoveryFile.Publish(FilePath, 5101, new string('b', 32));
        Assert.True(DiscoveryFile.TryRead(FilePath, out var read, out _));
        Assert.Equal((5101, new string('b', 32)), (read.Port, read.Token));
    }

    [Fact]
    public void Only_the_person_running_the_backend_can_read_the_file()
    {
        DiscoveryFile.Publish(FilePath, 5100, new string('a', 32));
        if (OperatingSystem.IsWindows()) AssertOwnerAndSystemOnly(FilePath);
        else Assert.Equal(UnixFileMode.UserRead | UnixFileMode.UserWrite, File.GetUnixFileMode(FilePath));
    }

    [System.Runtime.Versioning.SupportedOSPlatform("windows")]
    private static void AssertOwnerAndSystemOnly(string path)
    {
        var security = new FileInfo(path).GetAccessControl();
        Assert.True(security.AreAccessRulesProtected);
        var rules = security.GetAccessRules(includeExplicit: true, includeInherited: true, typeof(SecurityIdentifier)).Cast<FileSystemAccessRule>().ToList();
        using var identity = WindowsIdentity.GetCurrent();
        Assert.All(rules, r => Assert.Equal(AccessControlType.Allow, r.AccessControlType));
        Assert.Equal(
            new[] { identity.User!.Value, new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null).Value }.Order(),
            rules.Select(r => r.IdentityReference.Value).Distinct().Order());
    }

    [Fact]
    public void No_file_a_damaged_file_and_a_dead_process_are_told_apart()
    {
        Assert.False(DiscoveryFile.TryRead(FilePath, out _, out var missing));
        Assert.Equal(DiscoveryProblem.Missing, missing);

        Directory.CreateDirectory(Path.GetDirectoryName(FilePath)!);
        File.WriteAllText(FilePath, "{ not json");
        Assert.False(DiscoveryFile.TryRead(FilePath, out _, out var damaged));
        Assert.Equal(DiscoveryProblem.Unreadable, damaged);

        // This process's id with a start time it never had: a later process that reused the id.
        var published = DiscoveryFile.Publish(FilePath, 5100, new string('a', 32));
        File.WriteAllText(FilePath, $$"""{"port":5100,"token":"{{new string('a', 32)}}","pid":{{published.Pid}},"startedAt":978307200000}""");
        Assert.False(DiscoveryFile.TryRead(FilePath, out _, out var stale));
        Assert.Equal(DiscoveryProblem.Stale, stale);
    }

    [Fact]
    public void A_stopping_backend_removes_its_own_file_but_not_a_later_runs()
    {
        var first = DiscoveryFile.Publish(FilePath, 5100, new string('a', 32));
        var second = DiscoveryFile.Publish(FilePath, 5101, new string('b', 32));
        Assert.False(DiscoveryFile.RemoveIfStill(FilePath, first));
        Assert.True(File.Exists(FilePath));
        Assert.True(DiscoveryFile.RemoveIfStill(FilePath, second));
        Assert.False(File.Exists(FilePath));
    }

    [Fact]
    public async Task A_backend_run_with_publishing_is_found_and_called_and_leaves_no_file()
    {
        var credentials = new LoopbackCredentials();
        var owner = credentials.Issue("owner");
        var builder = LoopbackHost.CreateSlimBuilder([]);
        builder.Services.AddLoopbackAuthentication(credentials);
        var app = builder.Build();
        app.UseLoopbackHostCheck();
        app.MapGet("/ok", () => "fine");
        var running = app.RunPublishingAsync(FilePath, owner);

        LoopbackDiscovery? found = null;
        for (var i = 0; i < 100 && !DiscoveryFile.TryRead(FilePath, out found, out _); i++) await Task.Delay(50, TestContext.Current.CancellationToken);
        Assert.NotNull(found);
        using (var client = new HttpClient { BaseAddress = new Uri($"http://127.0.0.1:{found.Port}") })
        {
            client.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", found.Token);
            Assert.Equal("fine", await client.GetStringAsync(new Uri("/ok", UriKind.Relative), TestContext.Current.CancellationToken));
        }

        await app.StopAsync(TestContext.Current.CancellationToken);
        await running;
        await app.DisposeAsync();
        Assert.False(File.Exists(FilePath));
    }
}
