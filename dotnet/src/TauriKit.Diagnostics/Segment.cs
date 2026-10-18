using System.Security.Cryptography;

namespace TauriKit.Diagnostics;

/// <summary>
/// One process's part of a stream kept in a folder — the tauri-kit-diagnostics crate's <c>Segment</c>: a file this
/// process alone appends to (hand <see cref="File"/> to <see cref="Events"/> or <see cref="Reporter"/>), beside a lock
/// <c>&lt;id&gt;.lock</c> it holds while the segment is open, which the operating system lets go when the process goes,
/// however it goes. The app's shell reads every segment of the folder (<c>segments</c>) and deletes a segment only once
/// it can take that lock (<c>tidy</c>), so it never rewrites or removes lines this process is still writing. Keep the
/// segment open for as long as anything writes its file.
/// </summary>
/// <remarks>
/// The lock is the lock file opened with <see cref="FileShare.None"/> for the segment's life — on Windows another process
/// cannot open it; elsewhere .NET takes an exclusive <c>flock</c> on it, as the crate does. (A byte-range
/// <see cref="FileStream.Lock(long, long)"/> would not do: on Linux and macOS it is an <c>fcntl</c> lock, which a
/// <c>flock</c> does not see.)
/// </remarks>
public sealed class Segment : IDisposable
{
    private readonly FileStream _lock;

    private Segment(string file, FileStream held)
    {
        File = file;
        _lock = held;
    }

    /// <summary>The file this process appends to.</summary>
    public string File { get; }

    /// <summary>A new segment of the stream kept in <paramref name="dir"/>, creating the folder when needed. Its file is made when it is first written.</summary>
    public static Segment Open(string dir)
    {
        ArgumentException.ThrowIfNullOrEmpty(dir);
        Directory.CreateDirectory(dir);
        var id = Convert.ToHexStringLower(RandomNumberGenerator.GetBytes(16));
        var held = new FileStream(Path.Combine(dir, id + ".lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        return new Segment(Path.Combine(dir, id + ".jsonl"), held);
    }

    /// <summary>Lets the lock go: the segment is ended, and the shell may delete it once it has sent it.</summary>
    public void Dispose() => _lock.Dispose();
}
