using System.Globalization;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace TauriKit.Diagnostics;

/// <summary>What <see cref="Events.Record"/> did.</summary>
public enum Recorded
{
    /// <summary>The event was written.</summary>
    Written,

    /// <summary>An undeclared field — the event was not written; the drop notice was (once).</summary>
    DroppedUnknownField,

    /// <summary>A value not of its kind, a declared field left out or given twice — the event was not written; the drop notice was (once).</summary>
    DroppedBadValue,

    /// <summary>An <see cref="EventName"/> from another registry — written nowhere.</summary>
    DroppedUnknownEvent,

    /// <summary>The event was written its most for this <see cref="Events"/>; the notice says so once.</summary>
    Capped,
}

/// <summary>
/// Events of one run of a .NET backend, appended to a JSON Lines file of its own in the shape the
/// tauri-kit-diagnostics crate writes and reads — so the app's shell verifies them against the same registry and
/// sends them (<c>pending_events</c>). Each line carries a random id of these events' own (never the installation's),
/// the app's version, the platform and the time; the fields are only what the registry declares, each of its kind.
/// What does not fit is dropped and said once with <c>kit.events.dropped</c>, without the undeclared name or value.
/// One writer per file: give each process its own <see cref="Segment"/>.
/// </summary>
public sealed class Events
{
    /// <summary>The event written when another is dropped.</summary>
    public const string DroppedEvent = "kit.events.dropped";

    /// <summary>How many times one event is written by default.</summary>
    public const int DefaultMaxPerEvent = 100;

    private readonly Registry _registry;
    private readonly string _version;
    private readonly Lock _lock = new();
    private readonly Dictionary<string, int> _written = new(StringComparer.Ordinal);
    private readonly HashSet<(Recorded, string, string?)> _told = [];
    private readonly HashSet<string> _capped = new(StringComparer.Ordinal);

    /// <summary>Events declared by <paramref name="registry"/>, of the app at <paramref name="version"/>, appended to <paramref name="file"/>.</summary>
    public Events(Registry registry, string version, string file, int maxPerEvent = DefaultMaxPerEvent)
    {
        ArgumentNullException.ThrowIfNull(registry);
        ArgumentException.ThrowIfNullOrEmpty(version);
        ArgumentException.ThrowIfNullOrEmpty(file);
        if (version.Length > 64 || !version.All(c => char.IsAsciiLetterOrDigit(c) || c is '.' or '-' or '+' or '_'))
            throw new ArgumentException("A version is ASCII letters, digits, '.', '-', '+' and '_', at most 64.", nameof(version));
        _registry = registry;
        _version = version;
        File = file;
        MaxPerEvent = maxPerEvent;
        Id = Convert.ToHexStringLower(RandomNumberGenerator.GetBytes(16));
    }

    /// <summary>The file it appends to.</summary>
    public string File { get; }

    /// <summary>The random id each line carries.</summary>
    public string Id { get; }

    /// <summary>How many times each event is written at most.</summary>
    public int MaxPerEvent { get; }

    /// <summary>
    /// Writes <paramref name="name"/> with <paramref name="fields"/> — every declared field, each of its kind — or drops
    /// it and says so. All or nothing. Exceptions are only the file's.
    /// </summary>
    public Recorded Record(EventName name, params (string Field, Value Value)[] fields)
    {
        ArgumentNullException.ThrowIfNull(name);
        if (!_registry.Events.TryGetValue(name.Name, out var declared)) return Recorded.DroppedUnknownEvent;
        var line = new SortedDictionary<string, object>(StringComparer.Ordinal);
        (Recorded Why, string? Field)? problem = null;
        foreach (var (field, value) in fields)
        {
            if (!declared.TryGetValue(field, out var kind))
            {
                problem ??= (Recorded.DroppedUnknownField, null);
            }
            else if (Registry.Written(kind, value) is { } written && !line.ContainsKey(field))
            {
                line[field] = written;
            }
            else
            {
                problem ??= (Recorded.DroppedBadValue, field);
            }
        }

        if (problem is null && declared.Keys.FirstOrDefault(k => !line.ContainsKey(k)) is { } missing)
            problem = (Recorded.DroppedBadValue, missing);

        lock (_lock)
        {
            if (problem is { } p)
            {
                var key = (p.Why, name.Name, p.Field);
                if (!_told.Contains(key))
                {
                    var notice = new SortedDictionary<string, object>(StringComparer.Ordinal)
                    {
                        ["reason"] = p.Why == Recorded.DroppedUnknownField ? "unknown_field" : "bad_value",
                        ["event"] = name.Name,
                    };
                    if (p.Field is { } f) notice["field"] = f;
                    Append(DroppedEvent, notice);
                    _told.Add(key); // only once it is in the file: a failed write is said again next time
                }

                return p.Why;
            }

            var count = _written.GetValueOrDefault(name.Name);
            if (count >= MaxPerEvent)
            {
                if (!_capped.Contains(name.Name))
                {
                    Append(DroppedEvent, new SortedDictionary<string, object>(StringComparer.Ordinal) { ["reason"] = "capped", ["event"] = name.Name });
                    _capped.Add(name.Name);
                }

                return Recorded.Capped;
            }

            Append(name.Name, line);
            _written[name.Name] = count + 1;
            return Recorded.Written;
        }
    }

    /// <summary>One line, in the crate's field order: name, id (as <c>session</c>), version, os, arch, time, fields.</summary>
    private void Append(string name, SortedDictionary<string, object> fields)
    {
        using var buffer = new MemoryStream();
        using (var json = new Utf8JsonWriter(buffer))
        {
            json.WriteStartObject();
            json.WriteString("name", name);
            json.WriteString("session", Id);
            json.WriteString("version", _version);
            json.WriteString("os", Os);
            json.WriteString("arch", Arch);
            json.WriteString("time", DateTime.UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'", CultureInfo.InvariantCulture));
            if (fields.Count > 0)
            {
                json.WriteStartObject("fields");
                foreach (var (field, value) in fields)
                {
                    switch (value)
                    {
                        case ulong n: json.WriteNumber(field, n); break;
                        case bool b: json.WriteBoolean(field, b); break;
                        case string s: json.WriteString(field, s); break;
                        case string[] set:
                            json.WriteStartArray(field);
                            foreach (var v in set) json.WriteStringValue(v);
                            json.WriteEndArray();
                            break;
                    }
                }

                json.WriteEndObject();
            }

            json.WriteEndObject();
        }

        buffer.WriteByte((byte)'\n');
        var folder = Path.GetDirectoryName(Path.GetFullPath(File));
        if (folder is not null) Directory.CreateDirectory(folder);
        using var stream = new FileStream(File, FileMode.Append, FileAccess.Write, FileShare.Read);
        stream.Write(buffer.GetBuffer(), 0, (int)buffer.Length);
    }

    /// <summary>The operating system as Rust's <c>std::env::consts::OS</c> names it.</summary>
    internal static string Os =>
        OperatingSystem.IsWindows() ? "windows" : OperatingSystem.IsMacOS() ? "macos" : OperatingSystem.IsLinux() ? "linux" : "unknown";

    /// <summary>The architecture as Rust's <c>std::env::consts::ARCH</c> names it.</summary>
    internal static string Arch => RuntimeInformation.ProcessArchitecture switch
    {
        Architecture.X64 => "x86_64",
        Architecture.Arm64 => "aarch64",
        Architecture.X86 => "x86",
        Architecture.Arm => "arm",
        _ => "unknown",
    };
}
