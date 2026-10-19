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
/// Secrets an app keeps under one service name, one per account, in the Windows Credential Manager — the same entries
/// the tauri-kit-credentials crate reads and writes: a generic credential named <c>&lt;account&gt;.&lt;service&gt;</c>
/// (the service with <c>.test</c> or <c>.dev</c> for those builds), its user name the account, the secret as UTF-16.
/// A Rust shell and its .NET backend built the same way therefore share each secret.
/// </summary>
/// <remarks>
/// One entry holds at most 2,560 bytes — 1,280 UTF-16 units — which a signed-in session easily passes. A longer secret
/// is kept in parts, in the crate's layout: the entry for <c>&lt;account&gt;</c> holds
/// <c>"\0tauri-kit-credentials/parts=&lt;n&gt;\0"</c> and the first part, the entry for <c>&lt;account&gt;/&lt;i&gt;</c>
/// part <c>i</c>. Reading joins them, and a missing part is an error rather than a cut secret. A secret that fits is one
/// entry as before. A secret may therefore not begin with <c>'\0'</c>.
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
public sealed class Credentials
{
    // The Windows limits the crate checks too: a secret of at most 2,560 bytes, names of at most 32,767 characters.
    private const int MaxSecretBytes = 5 * 512;
    private const int MaxTargetLength = 32767;
    private const int MaxUserNameLength = 513;

    // Long secrets, as the crate keeps them (its «Long secrets» section).
    private const string PartsMarker = "\0tauri-kit-credentials/parts=";
    private const int MaxParts = 64;

    private readonly int _partUnits = MaxSecretBytes / 2;

    /// <summary>Secrets under <paramref name="service"/> (for example the app's identifier), as seen by this kind of build.</summary>
    public Credentials(string service, BuildKind kind)
    {
        ArgumentException.ThrowIfNullOrEmpty(service);
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

    /// <summary>The Credential Manager name of <paramref name="account"/>'s entry.</summary>
    public string TargetName(string account)
    {
        ArgumentException.ThrowIfNullOrEmpty(account);
        var target = account + "." + Service;
        if (target.Length > MaxTargetLength) throw new ArgumentException("The account and service together are longer than the Credential Manager allows.", nameof(account));
        if (account.Length > MaxUserNameLength) throw new ArgumentException("The account is longer than the Credential Manager allows for a user name.", nameof(account));
        return target;
    }

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

    private unsafe void WriteEntry(string account, string secret)
    {
        var target = TargetName(account);
        var blob = new byte[secret.Length * 2];
        System.Text.Encoding.Unicode.GetBytes(secret, blob); // UTF-16LE, no terminator — as the crate writes it
        if (blob.Length > MaxSecretBytes) throw new ArgumentException("The secret is longer than the Credential Manager allows (2,560 bytes as UTF-16).", nameof(secret));

        var (user, alias, comment) = (account, (string?)null, (string?)null);
        if (Native.CredRead(target, Native.CredTypeGeneric, 0, out var existing))
        {
            try
            {
                var old = (Native.Credential*)existing;
                user = Marshal.PtrToStringUni(old->UserName) ?? account;
                alias = Marshal.PtrToStringUni(old->TargetAlias);
                comment = Marshal.PtrToStringUni(old->Comment);
            }
            finally
            {
                Native.CredFree(existing);
            }
        }

        fixed (char* targetPtr = target)
        fixed (char* userPtr = user)
        fixed (char* aliasPtr = alias ?? string.Empty)
        fixed (char* commentPtr = comment ?? string.Empty)
        fixed (byte* blobPtr = blob)
        {
            var credential = new Native.Credential
            {
                Type = Native.CredTypeGeneric,
                TargetName = (nint)targetPtr,
                Comment = (nint)commentPtr,
                CredentialBlobSize = (uint)blob.Length,
                CredentialBlob = (nint)blobPtr,
                Persist = Native.CredPersistEnterprise,
                TargetAlias = (nint)aliasPtr,
                UserName = (nint)userPtr,
            };
            var written = Native.CredWrite(&credential, 0);
            Array.Clear(blob);
            if (!written) throw new Win32Exception(Marshal.GetLastPInvokeError());
        }
    }

    private unsafe string? ReadEntry(string account)
    {
        var target = TargetName(account);
        if (!Native.CredRead(target, Native.CredTypeGeneric, 0, out var found))
        {
            var error = Marshal.GetLastPInvokeError();
            if (error == Native.ErrorNotFound) return null;
            throw new Win32Exception(error);
        }

        try
        {
            var credential = (Native.Credential*)found;
            var size = (int)credential->CredentialBlobSize;
            if (size % 2 != 0) throw new InvalidDataException("The entry's secret is not UTF-16 text.");
            var bytes = new ReadOnlySpan<byte>((void*)credential->CredentialBlob, size);
            try
            {
                return new System.Text.UnicodeEncoding(bigEndian: false, byteOrderMark: false, throwOnInvalidBytes: true).GetString(bytes);
            }
            catch (System.Text.DecoderFallbackException e)
            {
                throw new InvalidDataException("The entry's secret is not UTF-16 text.", e);
            }
        }
        finally
        {
            Native.CredFree(found);
        }
    }

    private void RemoveEntry(string account)
    {
        var target = TargetName(account);
        if (Native.CredDelete(target, Native.CredTypeGeneric, 0)) return;
        var error = Marshal.GetLastPInvokeError();
        if (error != Native.ErrorNotFound) throw new Win32Exception(error);
    }
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
