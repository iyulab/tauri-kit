using System.Diagnostics;
using System.Diagnostics.CodeAnalysis;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// Where a running loopback backend is and how to call it: its port, the credential it accepts, and which process it
/// is (its id and when it started, so a file left by one that died can be told from a live one).
/// </summary>
/// <param name="Port">The port it listens on, on 127.0.0.1.</param>
/// <param name="Token">The credential, sent as <c>Authorization: Bearer &lt;token&gt;</c>.</param>
/// <param name="Pid">The backend's process id.</param>
/// <param name="StartedAt">When that process started — in the file, milliseconds since the Unix epoch.</param>
public sealed record LoopbackDiscovery(int Port, string Token, int Pid, [property: JsonConverter(typeof(UnixMillisecondsConverter))] DateTimeOffset StartedAt);

/// <summary>A time as milliseconds since the Unix epoch — what a reader in any language compares without a date parser.</summary>
internal sealed class UnixMillisecondsConverter : JsonConverter<DateTimeOffset>
{
    public override DateTimeOffset Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options) =>
        DateTimeOffset.FromUnixTimeMilliseconds(reader.GetInt64());

    public override void Write(Utf8JsonWriter writer, DateTimeOffset value, JsonSerializerOptions options)
    {
        ArgumentNullException.ThrowIfNull(writer);
        writer.WriteNumberValue(value.ToUnixTimeMilliseconds());
    }
}

/// <summary>Why a discovery file gave no backend to call.</summary>
public enum DiscoveryProblem
{
    /// <summary>There is no file: no backend has published one, or the last one stopped cleanly.</summary>
    Missing,

    /// <summary>The file could not be read or is not a discovery record.</summary>
    Unreadable,

    /// <summary>The process that published it is gone (it did not stop cleanly).</summary>
    Stale,
}

/// <summary>
/// The file a loopback backend that lives on its own publishes for its callers — a shell, the backend's own tools,
/// another process of the same person. Only the person who runs the backend can read it: on Windows its access list
/// grants that user and the system and nothing else, inheriting nothing; elsewhere its mode is 0600. It is written
/// to a file beside it first and moved into place, so a reader never sees half of one. A caller re-reads it once on
/// a 401: every start issues a new credential.
/// </summary>
public static class DiscoveryFile
{
    /// <summary>
    /// Publishes this process as listening on <paramref name="port"/> with <paramref name="token"/>, replacing what
    /// <paramref name="path"/> held (its folder is created if missing), and returns what was written.
    /// </summary>
    public static LoopbackDiscovery Publish(string path, int port, string token)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        ArgumentNullException.ThrowIfNull(token);
        using var self = Process.GetCurrentProcess();
        var started = DateTimeOffset.FromUnixTimeMilliseconds(new DateTimeOffset(self.StartTime.ToUniversalTime(), TimeSpan.Zero).ToUnixTimeMilliseconds());
        var record = new LoopbackDiscovery(port, token, Environment.ProcessId, started);
        var full = Path.GetFullPath(path);
        Directory.CreateDirectory(Path.GetDirectoryName(full)!);
        var temporary = full + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            using (var stream = CreateOwnerOnly(temporary))
                JsonSerializer.Serialize(stream, record, LoopbackJson.Default.LoopbackDiscovery);
            File.Move(temporary, full, overwrite: true);
        }
        catch
        {
            File.Delete(temporary);
            throw;
        }

        return record;
    }

    /// <summary>
    /// The backend <paramref name="path"/> names, when the file is there, readable, and its process still runs.
    /// </summary>
    public static bool TryRead(string path, [NotNullWhen(true)] out LoopbackDiscovery? discovery, out DiscoveryProblem problem)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        discovery = null;
        LoopbackDiscovery? read;
        try
        {
            using var stream = File.OpenRead(path);
            read = JsonSerializer.Deserialize(stream, LoopbackJson.Default.LoopbackDiscovery);
        }
        catch (Exception e) when (e is FileNotFoundException or DirectoryNotFoundException)
        {
            problem = DiscoveryProblem.Missing;
            return false;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            problem = DiscoveryProblem.Unreadable;
            return false;
        }

        if (read is not { Port: > 0 and <= 65535, Token.Length: >= LoopbackCredentials.MinimumLength, Pid: > 0 })
        {
            problem = DiscoveryProblem.Unreadable;
            return false;
        }

        if (!IsRunning(read.Pid, read.StartedAt))
        {
            problem = DiscoveryProblem.Stale;
            return false;
        }

        discovery = read;
        problem = default;
        return true;
    }

    /// <summary>Removes the file at <paramref name="path"/> if it still holds <paramref name="published"/> — not one a later run replaced it with.</summary>
    public static bool RemoveIfStill(string path, LoopbackDiscovery published)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        ArgumentNullException.ThrowIfNull(published);
        try
        {
            LoopbackDiscovery? read;
            using (var stream = File.OpenRead(path))
                read = JsonSerializer.Deserialize(stream, LoopbackJson.Default.LoopbackDiscovery);
            if (read != published) return false;
            File.Delete(path);
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            return false;
        }
    }

    // A process id is reused once its process is gone, so the start time must match too (to the second — the clock the
    // record was written with and the one read now round differently).
    internal static bool IsRunning(int pid, DateTimeOffset startedAt)
    {
        try
        {
            using var process = Process.GetProcessById(pid);
            if (process.HasExited) return false;
            return (new DateTimeOffset(process.StartTime.ToUniversalTime(), TimeSpan.Zero) - startedAt).Duration() < TimeSpan.FromSeconds(2);
        }
        catch (ArgumentException)
        {
            return false; // no process has that id
        }
        catch (Exception e) when (e is InvalidOperationException or System.ComponentModel.Win32Exception or NotSupportedException)
        {
            return true; // there, but not ours to inspect: it cannot be shown dead
        }
    }

    private static FileStream CreateOwnerOnly(string path)
    {
        if (!OperatingSystem.IsWindows())
            return new FileStream(path, new FileStreamOptions
            {
                Mode = FileMode.CreateNew,
                Access = FileAccess.Write,
                UnixCreateMode = UnixFileMode.UserRead | UnixFileMode.UserWrite,
            });

        var security = new FileSecurity();
        security.SetAccessRuleProtection(isProtected: true, preserveInheritance: false);
        using var identity = WindowsIdentity.GetCurrent();
        var user = identity.User!;
        security.SetOwner(user);
        security.AddAccessRule(new FileSystemAccessRule(user, FileSystemRights.FullControl, AccessControlType.Allow));
        security.AddAccessRule(new FileSystemAccessRule(new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null), FileSystemRights.FullControl, AccessControlType.Allow));
        return new FileInfo(path).Create(FileMode.CreateNew, FileSystemRights.Write | FileSystemRights.ReadPermissions, FileShare.None, 4096, FileOptions.None, security);
    }
}
