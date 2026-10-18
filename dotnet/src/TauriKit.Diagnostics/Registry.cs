using System.Collections.ObjectModel;
using System.Globalization;
using System.Text.Json;

namespace TauriKit.Diagnostics;

/// <summary>The kind of one declared field — the same closed kinds as the tauri-kit-diagnostics crate.</summary>
public abstract record FieldKind
{
    private protected FieldKind() { }

    /// <summary>A whole number, written as the lower bound of its bucket, or as itself when at most <c>Max</c>.</summary>
    public sealed record Count(IReadOnlyList<ulong> Bucket, ulong? Max) : FieldKind;

    /// <summary>True or false.</summary>
    public sealed record Bool : FieldKind;

    /// <summary>One of the declared values.</summary>
    public sealed record Choice(IReadOnlyList<string> Values) : FieldKind;

    /// <summary>Some of the declared values, written sorted (the registry's <c>set</c>).</summary>
    public sealed record Subset(IReadOnlyList<string> Values) : FieldKind;

    /// <summary>Milliseconds, at most <c>MaxMs</c> (default and most: one day).</summary>
    public sealed record Duration(ulong? MaxMs) : FieldKind;
}

/// <summary>Why a registry was refused.</summary>
public sealed class RegistryException(string message) : Exception(message);

/// <summary>
/// The events an app may write, each with its fields — read from the same registry JSON the crate reads
/// (<c>{ "events": { "&lt;name&gt;": { "fields": { "&lt;field&gt;": { "kind": … } } } } }</c>), refused by the same
/// rules: a name that is not a plain lowercase identifier or starts with <c>kit.</c>, more than eight fields, a
/// count without a bucket or a cap (or a cap over 1000, or bucket bounds that do not start at 0 and at least double
/// after the first non-zero one), a choice or set without plain values, a duration cap over a day, a name given twice.
/// Its <c>reports</c> (<c>{ "&lt;layer&gt;": { "&lt;detail&gt;": { "kind": … } } }</c>) declare, by the same rules, the details
/// each layer's reports may carry (<see cref="Layer(string)"/>). Embed the file in the app rather than reading it at run time: whoever can edit it decides what is written.
/// </summary>
public sealed class Registry
{
    /// <summary>How many fields an event may declare.</summary>
    public const int MaxFields = 8;

    /// <summary>How many values a choice or a set may declare.</summary>
    public const int MaxValues = 64;

    /// <summary>The largest count cap a registry may declare.</summary>
    public const ulong MaxCount = 1000;

    /// <summary>The longest duration, in milliseconds: one day.</summary>
    public const ulong MaxDurationMs = 86_400_000;

    internal Registry(
        IReadOnlyDictionary<string, IReadOnlyDictionary<string, FieldKind>> events,
        IReadOnlyDictionary<string, IReadOnlyDictionary<string, FieldKind>> reports)
    {
        Events = events;
        Reports = reports;
    }

    /// <summary>The declared events and their fields.</summary>
    public IReadOnlyDictionary<string, IReadOnlyDictionary<string, FieldKind>> Events { get; }

    /// <summary>The details each layer's reports may carry, by layer name (the registry's <c>reports</c>).</summary>
    public IReadOnlyDictionary<string, IReadOnlyDictionary<string, FieldKind>> Reports { get; }

    /// <summary>Reads and checks a registry.</summary>
    /// <exception cref="RegistryException">The registry is not one, or could carry text.</exception>
    public static Registry FromJson(string json)
    {
        JsonDocument document;
        try
        {
            document = JsonDocument.Parse(json);
        }
        catch (JsonException e)
        {
            throw new RegistryException($"not a registry: {e.Message}");
        }

        using (document)
        {
            var root = Object(document.RootElement, "the registry", "events", "reports");
            var events = new SortedDictionary<string, IReadOnlyDictionary<string, FieldKind>>(StringComparer.Ordinal);
            if (root.TryGetValue("events", out var eventsElement))
            {
                foreach (var (name, eventElement) in Object(eventsElement, "events", null))
                {
                    if (!IsEventName(name)) throw new RegistryException($"'{name}' is not a plain lowercase event name");
                    if (name.StartsWith("kit.", StringComparison.Ordinal)) throw new RegistryException($"'{name}': names under 'kit.' are the crate's own");
                    var eventObject = Object(eventElement, $"'{name}'", "fields");
                    var fields = new SortedDictionary<string, FieldKind>(StringComparer.Ordinal);
                    if (eventObject.TryGetValue("fields", out var fieldsElement))
                    {
                        foreach (var (field, kindElement) in Object(fieldsElement, $"'{name}' fields", null))
                        {
                            if (!IsFieldName(field)) throw new RegistryException($"'{name}.{field}' is not a plain lowercase field name");
                            fields[field] = Kind(kindElement, $"'{name}.{field}'");
                        }
                    }

                    if (fields.Count > MaxFields) throw new RegistryException($"'{name}' has more than {MaxFields} fields");
                    events[name] = new ReadOnlyDictionary<string, FieldKind>(fields);
                }
            }

            var reports = new SortedDictionary<string, IReadOnlyDictionary<string, FieldKind>>(StringComparer.Ordinal);
            if (root.TryGetValue("reports", out var reportsElement))
            {
                foreach (var (layer, detailsElement) in Object(reportsElement, "reports", null))
                {
                    var details = new SortedDictionary<string, FieldKind>(StringComparer.Ordinal);
                    foreach (var (detail, kindElement) in Object(detailsElement, $"layer '{layer}'", null))
                    {
                        if (!IsFieldName(detail)) throw new RegistryException($"'{layer}.{detail}' is not a plain lowercase detail name");
                        details[detail] = Kind(kindElement, $"'{layer}.{detail}'");
                    }

                    if (details.Count > MaxFields) throw new RegistryException($"layer '{layer}' has more than {MaxFields} details");
                    reports[layer] = new ReadOnlyDictionary<string, FieldKind>(details);
                }
            }

            return new Registry(
                new ReadOnlyDictionary<string, IReadOnlyDictionary<string, FieldKind>>(events),
                new ReadOnlyDictionary<string, IReadOnlyDictionary<string, FieldKind>>(reports));
        }
    }

    /// <summary>
    /// A <see cref="Diagnostics.Layer"/> named <paramref name="name"/> carrying the details this registry declares for it under
    /// <c>reports</c> — none, if it declares none.
    /// </summary>
    public Layer Layer(string name) =>
        new(name, Reports.TryGetValue(name, out var details) ? details : Diagnostics.Layer.NoDetails);

    /// <summary>The declared event <paramref name="name"/> — a misspelled name fails here, at start.</summary>
    /// <exception cref="RegistryException">The registry does not declare it.</exception>
    public EventName Event(string name) =>
        Events.ContainsKey(name) ? new EventName(name) : throw new RegistryException($"the registry does not declare '{name}'");

    /// <summary>The members of an object, refusing a member given twice and, when <paramref name="allowed"/> is given, any other.</summary>
    private static Dictionary<string, JsonElement> Object(JsonElement element, string what, params string[]? allowed)
    {
        if (element.ValueKind != JsonValueKind.Object) throw new RegistryException($"{what} is not an object");
        var members = new Dictionary<string, JsonElement>(StringComparer.Ordinal);
        foreach (var property in element.EnumerateObject())
        {
            if (allowed is not null && Array.IndexOf(allowed, property.Name) < 0) throw new RegistryException($"{what}: unknown member '{property.Name}'");
            if (!members.TryAdd(property.Name, property.Value)) throw new RegistryException($"'{property.Name}' is given twice");
        }

        return members;
    }

    private static FieldKind Kind(JsonElement element, string what)
    {
        if (element.ValueKind != JsonValueKind.Object || !element.TryGetProperty("kind", out var kind) || kind.ValueKind != JsonValueKind.String)
            throw new RegistryException($"{what}: a field is an object with a kind");
        switch (kind.GetString())
        {
            case "count":
            {
                var members = Object(element, what, "kind", "bucket", "max");
                var bucket = members.TryGetValue("bucket", out var b) ? Numbers(b, what) : [];
                ulong? max = members.TryGetValue("max", out var m) ? Number(m, what) : null;
                if (bucket.Count == 0 && max is null) throw new RegistryException($"{what}: a count needs a bucket or a max");
                if (bucket.Count > 0 && max is not null) throw new RegistryException($"{what}: a count takes a bucket or a max, not both");
                if (max is { } cap && cap is < 1 or > MaxCount) throw new RegistryException($"{what}: max must be 1 to {MaxCount}");
                if (bucket.Count > 0)
                {
                    if (bucket[0] != 0 || bucket.Zip(bucket.Skip(1)).Any(p => p.First >= p.Second)) throw new RegistryException($"{what}: bucket bounds must start at 0 and ascend");
                    if (bucket.Zip(bucket.Skip(1)).Any(p => p.First != 0 && p.Second < p.First * 2)) throw new RegistryException($"{what}: each bucket bound after the first non-zero one must be at least twice the one before");
                    if (bucket.Count > MaxValues) throw new RegistryException($"{what}: at most {MaxValues} bucket bounds");
                }

                return new FieldKind.Count(bucket, max);
            }
            case "bool":
                Object(element, what, "kind");
                return new FieldKind.Bool();
            case "choice":
            case "set":
            {
                var members = Object(element, what, "kind", "values");
                if (!members.TryGetValue("values", out var v) || v.ValueKind != JsonValueKind.Array) throw new RegistryException($"{what}: 1 to {MaxValues} values");
                var values = v.EnumerateArray().Select(x => x.ValueKind == JsonValueKind.String ? x.GetString()! : "").ToList();
                if (values.Count is 0 or > MaxValues) throw new RegistryException($"{what}: 1 to {MaxValues} values");
                if (values.Distinct(StringComparer.Ordinal).Count() != values.Count) throw new RegistryException($"{what}: a value is repeated");
                if (!values.All(IsValue)) throw new RegistryException($"{what}: values are plain identifiers of at most 32 characters");
                return kind.GetString() == "choice" ? new FieldKind.Choice(values) : new FieldKind.Subset(values);
            }
            case "duration":
            {
                var members = Object(element, what, "kind", "max_ms");
                ulong? maxMs = members.TryGetValue("max_ms", out var m) ? Number(m, what) : null;
                if (maxMs is { } ms && ms is 0 or > MaxDurationMs) throw new RegistryException($"{what}: max_ms must be 1 ms to one day");
                return new FieldKind.Duration(maxMs);
            }
            default:
                throw new RegistryException($"not a registry: {what} has an unknown kind '{kind.GetString()}'");
        }
    }

    private static ulong Number(JsonElement element, string what) =>
        element.ValueKind == JsonValueKind.Number && element.TryGetUInt64(out var n) ? n : throw new RegistryException($"{what}: not a whole number");

    private static List<ulong> Numbers(JsonElement element, string what) =>
        element.ValueKind == JsonValueKind.Array ? [.. element.EnumerateArray().Select(e => Number(e, what))] : throw new RegistryException($"{what}: a bucket is a list");

    internal static bool IsEventName(string name) =>
        name.Length <= 64 && name.Split('.').Select((part, i) => part.Length > 0
            && part.All(c => c is (>= 'a' and <= 'z') or (>= '0' and <= '9') or '_')
            && (i > 0 || part[0] is >= 'a' and <= 'z')).All(ok => ok);

    internal static bool IsFieldName(string name) =>
        name.Length is >= 1 and <= 32 && name[0] is >= 'a' and <= 'z' && name.All(c => c is (>= 'a' and <= 'z') or (>= '0' and <= '9') or '_');

    internal static bool IsValue(string value) =>
        value.Length is >= 1 and <= 32 && char.IsAsciiLetterOrDigit(value[0]) && value.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '-' or '.');

    /// <summary>The value as it is written, if it fits <paramref name="kind"/>; strings for choices, sorted lists for sets.</summary>
    internal static object? Written(FieldKind kind, Value value) => (kind, value) switch
    {
        (FieldKind.Count c, Value.CountValue n) => c.Max is { } max
            ? (n.N <= max ? n.N : null)
            : c.Bucket.LastOrDefault(b => b <= n.N, ulong.MaxValue) is var bound && bound != ulong.MaxValue ? bound : null,
        (FieldKind.Bool, Value.BoolValue b) => b.B,
        (FieldKind.Choice c, Value.ChoiceValue v) => c.Values.Contains(v.V, StringComparer.Ordinal) ? v.V : null,
        (FieldKind.Subset s, Value.SetValue v) => v.Vs.All(x => s.Values.Contains(x, StringComparer.Ordinal))
            ? v.Vs.Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal).ToArray()
            : null,
        (FieldKind.Duration d, Value.DurationValue t) => t.D >= TimeSpan.Zero && (ulong)t.D.TotalMilliseconds <= (d.MaxMs ?? MaxDurationMs)
            ? (ulong)t.D.TotalMilliseconds
            : null,
        _ => null,
    };

    internal static string Format(ulong n) => n.ToString(CultureInfo.InvariantCulture);

    /// <summary>
    /// A report detail as it is written, if <paramref name="raw"/> — untrusted text from where the failure was caught — is
    /// a value of <paramref name="kind"/>: a count, a duration in milliseconds, <c>true</c>/<c>false</c>, a declared choice,
    /// or declared values joined with <c>,</c>. The text is <see cref="Written"/>'s, so a count lands on its bucket.
    /// </summary>
    internal static string? DetailText(FieldKind kind, string raw)
    {
        ulong? number = raw.Length is >= 1 and <= 20 && raw.All(char.IsAsciiDigit)
            && ulong.TryParse(raw, NumberStyles.None, CultureInfo.InvariantCulture, out var n) ? n : null;
        Value? value = kind switch
        {
            FieldKind.Count => number is { } c ? Value.Count(c) : null,
            FieldKind.Duration => number is { } ms && ms <= MaxDurationMs ? Value.Duration(TimeSpan.FromMilliseconds(ms)) : null,
            FieldKind.Bool => raw switch { "true" => Value.Bool(true), "false" => Value.Bool(false), _ => null },
            FieldKind.Choice => Value.Choice(raw),
            FieldKind.Subset => Value.Subset([.. raw.Split(',', StringSplitOptions.RemoveEmptyEntries)]),
            _ => null,
        };
        return (value is null ? null : Written(kind, value)) switch
        {
            ulong w => Format(w),
            bool b => b ? "true" : "false",
            string s => s,
            string[] set => string.Join(',', set),
            _ => null,
        };
    }
}

/// <summary>A name a <see cref="Registry"/> declares, from <see cref="Registry.Event"/>.</summary>
public sealed record EventName
{
    internal EventName(string name) => Name = name;

    /// <summary>The name.</summary>
    public string Name { get; }
}

/// <summary>One field's value, as the app hands it to <see cref="Events.Record"/>.</summary>
public abstract record Value
{
    private protected Value() { }

    /// <summary>For a count.</summary>
    public static Value Count(ulong n) => new CountValue(n);

    /// <summary>For true or false.</summary>
    public static Value Bool(bool b) => new BoolValue(b);

    /// <summary>For a choice: one of its declared values.</summary>
    public static Value Choice(string value) => new ChoiceValue(value);

    /// <summary>For a set (<see cref="FieldKind.Subset"/>): some of its declared values.</summary>
    public static Value Subset(params string[] values) => new SetValue(values);

    /// <summary>For a duration.</summary>
    public static Value Duration(TimeSpan duration) => new DurationValue(duration);

    internal sealed record CountValue(ulong N) : Value;

    internal sealed record BoolValue(bool B) : Value;

    internal sealed record ChoiceValue(string V) : Value;

    internal sealed record SetValue(string[] Vs) : Value;

    internal sealed record DurationValue(TimeSpan D) : Value;
}
