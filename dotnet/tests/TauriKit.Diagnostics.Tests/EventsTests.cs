using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace TauriKit.Diagnostics.Tests;

public sealed partial class EventsTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("diagnostics-events-").FullName;

    public void Dispose() => Directory.Delete(_dir, recursive: true);

    /// <summary>The crate's test cases: the registry both languages read, and the lines both must write.</summary>
    internal static string Cases
    {
        get
        {
            var dir = new DirectoryInfo(AppContext.BaseDirectory);
            while (dir is not null && !File.Exists(Path.Combine(dir.FullName, "Cargo.toml"))) dir = dir.Parent;
            return Path.Combine(dir!.FullName, "crates", "diagnostics", "tests", "cases");
        }
    }

    private static Registry Shared() => Registry.FromJson(File.ReadAllText(Path.Combine(Cases, "registry.json")));

    /// <summary>
    /// The scene both languages write (the crate's <c>tests/dotnet_lines.rs</c> writes the same): three events, a value
    /// not of its kind, an undeclared field, and an event past its cap.
    /// </summary>
    private static void Scene(Events events, Registry registry)
    {
        var export = registry.Event("export.finished");
        Assert.Equal(Recorded.Written, events.Record(export, ("format", Value.Choice("csv")), ("rows", Value.Count(437)), ("took", Value.Duration(TimeSpan.FromMilliseconds(1250)))));
        Assert.Equal(Recorded.Written, events.Record(registry.Event("search.used"), ("filters", Value.Subset("size", "date")), ("results", Value.Count(7)), ("empty", Value.Bool(false))));
        Assert.Equal(Recorded.DroppedBadValue, events.Record(export, ("format", Value.Choice("C:/Users/someone/salaries.xlsx")), ("rows", Value.Count(1)), ("took", Value.Duration(TimeSpan.Zero))));
        Assert.Equal(Recorded.DroppedUnknownField, events.Record(export, ("format", Value.Choice("csv")), ("rows", Value.Count(1)), ("took", Value.Duration(TimeSpan.Zero)), ("filename", Value.Choice("salaries"))));
        var opened = registry.Event("app.opened");
        Assert.Equal(Recorded.Written, events.Record(opened));
        Assert.Equal(Recorded.Capped, events.Record(opened));
    }

    /// <summary>The written lines with what differs from run to run — the random id, the time, the platform — made fixed.</summary>
    private static string Normalized(string file) => string.Concat(File.ReadAllLines(file).Select(line =>
    {
        var node = JsonNode.Parse(line)!.AsObject();
        node["session"] = "00000000000000000000000000000000";
        node["time"] = "2026-01-01T00:00:00Z";
        node["os"] = "windows";
        node["arch"] = "x86_64";
        return node.ToJsonString() + "\n";
    }));

    [Fact]
    public void The_lines_are_the_ones_the_crate_writes_for_the_same_scene()
    {
        var registry = Shared();
        var events = new Events(registry, "1.2.3", Path.Combine(_dir, "events.jsonl"), maxPerEvent: 1);
        Scene(events, registry);
        var written = Normalized(events.File);
        var expected = Path.Combine(Cases, "dotnet-events.jsonl");
        if (Environment.GetEnvironmentVariable("TAURI_KIT_REGENERATE") == "1") File.WriteAllText(expected, written);
        Assert.Equal(File.ReadAllText(expected).ReplaceLineEndings("\n"), written);
        Assert.DoesNotContain("salaries", written, StringComparison.Ordinal);
        Assert.DoesNotContain("filename", written, StringComparison.Ordinal);
    }

    [Fact]
    public void Each_line_carries_a_random_id_of_its_own_and_the_platform_as_the_crate_names_it()
    {
        var registry = Shared();
        var a = new Events(registry, "1.2.3", Path.Combine(_dir, "a.jsonl"));
        var b = new Events(registry, "1.2.3", Path.Combine(_dir, "b.jsonl"));
        Assert.Matches(Hex32(), a.Id);
        Assert.NotEqual(a.Id, b.Id);
        Assert.Equal(Recorded.Written, a.Record(registry.Event("app.opened")));
        var line = JsonNode.Parse(File.ReadAllLines(a.File)[0])!.AsObject();
        Assert.Equal(a.Id, line["session"]!.GetValue<string>());
        Assert.Matches(Time(), line["time"]!.GetValue<string>());
        Assert.Matches("^(windows|macos|linux)$", line["os"]!.GetValue<string>());
        Assert.Null(line["install"]);
    }

    [Theory]
    [InlineData("""{"events":{"x":{"fields":{"size":{"kind":"count"}}}}}""", "bucket or a max")]
    [InlineData("""{"events":{"x":{"fields":{"size":{"kind":"count","max":1001}}}}}""", "max must be")]
    [InlineData("""{"events":{"x":{"fields":{"size":{"kind":"count","bucket":[0,482113906,482113907]}}}}}""", "twice")]
    [InlineData("""{"events":{"x":{"fields":{"who":{"kind":"choice","values":["a b"]}}}}}""", "plain identifiers")]
    [InlineData("""{"events":{"x":{"fields":{"name":{"kind":"text"}}}}}""", "unknown kind")]
    [InlineData("""{"events":{"kit.mine":{}}}""", "crate's own")]
    [InlineData("""{"events":{"Export":{}}}""", "plain lowercase")]
    [InlineData("""{"events":{"x":{},"x":{}}}""", "given twice")]
    [InlineData("""{"events":{"x":{"fields":{"t":{"kind":"duration","max_ms":999999999999}}}}}""", "one day")]
    public void A_registry_that_could_carry_content_is_refused_as_the_crate_refuses_it(string json, string why)
    {
        var e = Assert.Throws<RegistryException>(() => Registry.FromJson(json));
        Assert.Contains(why, e.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void A_misspelled_event_fails_at_start()
    {
        Assert.Throws<RegistryException>(() => Shared().Event("export.finshed"));
    }

    [GeneratedRegex("^[0-9a-f]{32}$")]
    private static partial Regex Hex32();

    [GeneratedRegex(@"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")]
    private static partial Regex Time();
}
