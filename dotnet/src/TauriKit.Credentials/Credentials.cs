using System.ComponentModel;
using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;

namespace TauriKit.Credentials;

/// <summary>
/// Which kind of build is running — each kind keeps its secrets under its own entries, so a test that ends by clearing
/// an entry does not clear the one the installed app relies on, and a development build does not sign in over it.
/// </summary>
public enum BuildKind
{
    /// <summary>A test run: entries under <c>&lt;service&gt;.test</c>.</summary>
    Test,

    /// <summary>A debug build: entries under <c>&lt;service&gt;.dev</c>.</summary>
    Dev,

    /// <summary>A release build — the entries the installed app uses: <c>&lt;service&gt;</c>.</summary>
    Release,
}

/// <summary>Reading a <see cref="BuildKind"/> off an assembly.</summary>
public static class BuildKinds
{
    /// <summary>
    /// <see cref="BuildKind.Dev"/> when <paramref name="assembly"/> — pass the app's own, <c>typeof(Program).Assembly</c> —
    /// was compiled without optimization (a Debug build), otherwise <see cref="BuildKind.Release"/>. A test run passes
    /// <see cref="BuildKind.Test"/> itself: a library cannot tell that it is being tested.
    /// </summary>
    public static BuildKind Of(Assembly assembly)
    {
        ArgumentNullException.ThrowIfNull(assembly);
        return assembly.GetCustomAttribute<DebuggableAttribute>()?.IsJITOptimizerDisabled == true ? BuildKind.Dev : BuildKind.Release;
    }
}

/// <summary>
/// Secrets an app keeps under one service name, one per account, in the platform's own store — the same entries the
/// tauri-kit-credentials crate reads and writes, so a Rust shell and its .NET backend built the same way share each secret.
/// <list type="bullet">
/// <item><description><b>Windows</b>: the Credential Manager — a generic credential named
/// <c>&lt;account&gt;.&lt;service&gt;</c> (the service with <c>.test</c> or <c>.dev</c> for those builds), its user name
/// the account, the secret as UTF-16.</description></item>
/// <item><description><b>macOS</b>: the login keychain — a generic password with service attribute
/// <c>&lt;service&gt;</c> (with the same suffixes) and account attribute <c>&lt;account&gt;</c>, the secret as UTF-8.
/// The keychain asks the person before a program reads an item that another, differently signed program made, so the
/// shell and the backend share a secret without a question only when both are signed as the same developer.</description></item>
/// </list>
/// </summary>
/// <remarks>
/// On Windows one entry holds at most 2,560 bytes — 1,280 UTF-16 units — which a signed-in session easily passes. A
/// longer secret is kept in parts, in the crate's layout: the entry for <c>&lt;account&gt;</c> holds
/// <c>"\0tauri-kit-credentials/parts=&lt;n&gt;\0"</c> and the first part, the entry for <c>&lt;account&gt;/&lt;i&gt;</c>
/// part <c>i</c>. Reading joins them, and a missing part is an error rather than a cut secret. A secret that fits is one
/// entry as before. A secret may therefore not begin with <c>'\0'</c>. A keychain item holds a secret of any length, so
/// on macOS nothing is cut.
/// </remarks>
/// <example>
/// <code>
/// var credentials = new Credentials("com.example.app", BuildKinds.Of(typeof(Program).Assembly));
/// credentials.Set("api-key", "s3cret");
/// var key = credentials.Get("api-key"); // null when there is none
/// credentials.Delete("api-key");
/// </code>
/// </example>
[SupportedOSPlatform("windows")]
[SupportedOSPlatform("macos")]
public sealed class Credentials
{
    // Long secrets, as the crate keeps them (its «Long secrets» section).
    private const string PartsMarker = "\0tauri-kit-credentials/parts=";
    private const int MaxParts = 64;

    private readonly IEntryStore _store;
    private readonly int _partUnits;

    /// <summary>Secrets under <paramref name="service"/> (for example the app's identifier), as seen by this kind of build.</summary>
    public Credentials(string service, BuildKind kind)
    {
        ArgumentException.ThrowIfNullOrEmpty(service);
        _store = OperatingSystem.IsWindows() ? new WindowsStore()
            : OperatingSystem.IsMacOS() ? new KeychainStore()
            : throw new PlatformNotSupportedException("Credentials are kept in the Windows Credential Manager or the macOS keychain.");
        _partUnits = _store.PartUnits;
        Service = kind switch
        {
            BuildKind.Test => service + ".test",
            BuildKind.Dev => service + ".dev",
            BuildKind.Release => service,
            _ => throw new ArgumentOutOfRangeException(nameof(kind)),
        };
    }

    /// <summary>Parts of at most <paramref name="partUnits"/> UTF-16 units — for testing the layout with small parts.</summary>
    internal Credentials(string service, BuildKind kind, int partUnits) : this(service, kind) => _partUnits = partUnits;

    /// <summary>The service name entries are actually stored under.</summary>
    public string Service { get; }

    /// <summary>
    /// The name the platform keeps <paramref name="account"/>'s entry under: on Windows the Credential Manager's
    /// <c>&lt;account&gt;.&lt;service&gt;</c>, on macOS the keychain item's account (its service attribute is <see cref="Service"/>).
    /// </summary>
    public string TargetName(string account) => _store.Name(Service, account);

    /// <summary>Stores <paramref name="secret"/> for <paramref name="account"/>, replacing any earlier one (and keeping its user name, alias and comment).</summary>
    /// <exception cref="ArgumentException">The secret begins with <c>'\0'</c>, or would take more than 64 entries.</exception>
    public void Set(string account, string secret)
    {
        ArgumentNullException.ThrowIfNull(secret);
        if (secret.StartsWith('\0')) throw new ArgumentException("A secret may not begin with a NUL character.", nameof(secret));
        TargetName(account);
        var before = PartCount(account);
        var parts = Split(secret);
        for (var i = 1; i < parts.Count; i++) WriteEntry(PartAccount(account, i), parts[i]);
        WriteEntry(account, parts.Count == 1 ? parts[0] : $"{PartsMarker}{parts.Count}\0{parts[0]}");
        RemoveParts(account, parts.Count, before);
    }

    /// <summary>The secret stored for <paramref name="account"/>, or <see langword="null"/> when there is none.</summary>
    /// <exception cref="InvalidDataException">An entry holds bytes that are not UTF-16 text (written by another program), or a part of a long secret is missing.</exception>
    public string? Get(string account)
    {
        var head = ReadEntry(account);
        if (head is null || ParseHead(head) is not var (count, first)) return head;
        var secret = new System.Text.StringBuilder(first);
        for (var i = 1; i < count; i++)
            secret.Append(ReadEntry(PartAccount(account, i)) ?? throw new InvalidDataException($"Part {i} of {count} of this secret is missing."));
        return secret.ToString();
    }

    /// <summary>Whether a secret is stored for <paramref name="account"/>, without handing it out.</summary>
    public bool Contains(string account) => Get(account) is not null;

    /// <summary>Removes the secret for <paramref name="account"/>. Removing one that is not there is not an error.</summary>
    public void Delete(string account)
    {
        RemoveParts(account, 1, PartCount(account));
        RemoveEntry(account);
    }

    private static string PartAccount(string account, int i) => $"{account}/{i}";

    // How many parts the secret now under the account is kept in (1 when it is one entry or none). A first entry whose
    // marker cannot be read counts as one: writing over it is how it is fixed.
    private int PartCount(string account)
    {
        var head = ReadEntry(account);
        try
        {
            return head is not null && ParseHead(head) is var (count, _) ? count : 1;
        }
        catch (InvalidDataException)
        {
            return 1;
        }
    }

    private void RemoveParts(string account, int from, int to)
    {
        for (var i = Math.Max(from, 1); i < to; i++) RemoveEntry(PartAccount(account, i));
    }

    private static (int Count, string First)? ParseHead(string head)
    {
        if (!head.StartsWith(PartsMarker, StringComparison.Ordinal)) return null;
        var rest = head.AsSpan(PartsMarker.Length);
        var end = rest.IndexOf('\0');
        if (end < 0 || !int.TryParse(rest[..end], System.Globalization.NumberStyles.None, System.Globalization.CultureInfo.InvariantCulture, out var count) || count is < 2 or > MaxParts)
            throw new InvalidDataException("The first entry's parts marker is malformed.");
        return (count, rest[(end + 1)..].ToString());
    }

    // The secret cut into parts that each fit one entry, the first leaving room for the marker (at most two digits).
    private List<string> Split(string secret)
    {
        if (secret.Length <= _partUnits) return [secret];
        var parts = new List<string>();
        var room = _partUnits - (PartsMarker.Length + 3);
        var start = 0;
        while (start < secret.Length)
        {
            var length = Math.Min(room, secret.Length - start);
            if (start + length < secret.Length && char.IsHighSurrogate(secret[start + length - 1])) length--; // keep a pair whole
            parts.Add(secret.Substring(start, length));
            start += length;
            room = _partUnits;
        }
        if (parts.Count > MaxParts) throw new ArgumentException($"The secret is longer than {MaxParts} Credential Manager entries hold.", nameof(secret));
        return parts;
    }

    private void WriteEntry(string account, string secret) => _store.Write(Service, account, secret);

    private string? ReadEntry(string account) => _store.Read(Service, account);

    private void RemoveEntry(string account) => _store.Remove(Service, account);
}

[SupportedOSPlatform("windows")]
internal static partial class Native
{
    public const uint CredTypeGeneric = 1;
    public const uint CredPersistEnterprise = 3;
    public const int ErrorNotFound = 1168;

    [StructLayout(LayoutKind.Sequential)]
    public struct Credential
    {
        public uint Flags;
        public uint Type;
        public nint TargetName;
        public nint Comment;
        public uint LastWrittenLow; // FILETIME: two DWORDs, 4-byte aligned
        public uint LastWrittenHigh;
        public uint CredentialBlobSize;
        public nint CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public nint Attributes;
        public nint TargetAlias;
        public nint UserName;
    }

    [LibraryImport("advapi32.dll", EntryPoint = "CredReadW", SetLastError = true, StringMarshalling = StringMarshalling.Utf16)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool CredRead(string target, uint type, uint flags, out nint credential);

    [LibraryImport("advapi32.dll", EntryPoint = "CredWriteW", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static unsafe partial bool CredWrite(Credential* credential, uint flags);

    [LibraryImport("advapi32.dll", EntryPoint = "CredDeleteW", SetLastError = true, StringMarshalling = StringMarshalling.Utf16)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool CredDelete(string target, uint type, uint flags);

    [LibraryImport("advapi32.dll", EntryPoint = "CredFree")]
    public static partial void CredFree(nint buffer);
}
