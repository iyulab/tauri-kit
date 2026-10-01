using System.Net;
using System.Net.Http.Headers;
using System.Reflection;
using System.Text.Json.Nodes;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;

namespace TauriKit.Sidecar.Loopback.Tests;

public sealed class LoopbackHostTests : IAsyncLifetime
{
    private const string Token = "0123456789abcdef0123456789abcdef-test";
    private WebApplication _app = null!;
    private HttpClient _http = null!;

    public async ValueTask InitializeAsync()
    {
        _app = LoopbackHost.CreateSlimBuilder([]).Build();
        _app.UseBearerToken(Token);
        _app.UseFaults(new FaultOptions
        {
            OwnNamespaces = ["TauriKit.Sidecar.Loopback.Tests"],
            StatusFor = e => e is KeyNotFoundException ? StatusCodes.Status409Conflict : null,
        });
        _app.MapGet("/ok", () => "fine");
        _app.MapGet("/boom", string () => Thrower.Throw("the record of someone private"));
        _app.MapGet("/expected", string () => throw new KeyNotFoundException("nothing loaded"));
        await _app.StartAsync();
        _http = new HttpClient { BaseAddress = new Uri(_app.Urls.First()) };
        _http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", Token);
    }

    public async ValueTask DisposeAsync()
    {
        _http.Dispose();
        await _app.DisposeAsync();
    }

    [Fact]
    public void Listens_on_the_loopback_address_only_on_a_port_the_system_picked()
    {
        Assert.Equal("127.0.0.1", new Uri(_app.Urls.First()).Host);
        Assert.NotEqual(0, _app.Port());
    }

    [Fact]
    public async Task Answers_a_request_carrying_the_token()
    {
        using var response = await _http.GetAsync(new Uri("/ok", UriKind.Relative), TestContext.Current.CancellationToken);
        Assert.Equal(HttpStatusCode.OK, response.StatusCode);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("Bearer wrong")]
    [InlineData(Token)]
    [InlineData("Bearer " + Token + "x")]
    public async Task Refuses_a_request_without_the_token(string? authorization)
    {
        using var client = new HttpClient { BaseAddress = _http.BaseAddress };
        if (authorization is not null) client.DefaultRequestHeaders.TryAddWithoutValidation("Authorization", authorization);
        using var response = await client.GetAsync(new Uri("/ok", UriKind.Relative), TestContext.Current.CancellationToken);
        Assert.Equal(HttpStatusCode.Unauthorized, response.StatusCode);
    }

    [Fact]
    public async Task An_unexpected_failure_says_its_type_and_where_but_not_its_message()
    {
        using var response = await _http.GetAsync(new Uri("/boom", UriKind.Relative), TestContext.Current.CancellationToken);
        Assert.Equal(HttpStatusCode.InternalServerError, response.StatusCode);
        var text = await response.Content.ReadAsStringAsync(TestContext.Current.CancellationToken);
        Assert.DoesNotContain("someone private", text, StringComparison.Ordinal);
        var fault = JsonNode.Parse(text)!["fault"]!;
        Assert.Equal("System.InvalidOperationException", (string?)fault["type"]);
        Assert.Equal("TauriKit.Sidecar.Loopback.Tests.Thrower.Throw", (string?)fault["at"]);
        var frames = fault["frames"]!.AsArray().Select(f => (string?)f).ToList();
        Assert.Equal("TauriKit.Sidecar.Loopback.Tests.Thrower.Throw", frames[0]);
        Assert.All(frames, f => Assert.StartsWith("TauriKit.Sidecar.Loopback.Tests", f, StringComparison.Ordinal));
    }

    [Fact]
    public async Task An_expected_failure_gets_the_status_the_app_gives_it()
    {
        using var response = await _http.GetAsync(new Uri("/expected", UriKind.Relative), TestContext.Current.CancellationToken);
        Assert.Equal(HttpStatusCode.Conflict, response.StatusCode);
        Assert.Empty(await response.Content.ReadAsStringAsync(TestContext.Current.CancellationToken));
    }
}

internal static class Thrower
{
    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    public static string Throw(string about) => throw new InvalidOperationException(about);
}

public sealed class LoopbackHostStaticTests
{
    [Fact]
    public void A_missing_or_short_token_is_refused()
    {
        var name = "TAURI_KIT_TEST_TOKEN_" + Guid.NewGuid().ToString("N");
        Assert.Null(LoopbackHost.ReadToken(name));
        Environment.SetEnvironmentVariable(name, "short");
        Assert.Null(LoopbackHost.ReadToken(name));
        Environment.SetEnvironmentVariable(name, new string('a', 32));
        Assert.Equal(new string('a', 32), LoopbackHost.ReadToken(name));
        Environment.SetEnvironmentVariable(name, null);
    }

    [Fact]
    public void The_readiness_line_is_the_prefix_then_the_port()
    {
        Assert.Equal("helper ready port=51234", LoopbackHost.ReadyLine("helper ready port=", 51234));
    }

    [Fact]
    public async Task The_readiness_line_is_written_once_the_app_listens()
    {
        var app = LoopbackHost.CreateSlimBuilder([]).Build();
        using var output = new StringWriter();
        var running = app.RunAnnouncingAsync("helper ready port=", output);
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (output.ToString().Length == 0 && DateTime.UtcNow < deadline) await Task.Delay(20, TestContext.Current.CancellationToken);
        Assert.Equal(LoopbackHost.ReadyLine("helper ready port=", app.Port()), output.ToString().TrimEnd());
        await app.StopAsync(TestContext.Current.CancellationToken);
        await running;
        await app.DisposeAsync();
    }

    [Fact]
    public void The_own_frame_is_the_innermost_one_in_the_named_namespaces()
    {
        const string trace = """
               at System.Linq.Enumerable.First[TSource](IEnumerable`1 source)
               at MyApp.Core.Ledger.Post(Entry entry) in C:\src\Ledger.cs:line 10
               at MyApp.Host.Api.Handle(HttpContext context)
            """;
        Assert.Equal("MyApp.Core.Ledger.Post", Fault.OwnFrame(trace, ["MyApp."]));
        Assert.Equal("MyApp.Host.Api.Handle", Fault.OwnFrame(trace, ["MyApp.Host."]));
        Assert.Null(Fault.OwnFrame(trace, ["Other."]));
        Assert.Null(Fault.OwnFrame(null, ["MyApp."]));
    }

    [Fact]
    public void The_own_frames_are_the_path_through_the_app_innermost_first()
    {
        const string trace = """
               at System.Linq.Enumerable.First[TSource](IEnumerable`1 source)
               at MyApp.Core.Ledger.Post(Entry entry) in C:\src\Ledger.cs:line 10
               at MyApp.Core.Ledger.Post(Entry entry) in C:\src\Ledger.cs:line 14
               at System.Runtime.CompilerServices.TaskAwaiter.ThrowForNonSuccess(Task task)
            --- End of stack trace from previous location ---
               at MyApp.Host.Api.Handle(HttpContext context)
               at Microsoft.AspNetCore.Routing.EndpointMiddleware.Invoke(HttpContext httpContext)
            """;
        Assert.Equal(["MyApp.Core.Ledger.Post", "MyApp.Host.Api.Handle"], Fault.OwnFrames(trace, ["MyApp."]));
        Assert.Equal(["MyApp.Core.Ledger.Post"], Fault.OwnFrames(trace, ["MyApp."], maxFrames: 1));
        Assert.Empty(Fault.OwnFrames(trace, ["MyApp."], maxFrames: 0));
        Assert.Empty(Fault.OwnFrames(null, ["MyApp."]));
        Assert.Throws<ArgumentOutOfRangeException>(() => Fault.OwnFrames(trace, ["MyApp."], maxFrames: -1));
    }

    [Fact]
    public void A_fault_off_any_request_reads_the_same_way()
    {
        try
        {
            Thrower.Throw("someone private");
        }
        catch (InvalidOperationException e)
        {
            var fault = Fault.Of(e, ["TauriKit.Sidecar.Loopback.Tests"]);
            Assert.Equal("System.InvalidOperationException", fault.Type);
            Assert.Equal(fault.Frames[0], fault.At);
            Assert.Contains("TauriKit.Sidecar.Loopback.Tests.LoopbackHostStaticTests.A_fault_off_any_request_reads_the_same_way", fault.Frames);
            Assert.Null(Fault.Of(e, ["Other."]).At);
        }
    }

    [Fact]
    public void The_package_has_the_crates_version()
    {
        // The Rust and .NET halves of the arrangement are released together, under one version.
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !File.Exists(Path.Combine(dir.FullName, "Cargo.toml"))) dir = dir.Parent;
        Assert.NotNull(dir);
        var cargo = File.ReadAllLines(Path.Combine(dir.FullName, "Cargo.toml"));
        var workspaceVersion = cargo.SkipWhile(l => l.Trim() != "[workspace.package]").First(l => l.TrimStart().StartsWith("version", StringComparison.Ordinal)).Split('"')[1];
        var packageVersion = typeof(LoopbackHost).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()!.InformationalVersion.Split('+')[0];
        Assert.Equal(workspaceVersion, packageVersion);
    }
}
