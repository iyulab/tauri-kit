using System.Text.Json;
using System.Text.Json.Serialization;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// What an unexpected failure was, without anything it was about: the exception's type and the
/// method in the app's own code where it was thrown. The message is left out — it can quote the
/// data the request was about, or a path.
/// </summary>
public sealed record FaultView(string Type, string? At);

/// <summary>The body of a 500 answer: <c>{"fault":{"type":…,"at":…}}</c>.</summary>
public sealed record FaultResponse(FaultView Fault);

/// <summary>Reads a <see cref="FaultView"/> off an exception.</summary>
public static class Fault
{
    /// <summary>The exception's type and its innermost frame in <paramref name="ownNamespaces"/>.</summary>
    public static FaultView Of(Exception exception, IReadOnlyList<string> ownNamespaces)
    {
        ArgumentNullException.ThrowIfNull(exception);
        return new(exception.GetType().FullName ?? exception.GetType().Name, OwnFrame(exception.StackTrace, ownNamespaces));
    }

    /// <summary>
    /// The innermost frame of <paramref name="trace"/> in one of <paramref name="ownNamespaces"/>, as
    /// <c>Namespace.Type.Method</c>. Read from the rendered trace, which is also what an
    /// ahead-of-time compiled build has.
    /// </summary>
    public static string? OwnFrame(string? trace, IReadOnlyList<string> ownNamespaces)
    {
        ArgumentNullException.ThrowIfNull(ownNamespaces);
        if (trace is null) return null;
        foreach (var line in trace.Split('\n'))
        {
            var frame = line.Trim();
            if (!frame.StartsWith("at ", StringComparison.Ordinal)) continue;
            frame = frame[3..];
            var call = frame.IndexOf('(', StringComparison.Ordinal);
            if (call > 0) frame = frame[..call];
            foreach (var own in ownNamespaces)
            {
                if (frame.StartsWith(own, StringComparison.Ordinal)) return frame;
            }
        }
        return null;
    }
}

[JsonSourceGenerationOptions(JsonSerializerDefaults.Web)]
[JsonSerializable(typeof(FaultResponse))]
internal sealed partial class LoopbackJson : JsonSerializerContext;
