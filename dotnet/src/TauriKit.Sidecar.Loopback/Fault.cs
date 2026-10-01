using System.Text.Json;
using System.Text.Json.Serialization;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// What an unexpected failure was, without anything it was about: the exception's type, the
/// method in the app's own code where it was thrown (<see cref="At"/>), and the app's own methods
/// it passed through on the way, innermost first (<see cref="Frames"/>, whose first is
/// <see cref="At"/>) — the same failure type can come from more than one path, and the path is what
/// tells them apart. The message is left out — it can quote the data the request was about, or a path.
/// </summary>
public sealed record FaultView(string Type, string? At, IReadOnlyList<string> Frames);

/// <summary>The body of a 500 answer: <c>{"fault":{"type":…,"at":…}}</c>.</summary>
public sealed record FaultResponse(FaultView Fault);

/// <summary>Reads a <see cref="FaultView"/> off an exception.</summary>
public static class Fault
{
    /// <summary>How many of the app's own frames a fault keeps unless told otherwise.</summary>
    public const int DefaultMaxFrames = 20;

    /// <summary>
    /// The exception's type and its frames in <paramref name="ownNamespaces"/>, innermost first, at
    /// most <paramref name="maxFrames"/> of them.
    /// </summary>
    public static FaultView Of(Exception exception, IReadOnlyList<string> ownNamespaces, int maxFrames = DefaultMaxFrames)
    {
        ArgumentNullException.ThrowIfNull(exception);
        var frames = OwnFrames(exception.StackTrace, ownNamespaces, maxFrames);
        return new(exception.GetType().FullName ?? exception.GetType().Name, frames.Count > 0 ? frames[0] : null, frames);
    }

    /// <summary>
    /// The innermost frame of <paramref name="trace"/> in one of <paramref name="ownNamespaces"/>, as
    /// <c>Namespace.Type.Method</c>.
    /// </summary>
    public static string? OwnFrame(string? trace, IReadOnlyList<string> ownNamespaces)
    {
        var frames = OwnFrames(trace, ownNamespaces, 1);
        return frames.Count > 0 ? frames[0] : null;
    }

    /// <summary>
    /// The frames of <paramref name="trace"/> in one of <paramref name="ownNamespaces"/>, innermost
    /// first, as <c>Namespace.Type.Method</c> — at most <paramref name="maxFrames"/>, and a frame
    /// repeated right after itself (a retry, a recursion) once. Read from the rendered trace, which
    /// is also what an ahead-of-time compiled build has.
    /// </summary>
    public static IReadOnlyList<string> OwnFrames(string? trace, IReadOnlyList<string> ownNamespaces, int maxFrames = DefaultMaxFrames)
    {
        ArgumentNullException.ThrowIfNull(ownNamespaces);
        ArgumentOutOfRangeException.ThrowIfNegative(maxFrames);
        var frames = new List<string>();
        if (trace is null) return frames;
        foreach (var line in trace.Split('\n'))
        {
            if (frames.Count == maxFrames) break;
            var frame = line.Trim();
            if (!frame.StartsWith("at ", StringComparison.Ordinal)) continue;
            frame = frame[3..];
            var call = frame.IndexOf('(', StringComparison.Ordinal);
            if (call > 0) frame = frame[..call];
            if (!ownNamespaces.Any(own => frame.StartsWith(own, StringComparison.Ordinal))) continue;
            if (frames.Count > 0 && frames[^1] == frame) continue;
            frames.Add(frame);
        }
        return frames;
    }
}

[JsonSourceGenerationOptions(JsonSerializerDefaults.Web)]
[JsonSerializable(typeof(FaultResponse))]
internal sealed partial class LoopbackJson : JsonSerializerContext;
