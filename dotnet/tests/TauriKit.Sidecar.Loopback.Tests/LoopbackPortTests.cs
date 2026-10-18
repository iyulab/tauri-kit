using System.Net;
using System.Net.Sockets;
using Microsoft.AspNetCore.Builder;

namespace TauriKit.Sidecar.Loopback.Tests;

public sealed class LoopbackPortTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("loopback-port-").FullName;
    private string PortFile => Path.Combine(_dir, "nested", "backend-port.json");

    public void Dispose() => Directory.Delete(_dir, recursive: true);

    private static WebApplication Build(int port) => LoopbackHost.CreateSlimBuilder([], port).Build();

    [Fact]
    public async Task A_first_start_takes_a_port_the_system_picks_and_remembers_it()
    {
        var started = await LoopbackPort.StartRememberedAsync(PortFile, Build, cancellationToken: TestContext.Current.CancellationToken);
        await using var app = started.App;
        Assert.Null(started.PreviousPort);
        Assert.Equal(app.Port(), LoopbackPort.Recall(PortFile));
    }

    [Fact]
    public async Task The_next_start_takes_the_same_port_again()
    {
        var first = await LoopbackPort.StartRememberedAsync(PortFile, Build, cancellationToken: TestContext.Current.CancellationToken);
        var port = first.App.Port();
        await first.App.DisposeAsync();

        var second = await LoopbackPort.StartRememberedAsync(PortFile, Build, cancellationToken: TestContext.Current.CancellationToken);
        await using var app = second.App;
        Assert.Equal(port, app.Port());
        Assert.Null(second.PreviousPort);
    }

    [Fact]
    public async Task A_port_that_stays_taken_moves_the_backend_and_says_where_from()
    {
        using var holder = new TcpListener(IPAddress.Loopback, 0);
        holder.Start();
        var taken = ((IPEndPoint)holder.LocalEndpoint).Port;
        LoopbackPort.Remember(PortFile, taken);
        var builds = 0;

        var started = await LoopbackPort.StartRememberedAsync(PortFile, port => { builds++; return Build(port); }, attempts: 3,
            retryDelay: TimeSpan.FromMilliseconds(10), cancellationToken: TestContext.Current.CancellationToken);
        await using var app = started.App;
        Assert.Equal(taken, started.PreviousPort);
        Assert.NotEqual(taken, app.Port());
        Assert.Equal(app.Port(), LoopbackPort.Recall(PortFile));
        Assert.Equal(4, builds); // three tries on the remembered port, then one on a picked port
    }

    [Fact]
    public async Task The_readiness_line_names_the_port_it_had_to_give_up()
    {
        using var holder = new TcpListener(IPAddress.Loopback, 0);
        holder.Start();
        var taken = ((IPEndPoint)holder.LocalEndpoint).Port;
        LoopbackPort.Remember(PortFile, taken);
        var started = await LoopbackPort.StartRememberedAsync(PortFile, Build, attempts: 1, cancellationToken: TestContext.Current.CancellationToken);
        await using var app = started.App;

        using var output = new StringWriter();
        var running = started.RunAnnouncingAsync("helper ready port=", output, [new("mode", "fresh")]);
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (output.ToString().Length == 0 && DateTime.UtcNow < deadline) await Task.Delay(20, TestContext.Current.CancellationToken);
        Assert.Equal($"helper ready port={app.Port()} previousPort={taken} mode=fresh", output.ToString().TrimEnd());
        await app.StopAsync(TestContext.Current.CancellationToken);
        await running;
    }

    [Theory]
    [InlineData("not json")]
    [InlineData("{ \"port\": 0 }")]
    [InlineData("{ \"port\": 70000 }")]
    [InlineData("[5100]")]
    public void A_file_that_cannot_be_used_is_no_remembered_port(string content)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(PortFile)!);
        File.WriteAllText(PortFile, content);
        Assert.Null(LoopbackPort.Recall(PortFile));
    }

    [Fact]
    public void A_missing_file_is_no_remembered_port_and_remembering_creates_its_folder()
    {
        Assert.Null(LoopbackPort.Recall(PortFile));
        LoopbackPort.Remember(PortFile, 5100);
        Assert.Equal(5100, LoopbackPort.Recall(PortFile));
        Assert.False(File.Exists(PortFile + ".tmp"));
    }
}
