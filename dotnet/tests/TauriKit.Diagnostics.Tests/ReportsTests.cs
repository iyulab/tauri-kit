using System.Text.Json.Nodes;

namespace TauriKit.Diagnostics.Tests;

public sealed class ReportsTests : IDisposable
{
    /// <summary>A .NET stack as an exception prints it: a frame with its source file, a lambda, a framework frame, a generic method.</summary>
    internal const string Stack =
        "   at Contoso.Export.Writer.Flush(Stream s) in C:\\Users\\builder\\src\\Writer.cs:line 42\n" +
        "   at Contoso.Export.Job.<Run>b__3_0()\n" +
        "   at System.Threading.Tasks.Task.Execute()\n" +
        "   at Contoso.Export.Generic`1.Run[T](T item)\n";

    private readonly string _dir = Directory.CreateTempSubdirectory("diagnostics-reports-").FullName;

    public void Dispose() => Directory.Delete(_dir, recursive: true);

    private static Registry Shared() => Registry.FromJson(File.ReadAllText(Path.Combine(EventsTests.Cases, "registry.json")));

    /// <summary>
    /// The scene both languages write (the crate's <c>tests/dotnet_lines.rs</c> writes the same): a report with declared and
    /// undeclared details, the same failure again, a kind that is not a plain identifier, and one past the cap of two.
    /// </summary>
    private static void Scene(Reporter reporter, Layer host)
    {
        Assert.True(reporter.Record(host.Report("System.IO.IOException", Stack, "1.2.3",
            ("status", "timeout"), ("attempt", "3"), ("path", "C:/Users/someone/salaries.xlsx"), ("status", "refused"))));
        Assert.False(reporter.Record(host.Report("System.IO.IOException", Stack, "1.2.3")));
        Assert.True(reporter.Record(host.Report("C:/Users/someone/salaries.xlsx", "", "1.2.3", ("status", "C:/Users/someone/"))));
        Assert.True(reporter.Record(host.Report("System.TimeoutException", "", "1.2.3")));
        Assert.False(reporter.Record(host.Report("System.InvalidOperationException", "", "1.2.3")));
    }

    /// <summary>The written lines with what differs from run to run — the time, the platform — made fixed.</summary>
    private static string Normalized(string file) => string.Concat(File.ReadAllLines(file).Select(line =>
    {
        var node = JsonNode.Parse(line)!.AsObject();
        node["time"] = "2026-01-01T00:00:00Z";
        node["os"] = "windows";
        node["arch"] = "x86_64";
        return node.ToJsonString(new System.Text.Json.JsonSerializerOptions { Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping }) + "\n";
    }));

    [Fact]
    public void The_lines_are_the_ones_the_crate_writes_for_the_same_scene()
    {
        var reporter = new Reporter(Path.Combine(_dir, "reports.jsonl"), maxReports: 2);
        Scene(reporter, Shared().Layer("host"));
        var written = Normalized(reporter.File);
        var expected = Path.Combine(EventsTests.Cases, "dotnet-reports.jsonl");
        if (Environment.GetEnvironmentVariable("TAURI_KIT_REGENERATE") == "1") File.WriteAllText(expected, written);
        Assert.Equal(File.ReadAllText(expected).ReplaceLineEndings("\n"), written);
        Assert.DoesNotContain("salaries", written, StringComparison.Ordinal);
        Assert.DoesNotContain("someone", written, StringComparison.Ordinal);
        Assert.DoesNotContain("builder", written, StringComparison.Ordinal);
        Assert.DoesNotContain("path", written, StringComparison.Ordinal);
    }

    [Fact]
    public void An_exception_reports_its_type_and_the_method_names_of_its_stack()
    {
        Exception caught;
        try
        {
            Throw();
            return;
        }
        catch (InvalidOperationException e)
        {
            caught = e;
        }

        var report = new Layer("host").Report(caught, "1.2.3", ("status", "timeout"));
        Assert.Equal("System.InvalidOperationException", report.Kind);
        Assert.Contains(report.Frames, f => f.EndsWith(".Throw", StringComparison.Ordinal));
        Assert.All(report.Frames, f => Assert.DoesNotContain(":", f, StringComparison.Ordinal));
        Assert.Empty(report.Details); // a layer made without the registry declares none
    }

    [Theory]
    [InlineData("count", "07", "7")]
    [InlineData("count", "-1", null)]
    [InlineData("count", "1e3", null)]
    [InlineData("bool", "True", null)]
    [InlineData("duration", "86400001", null)]
    public void A_detail_is_kept_only_when_it_is_of_its_kind(string kind, string raw, string? kept)
    {
        var json = kind == "count"
            ? """{"reports":{"host":{"d":{"kind":"count","max":10}}}}"""
            : """{"reports":{"host":{"d":{"kind":"KIND"}}}}""".Replace("KIND", kind, StringComparison.Ordinal);
        var report = Registry.FromJson(json).Layer("host").Report("Failed", "", "1.2.3", ("d", raw));
        Assert.Equal(kept, report.Details.GetValueOrDefault("d"));
    }

    [Theory]
    [InlineData("""{"reports":{"host":{"Path":{"kind":"bool"}}}}""", "plain lowercase detail name")]
    [InlineData("""{"reports":{"host":{"name":{"kind":"text"}}}}""", "unknown kind")]
    [InlineData("""{"reports":{"host":{"a":{"kind":"bool"},"a":{"kind":"bool"}}}}""", "given twice")]
    [InlineData("""{"reports":{"host":{"a":{"kind":"bool"},"b":{"kind":"bool"},"c":{"kind":"bool"},"d":{"kind":"bool"},"e":{"kind":"bool"},"f":{"kind":"bool"},"g":{"kind":"bool"},"h":{"kind":"bool"},"i":{"kind":"bool"}}}}""", "more than 8 details")]
    public void A_reports_section_that_could_carry_content_is_refused_as_the_crate_refuses_it(string json, string why)
    {
        var e = Assert.Throws<RegistryException>(() => Registry.FromJson(json));
        Assert.Contains(why, e.Message, StringComparison.Ordinal);
    }

    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    private static void Throw() => throw new InvalidOperationException("C:/Users/someone/salaries.xlsx");
}
