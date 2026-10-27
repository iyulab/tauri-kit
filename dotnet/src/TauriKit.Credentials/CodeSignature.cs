using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using System.Text;

namespace TauriKit.Credentials;

/// <summary>
/// Who signed a program, as far as the macOS keychain cares. The keychain answers the program that made an item
/// without asking, and asks the person about it when a differently signed program reads it; a build signed ad hoc is a
/// new identity every time it is built, and a request that meets the question waits on it. An app that keeps secrets in
/// <see cref="Credentials"/> can look at <see cref="TeamIdentifier"/> first and keep them in memory when it is
/// <see langword="null"/> — a development or unsigned build.
/// </summary>
[SupportedOSPlatform("macos")]
public static unsafe class CodeSignature
{
    /// <summary>
    /// The Apple team that signed the running program, or <see langword="null"/> when it is unsigned or signed ad hoc
    /// (or by Apple itself, which has no team). Note that a program started as <c>dotnet app.dll</c> is the
    /// <c>dotnet</c> host: its signature, not the app's, is what the keychain sees.
    /// </summary>
    public static string? TeamIdentifier { get; } = Read(0);

    /// <summary>The Apple team that signed the program or bundle at <paramref name="path"/>, or <see langword="null"/> (see <see cref="TeamIdentifier"/>).</summary>
    public static string? TeamIdentifierOf(string path)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        var bytes = Encoding.UTF8.GetBytes(path);
        nint url;
        fixed (byte* p = bytes) url = Mac.CFURLCreateFromFileSystemRepresentation(0, p, bytes.Length, false);
        if (url == 0) return null;
        try
        {
            if (Mac.SecStaticCodeCreateWithPath(url, 0, out var code) != 0) return null;
            try
            {
                return Read(code);
            }
            finally
            {
                Mac.CFRelease(code);
            }
        }
        finally
        {
            Mac.CFRelease(url);
        }
    }

    // The team of the code in `code`; 0 stands for the running program.
    private static string? Read(nint code)
    {
        var self = 0;
        if (code == 0)
        {
            if (Mac.SecCodeCopySelf(0, out code) != 0) return null;
            self = 1;
        }

        try
        {
            if (Mac.SecCodeCopySigningInformation(code, Mac.SigningInformation, out var info) != 0) return null;
            try
            {
                var team = Mac.CFDictionaryGetValue(info, Mac.Signing.TeamIdentifierKey);
                return team == 0 ? null : Mac.String(team);
            }
            finally
            {
                Mac.CFRelease(info);
            }
        }
        finally
        {
            if (self == 1) Mac.CFRelease(code);
        }
    }
}

[SupportedOSPlatform("macos")]
internal static unsafe partial class Mac
{
    public const uint SigningInformation = 2; // kSecCSSigningInformation

    public static class Signing
    {
        private static readonly nint Lib = NativeLibrary.Load("/System/Library/Frameworks/Security.framework/Security");

        public static readonly nint TeamIdentifierKey = *(nint*)NativeLibrary.GetExport(Lib, "kSecCodeInfoTeamIdentifier");
    }

    public static string? String(nint cfString)
    {
        var buffer = new byte[512];
        fixed (byte* p = buffer)
            return CFStringGetCString(cfString, p, buffer.Length, Utf8) ? Encoding.UTF8.GetString(buffer, 0, Array.IndexOf(buffer, (byte)0)) : null;
    }

    [LibraryImport("/System/Library/Frameworks/Security.framework/Security")]
    public static partial int SecCodeCopySelf(uint flags, out nint self);

    [LibraryImport("/System/Library/Frameworks/Security.framework/Security")]
    public static partial int SecCodeCopySigningInformation(nint code, uint flags, out nint information);

    [LibraryImport("/System/Library/Frameworks/Security.framework/Security")]
    public static partial int SecStaticCodeCreateWithPath(nint path, uint flags, out nint code);

    [LibraryImport("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")]
    public static partial nint CFURLCreateFromFileSystemRepresentation(nint allocator, byte* buffer, nint length, [MarshalAs(UnmanagedType.U1)] bool isDirectory);

    [LibraryImport("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")]
    public static partial nint CFDictionaryGetValue(nint dictionary, nint key);

    [LibraryImport("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")]
    [return: MarshalAs(UnmanagedType.U1)]
    public static partial bool CFStringGetCString(nint text, byte* buffer, nint size, uint encoding);
}
