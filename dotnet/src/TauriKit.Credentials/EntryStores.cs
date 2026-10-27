using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using System.Text;

namespace TauriKit.Credentials;

/// <summary>Where <see cref="Credentials"/> keeps one entry on the platform it runs on.</summary>
internal interface IEntryStore
{
    /// <summary>How many UTF-16 units one entry holds before a secret is cut into parts (<see cref="int.MaxValue"/>: never).</summary>
    int PartUnits { get; }

    /// <summary>The name the platform keeps the entry for <paramref name="account"/> under; refuses a name it cannot hold.</summary>
    string Name(string service, string account);

    string? Read(string service, string account);

    void Write(string service, string account, string secret);

    void Remove(string service, string account);
}

/// <summary>The Windows Credential Manager: a generic credential named <c>&lt;account&gt;.&lt;service&gt;</c>, the secret as UTF-16.</summary>
[SupportedOSPlatform("windows")]
internal sealed class WindowsStore : IEntryStore
{
    // The Windows limits the crate checks too: a secret of at most 2,560 bytes, names of at most 32,767 characters.
    private const int MaxSecretBytes = 5 * 512;
    private const int MaxTargetLength = 32767;
    private const int MaxUserNameLength = 513;

    public int PartUnits => MaxSecretBytes / 2;

    public string Name(string service, string account)
    {
        ArgumentException.ThrowIfNullOrEmpty(account);
        var target = account + "." + service;
        if (target.Length > MaxTargetLength) throw new ArgumentException("The account and service together are longer than the Credential Manager allows.", nameof(account));
        if (account.Length > MaxUserNameLength) throw new ArgumentException("The account is longer than the Credential Manager allows for a user name.", nameof(account));
        return target;
    }

    public unsafe void Write(string service, string account, string secret)
    {
        var target = Name(service, account);
        var blob = new byte[secret.Length * 2];
        Encoding.Unicode.GetBytes(secret, blob); // UTF-16LE, no terminator — as the crate writes it
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

    public unsafe string? Read(string service, string account)
    {
        var target = Name(service, account);
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
                return new UnicodeEncoding(bigEndian: false, byteOrderMark: false, throwOnInvalidBytes: true).GetString(bytes);
            }
            catch (DecoderFallbackException e)
            {
                throw new InvalidDataException("The entry's secret is not UTF-16 text.", e);
            }
        }
        finally
        {
            Native.CredFree(found);
        }
    }

    public void Remove(string service, string account)
    {
        var target = Name(service, account);
        if (Native.CredDelete(target, Native.CredTypeGeneric, 0)) return;
        var error = Marshal.GetLastPInvokeError();
        if (error != Native.ErrorNotFound) throw new Win32Exception(error);
    }
}

/// <summary>
/// The macOS login keychain: a generic password whose service attribute is the service and whose account attribute is
/// the account, the secret as UTF-8 — what the crate's keychain store (<c>apple-native-keyring-store</c>) reads and
/// writes. An item holds the whole secret, so nothing is ever cut into parts.
/// </summary>
/// <remarks>
/// The keychain lets the program that made an item read it again without asking; another program is asked about
/// unless the two are signed as the same developer. Updates of one signed app are, so an installed app keeps reading
/// its own entries; an ad-hoc signed build is a new identity every time it is built.
/// </remarks>
[SupportedOSPlatform("macos")]
internal sealed unsafe class KeychainStore : IEntryStore
{
    public int PartUnits => int.MaxValue;

    public string Name(string service, string account)
    {
        ArgumentException.ThrowIfNullOrEmpty(account);
        return account;
    }

    public string? Read(string service, string account)
    {
        var bytes = ReadBytes(service, account);
        if (bytes is null) return null;
        try
        {
            return new UTF8Encoding(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true).GetString(bytes);
        }
        catch (DecoderFallbackException e)
        {
            throw new InvalidDataException("The entry's secret is not UTF-8 text.", e);
        }
        finally
        {
            Array.Clear(bytes);
        }
    }

    /// <summary>The secret as the keychain holds it, or <see langword="null"/> when there is no such item.</summary>
    internal static byte[]? ReadBytes(string service, string account)
    {
        using var query = Mac.Query(service, account);
        query.Set(Mac.Security.ReturnData, Mac.CoreFoundation.True);
        query.Set(Mac.Security.MatchLimit, Mac.Security.MatchLimitOne);
        var status = Mac.SecItemCopyMatching(query.Handle, out var found);
        if (status == Mac.ErrItemNotFound) return null;
        Mac.Check(status, "read");
        try
        {
            var length = (int)Mac.CFDataGetLength(found);
            return new ReadOnlySpan<byte>((void*)Mac.CFDataGetBytePtr(found), length).ToArray();
        }
        finally
        {
            Mac.CFRelease(found);
        }
    }

    public void Write(string service, string account, string secret)
    {
        var bytes = Encoding.UTF8.GetBytes(secret);
        try
        {
            using var data = Mac.Data(bytes);
            using var add = Mac.Query(service, account);
            add.Set(Mac.Security.ValueData, data.Handle);
            var status = Mac.SecItemAdd(add.Handle, 0);
            if (status == Mac.ErrDuplicateItem)
            {
                using var match = Mac.Query(service, account);
                using var change = Mac.Dictionary();
                change.Set(Mac.Security.ValueData, data.Handle);
                status = Mac.SecItemUpdate(match.Handle, change.Handle);
            }

            Mac.Check(status, "write");
        }
        finally
        {
            Array.Clear(bytes);
        }
    }

    public void Remove(string service, string account)
    {
        using var query = Mac.Query(service, account);
        var status = Mac.SecItemDelete(query.Handle);
        if (status != Mac.ErrItemNotFound) Mac.Check(status, "remove");
    }
}

/// <summary>Security and CoreFoundation, called directly — no package.</summary>
[SupportedOSPlatform("macos")]
internal static unsafe partial class Mac
{
    private const string SecurityLib = "/System/Library/Frameworks/Security.framework/Security";
    private const string CoreFoundationLib = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";

    public const int ErrItemNotFound = -25300;
    public const int ErrDuplicateItem = -25299;

    /// <summary>The Security framework's constants (each a pointer-sized global the library exports).</summary>
    public static class Security
    {
        private static readonly nint Lib = NativeLibrary.Load(SecurityLib);

        public static readonly nint Class = Global(Lib, "kSecClass");
        public static readonly nint ClassGenericPassword = Global(Lib, "kSecClassGenericPassword");
        public static readonly nint AttrService = Global(Lib, "kSecAttrService");
        public static readonly nint AttrAccount = Global(Lib, "kSecAttrAccount");
        public static readonly nint ValueData = Global(Lib, "kSecValueData");
        public static readonly nint ReturnData = Global(Lib, "kSecReturnData");
        public static readonly nint MatchLimit = Global(Lib, "kSecMatchLimit");
        public static readonly nint MatchLimitOne = Global(Lib, "kSecMatchLimitOne");
    }

    public static class CoreFoundation
    {
        private static readonly nint Lib = NativeLibrary.Load(CoreFoundationLib);

        public static readonly nint True = Global(Lib, "kCFBooleanTrue");
        public static readonly nint KeyCallbacks = NativeLibrary.GetExport(Lib, "kCFTypeDictionaryKeyCallBacks");
        public static readonly nint ValueCallbacks = NativeLibrary.GetExport(Lib, "kCFTypeDictionaryValueCallBacks");
    }

    private static nint Global(nint library, string name) => *(nint*)NativeLibrary.GetExport(library, name);

    /// <summary>A dictionary that holds the CoreFoundation objects put into it, and lets them go with itself.</summary>
    public sealed class Dict : IDisposable
    {
        private readonly List<nint> _owned = [];

        public Dict() => Handle = CFDictionaryCreateMutable(0, 0, CoreFoundation.KeyCallbacks, CoreFoundation.ValueCallbacks);

        public nint Handle { get; }

        public void Set(nint key, nint value) => CFDictionarySetValue(Handle, key, value);

        /// <summary>Puts a string value in; the string is released with the dictionary.</summary>
        public void Set(nint key, string value)
        {
            var text = CFStringCreateWithCString(0, value, Utf8);
            _owned.Add(text);
            CFDictionarySetValue(Handle, key, text);
        }

        public void Dispose()
        {
            CFRelease(Handle);
            foreach (var item in _owned) CFRelease(item);
        }
    }

    /// <summary>A CFData holding a copy of some bytes.</summary>
    public sealed class DataRef : IDisposable
    {
        public DataRef(byte[] bytes)
        {
            fixed (byte* p = bytes) Handle = CFDataCreate(0, p, bytes.Length);
        }

        public nint Handle { get; }

        public void Dispose() => CFRelease(Handle);
    }

    public static Dict Dictionary() => new();

    public static DataRef Data(byte[] bytes) => new(bytes);

    /// <summary>The attributes that name one generic password.</summary>
    public static Dict Query(string service, string account)
    {
        var dict = new Dict();
        dict.Set(Security.Class, Security.ClassGenericPassword);
        dict.Set(Security.AttrService, service);
        dict.Set(Security.AttrAccount, account);
        return dict;
    }

    public static void Check(int status, string what)
    {
        if (status == 0) return;
        var meaning = status switch
        {
            -128 => "the person declined the keychain's question",
            -25293 => "the keychain refused the credentials",
            -25308 => "the keychain would have to ask the person, and may not here",
            -25307 => "there is no default keychain",
            -25295 => "the default keychain is not available",
            _ => "keychain error",
        };
        throw new IOException($"The keychain could not {what} the entry: {meaning} (OSStatus {status}).");
    }

    public const uint Utf8 = 0x08000100;

    [LibraryImport(SecurityLib)]
    public static partial int SecItemCopyMatching(nint query, out nint result);

    [LibraryImport(SecurityLib)]
    public static partial int SecItemAdd(nint attributes, nint result);

    [LibraryImport(SecurityLib)]
    public static partial int SecItemUpdate(nint query, nint attributesToUpdate);

    [LibraryImport(SecurityLib)]
    public static partial int SecItemDelete(nint query);

    [LibraryImport(CoreFoundationLib)]
    public static partial nint CFDictionaryCreateMutable(nint allocator, nint capacity, nint keyCallbacks, nint valueCallbacks);

    [LibraryImport(CoreFoundationLib)]
    public static partial void CFDictionarySetValue(nint dictionary, nint key, nint value);

    [LibraryImport(CoreFoundationLib, StringMarshalling = StringMarshalling.Utf8)]
    public static partial nint CFStringCreateWithCString(nint allocator, string text, uint encoding);

    [LibraryImport(CoreFoundationLib)]
    public static partial nint CFDataCreate(nint allocator, byte* bytes, nint length);

    [LibraryImport(CoreFoundationLib)]
    public static partial nint CFDataGetBytePtr(nint data);

    [LibraryImport(CoreFoundationLib)]
    public static partial nint CFDataGetLength(nint data);

    [LibraryImport(CoreFoundationLib)]
    public static partial void CFRelease(nint cf);
}
