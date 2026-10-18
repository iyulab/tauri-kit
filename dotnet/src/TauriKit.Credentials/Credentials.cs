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
    public unsafe void Set(string account, string secret)
    {
        ArgumentNullException.ThrowIfNull(secret);
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

    /// <summary>The secret stored for <paramref name="account"/>, or <see langword="null"/> when there is none.</summary>
    /// <exception cref="InvalidDataException">The entry holds bytes that are not UTF-16 text (written by another program).</exception>
    public unsafe string? Get(string account)
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

    /// <summary>Whether a secret is stored for <paramref name="account"/>, without handing it out.</summary>
    public bool Contains(string account) => Get(account) is not null;

    /// <summary>Removes the secret for <paramref name="account"/>. Removing one that is not there is not an error.</summary>
    public void Delete(string account)
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
