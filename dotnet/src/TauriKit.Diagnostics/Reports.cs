using System.Globalization;
using System.Text.Encodings.Web;
using System.Text.Json;

namespace TauriKit.Diagnostics;

/// <summary>
/// One error report, in the shape the tauri-kit-diagnostics crate writes, reads and sends (its <c>Report</c>). Every field
/// comes from an allowlist: the layer is the app's own name, the kind a plain identifier or <see cref="Report.UnrecognizedKind"/>,
/// the frames .NET method names of the stack (never file names — in a .NET stack they are paths on the machine that built
/// it), the details only what the registry declares for the layer, and the rest the app's version, the platform and the time.
/// </summary>
public sealed record Report
{
    /// <summary>The kind a report gets when what it was handed is not a plain identifier.</summary>
    public const string UnrecognizedKind = "Unrecognized";

    /// <summary>Enough of a stack to tell one failure from another: the frames a report keeps.</summary>
    public const int MaxFrames = 20;

    /// <summary>The most details one report carries.</summary>
    public const int MaxDetails = 8;

    internal Report(string layer, string kind, IReadOnlyList<string> frames, string version, IReadOnlyDictionary<string, string> details, string? time = null)
    {
        Layer = layer;
        Kind = kind;
        Frames = frames;
        Version = version;
        Os = Events.Os;
        Arch = Events.Arch;
        Time = time ?? DateTime.UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'", CultureInfo.InvariantCulture);
        Details = details;
    }

    /// <summary>The name of the layer that failed.</summary>
    public string Layer { get; }

    /// <summary>What failed: a type name, or a code of the app's own.</summary>
    public string Kind { get; }

    /// <summary>The method names of the stack that led to the failure, innermost first.</summary>
    public IReadOnlyList<string> Frames { get; }

    /// <summary>The app's version, as the app passed it.</summary>
    public string Version { get; }

    /// <summary>The operating system, as Rust's <c>std::env::consts::OS</c> names it.</summary>
    public string Os { get; }

    /// <summary>The CPU architecture, as Rust's <c>std::env::consts::ARCH</c> names it.</summary>
    public string Arch { get; }

    /// <summary>When it failed, in UTC to the second (ISO 8601) — reports are often sent on a later launch.</summary>
    public string Time { get; }

    /// <summary>More of what failed, under the names the registry declares for the layer.</summary>
    public IReadOnlyDictionary<string, string> Details { get; }

    /// <summary>A copy with only <paramref name="kind"/>: no frames, no details.</summary>
    internal Report OfKind(string kind) => new(Layer, kind, [], Version, NoDetails, Time);

    /// <summary>One line, in the crate's field order: layer, kind, frames, version, os, arch, time, details (when any).</summary>
    internal byte[] Line()
    {
        using var buffer = new MemoryStream();
        // The crate writes `<`, `>`, `+` and `` ` `` of method names as they are; the default encoder would escape them.
        using (var json = new Utf8JsonWriter(buffer, new JsonWriterOptions { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping }))
        {
            json.WriteStartObject();
            json.WriteString("layer", Layer);
            json.WriteString("kind", Kind);
            json.WriteStartArray("frames");
            foreach (var frame in Frames) json.WriteStringValue(frame);
            json.WriteEndArray();
            json.WriteString("version", Version);
            json.WriteString("os", Os);
            json.WriteString("arch", Arch);
            json.WriteString("time", Time);
            if (Details.Count > 0)
            {
                json.WriteStartObject("details");
                foreach (var (name, value) in Details.OrderBy(d => d.Key, StringComparer.Ordinal)) json.WriteString(name, value);
                json.WriteEndObject();
            }

            json.WriteEndObject();
        }

        buffer.WriteByte((byte)'\n');
        return buffer.ToArray();
    }

    private static readonly IReadOnlyDictionary<string, string> NoDetails = new Dictionary<string, string>();
}

/// <summary>
/// A part of the app that fails on its own — for the .NET side, the backend process — and the details its reports may
/// carry. Make it with <see cref="Registry.Layer(string)"/>, or with <see cref="Layer(string)"/> for a layer whose reports carry
/// no details. The name is written into every report as it is given, so it should be a fixed name of the app's own,
/// such as <c>"host"</c>.
/// </summary>
public sealed class Layer
{
    internal static readonly IReadOnlyDictionary<string, FieldKind> NoDetails = new Dictionary<string, FieldKind>();

    private readonly IReadOnlyDictionary<string, FieldKind> _details;

    /// <summary>A layer named <paramref name="name"/> whose reports carry no details.</summary>
    public Layer(string name)
        : this(name, NoDetails)
    {
    }

    internal Layer(string name, IReadOnlyDictionary<string, FieldKind> details)
    {
        ArgumentException.ThrowIfNullOrEmpty(name);
        Name = name;
        _details = details;
    }

    /// <summary>The name reports of this layer carry.</summary>
    public string Name { get; }

    /// <summary>
    /// A report of <paramref name="exception"/>: its type's full name as the kind, the method names of its stack as the
    /// frames, as the other overload takes them.
    /// </summary>
    public Report Report(Exception exception, string version, params (string Name, string Value)[] details)
    {
        ArgumentNullException.ThrowIfNull(exception);
        return Report(exception.GetType().FullName ?? Diagnostics.Report.UnrecognizedKind, exception.StackTrace ?? "", version, details);
    }

    /// <summary>
    /// A report of this layer. <paramref name="kind"/> and <paramref name="stack"/> are untrusted: a kind that is not a plain
    /// identifier (an ASCII letter, then letters, digits, <c>_</c>, <c>.</c>, <c>:</c> or <c>-</c>, at most 100 characters)
    /// becomes <see cref="Report.UnrecognizedKind"/> — whole, never in part — and of each line of the stack only the method
    /// name (<c>Namespace.Type.Method</c>, the arguments and the file dropped) is kept, when it is only an identifier path,
    /// at most <see cref="Report.MaxFrames"/>. Each detail is kept only when the registry declares its name for this layer
    /// and its value is of that kind, written as the kind writes it (a count on its bucket); anything else is left out
    /// whole. A name given twice keeps its first.
    /// </summary>
    public Report Report(string kind, string stack, string version, params (string Name, string Value)[] details)
    {
        ArgumentNullException.ThrowIfNull(kind);
        ArgumentNullException.ThrowIfNull(stack);
        ArgumentException.ThrowIfNullOrEmpty(version);
        var kept = new SortedDictionary<string, string>(StringComparer.Ordinal);
        foreach (var (name, value) in details)
        {
            if (kept.ContainsKey(name) || kept.Count >= Diagnostics.Report.MaxDetails) continue;
            if (_details.TryGetValue(name, out var declared) && Registry.DetailText(declared, value ?? "") is { } text) kept[name] = text;
        }

        var frames = stack.Split('\n').Select(Frame).OfType<string>().Take(Diagnostics.Report.MaxFrames).ToList();
        return new Report(Name, PlainKind(kind), frames, version, kept);
    }

    /// <summary>The kind whole, or <see cref="Report.UnrecognizedKind"/>.</summary>
    internal static string PlainKind(string raw) =>
        raw.Length is >= 1 and <= 100 && char.IsAsciiLetter(raw[0]) && raw.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '.' or ':' or '-')
            ? raw
            : Diagnostics.Report.UnrecognizedKind;

    /// <summary>
    /// The method name of one stack line — <c>Type.Method</c> as listed, or <c>at Type.Method(args) in file:line N</c> as
    /// .NET prints it — the crate's <c>FrameRule::DotNetMethod</c>, line for line.
    /// </summary>
    internal static string? Frame(string raw)
    {
        var line = raw.Trim();
        if (line.StartsWith("at ", StringComparison.Ordinal)) line = line[3..];
        if (line.StartsWith("async ", StringComparison.Ordinal)) line = line[6..];
        string function;
        var paren = line.LastIndexOf(" (", StringComparison.Ordinal);
        if (paren >= 0 && line.EndsWith(')')) function = line[..paren];
        else if (line.IndexOf(" in ", StringComparison.Ordinal) is var at and >= 0) function = line[..at];
        else function = line;
        var name = function.Split('(')[0];
        return name.Length is >= 1 and <= 200 && name.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '.' or '$' or '<' or '>' or '+' or '`')
            ? name
            : null;
    }
}

/// <summary>
/// The reports of one run of a .NET backend, appended to a JSON Lines file of its own in the crate's shape — so the app's
/// shell reads and sends them (<c>pending</c>) as it does its own. Each failure — the same layer, kind and first frame —
/// is written once, and at most <see cref="DefaultMaxReports"/>; past that, one last report of kind
/// <see cref="CappedKind"/> says the rest were left out. One writer per file: give each process its own.
/// </summary>
public sealed class Reporter
{
    /// <summary>A run that fails this often is failing the same way; more reports would say nothing new.</summary>
    public const int DefaultMaxReports = 50;

    /// <summary>The kind of the one last report once the most have been written.</summary>
    public const string CappedKind = "ReportsCapped";

    private readonly Lock _lock = new();
    private readonly HashSet<(string, string, string?)> _failures = [];
    private int _count;

    /// <summary>A reporter that appends to <paramref name="file"/>, creating it and its folders when it first writes.</summary>
    public Reporter(string file, int maxReports = DefaultMaxReports)
    {
        ArgumentException.ThrowIfNullOrEmpty(file);
        File = file;
        MaxReports = maxReports;
    }

    /// <summary>The file reports are appended to.</summary>
    public string File { get; }

    /// <summary>How many reports it writes at most, before the one that says the rest were left out.</summary>
    public int MaxReports { get; }

    /// <summary>
    /// Writes <paramref name="report"/> unless this failure was written already, or the most have been. Returns whether a
    /// report was written. Exceptions are only the file's.
    /// </summary>
    public bool Record(Report report)
    {
        ArgumentNullException.ThrowIfNull(report);
        lock (_lock)
        {
            if (_count > MaxReports) return false;
            if (_count == MaxReports)
            {
                report = report.OfKind(CappedKind);
            }
            else if (!_failures.Add((report.Layer, report.Kind, report.Frames.Count > 0 ? report.Frames[0] : null)))
            {
                return false;
            }

            var line = report.Line();
            var folder = Path.GetDirectoryName(Path.GetFullPath(File));
            if (folder is not null) Directory.CreateDirectory(folder);
            using (var stream = new FileStream(File, FileMode.Append, FileAccess.Write, FileShare.Read)) stream.Write(line);
            _count++;
            return true;
        }
    }
}
