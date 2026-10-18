namespace TauriKit.Diagnostics.Tests;

public sealed class SegmentTests : IDisposable
{
    private readonly string _dir = Directory.CreateTempSubdirectory("diagnostics-segments-").FullName;

    public void Dispose() => Directory.Delete(_dir, recursive: true);

    [Fact]
    public void Each_segment_is_a_file_of_its_own_named_as_the_crate_names_it()
    {
        var stream = Path.Combine(_dir, "events");
        using var a = Segment.Open(stream);
        using var b = Segment.Open(stream);
        Assert.NotEqual(a.File, b.File);
        Assert.Matches("^[0-9a-f]{32}\\.jsonl$", Path.GetFileName(a.File));
        Assert.True(File.Exists(Path.ChangeExtension(a.File, ".lock")));
        Assert.False(File.Exists(a.File), "made when first written");

        var registry = Registry.FromJson(File.ReadAllText(Path.Combine(EventsTests.Cases, "registry.json")));
        var events = new Events(registry, "1.2.3", a.File);
        Assert.Equal(Recorded.Written, events.Record(registry.Event("app.opened")));
        Assert.Single(File.ReadAllLines(a.File));
    }

    [Fact]
    public void Its_lock_is_held_while_it_is_open_and_let_go_when_it_ends()
    {
        var segment = Segment.Open(Path.Combine(_dir, "reports"));
        var lockFile = Path.ChangeExtension(segment.File, ".lock");
        Assert.Throws<IOException>(() => new FileStream(lockFile, FileMode.Open, FileAccess.ReadWrite, FileShare.None).Dispose());

        segment.Dispose();
        using var taken = new FileStream(lockFile, FileMode.Open, FileAccess.ReadWrite, FileShare.None);
    }

    [Fact]
    public void A_live_segment_can_still_be_read_by_a_sender()
    {
        using var segment = Segment.Open(Path.Combine(_dir, "reports"));
        var reporter = new Reporter(segment.File);
        Assert.True(reporter.Record(new Layer("host").Report("System.TimeoutException", "", "1.2.3")));
        using var read = new FileStream(segment.File, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        Assert.True(read.Length > 0);
    }
}
