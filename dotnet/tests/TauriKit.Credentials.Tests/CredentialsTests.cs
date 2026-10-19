using System.Reflection;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;

namespace TauriKit.Credentials.Tests;

public sealed class CredentialsTests
{
    [Fact]
    public void Each_kind_of_build_gets_its_own_service_name()
    {
        if (!OperatingSystem.IsWindows()) { Assert.Skip("Credential Manager is Windows only"); return; }
        Assert.Equal("com.example.app", new Credentials("com.example.app", BuildKind.Release).Service);
        Assert.Equal("com.example.app.dev", new Credentials("com.example.app", BuildKind.Dev).Service);
        Assert.Equal("com.example.app.test", new Credentials("com.example.app", BuildKind.Test).Service);
    }

    [Fact]
    public void An_entry_is_named_as_the_crate_names_it()
    {
        if (!OperatingSystem.IsWindows()) { Assert.Skip("Credential Manager is Windows only"); return; }
        Assert.Equal("api-key.com.example.app.test", new Credentials("com.example.app", BuildKind.Test).TargetName("api-key"));
    }

    [Fact]
    public void This_test_assembly_is_a_debug_build_and_a_release_assembly_is_not()
    {
#if DEBUG
        Assert.Equal(BuildKind.Dev, BuildKinds.Of(typeof(CredentialsTests).Assembly));
#else
        Assert.Equal(BuildKind.Release, BuildKinds.Of(typeof(CredentialsTests).Assembly));
#endif
        Assert.Equal(BuildKind.Release, BuildKinds.Of(typeof(object).Assembly));
    }

    [Fact]
    public void A_secret_is_stored_read_and_removed_in_the_crates_layout()
    {
        if (!OperatingSystem.IsWindows()) { Assert.Skip("Credential Manager is Windows only"); return; }
        RoundTrip();
    }

    [SupportedOSPlatform("windows")]
    private static unsafe void RoundTrip()
    {
        var credentials = new Credentials($"tauri-kit.credentials-tests.{Environment.ProcessId}", BuildKind.Test);
        const string Account = "api-key";
        try
        {
            Assert.Null(credentials.Get(Account));
            Assert.False(credentials.Contains(Account));

            credentials.Set(Account, "s3cret — 비밀");
            Assert.Equal("s3cret — 비밀", credentials.Get(Account));
            Assert.True(credentials.Contains(Account));

            // What the crate reads: a generic credential, user name = account, secret = UTF-16LE without a terminator.
            Assert.True(Native.CredRead(credentials.TargetName(Account), Native.CredTypeGeneric, 0, out var raw));
            try
            {
                var c = (Native.Credential*)raw;
                Assert.Equal(Native.CredTypeGeneric, c->Type);
                Assert.Equal(Account, Marshal.PtrToStringUni(c->UserName));
                Assert.Equal(Native.CredPersistEnterprise, c->Persist);
                var blob = new ReadOnlySpan<byte>((void*)c->CredentialBlob, (int)c->CredentialBlobSize).ToArray();
                Assert.Equal(System.Text.Encoding.Unicode.GetBytes("s3cret — 비밀"), blob);
            }
            finally
            {
                Native.CredFree(raw);
            }

            credentials.Set(Account, "replaced");
            Assert.Equal("replaced", credentials.Get(Account));
        }
        finally
        {
            credentials.Delete(Account);
        }

        Assert.Null(credentials.Get(Account));
        credentials.Delete(Account); // removing one that is not there is not an error
    }

    [Fact]
    public void A_session_longer_than_one_entry_round_trips_through_the_store()
    {
        if (!OperatingSystem.IsWindows()) { Assert.Skip("Credential Manager is Windows only"); return; }
        // About what a signed-in OIDC session takes (two JWTs and a refresh token): past 1,280 UTF-16 units.
        var credentials = new Credentials($"tauri-kit.credentials-tests.{Environment.ProcessId}", BuildKind.Test);
        var secret = string.Concat(Enumerable.Repeat("eyJhbGciOiJSUzI1NiIsImtpZCI6IjEifQ.😀", 120));
        try
        {
            credentials.Set("long", secret);
            Assert.Equal(secret, credentials.Get("long"));
            credentials.Set("long", "short again");
            Assert.Equal("short again", credentials.Get("long"));
            Assert.Null(Raw(credentials, "long/1"));
        }
        finally
        {
            credentials.Delete("long");
        }
        Assert.Null(credentials.Get("long"));
    }

    [Fact]
    [SupportedOSPlatform("windows")] // a Windows-only skip guard does not reach the analyzer inside a lambda
    public void A_long_secret_is_laid_out_in_parts_exactly_as_the_crate_lays_it_out()
    {
        if (!OperatingSystem.IsWindows()) { Assert.Skip("Credential Manager is Windows only"); return; }
        // The crate's mock-store test `parts_are_laid_out_the_same_in_both_languages` pins the same strings.
        var credentials = new Credentials($"tauri-kit.credentials-tests.layout.{Environment.ProcessId}", BuildKind.Test, partUnits: 40);
        try
        {
            credentials.Set("s", new string('x', 100));
            Assert.Equal("\0tauri-kit-credentials/parts=4\0" + new string('x', 8), Raw(credentials, "s"));
            Assert.Equal(new string('x', 40), Raw(credentials, "s/1"));
            Assert.Equal(new string('x', 12), Raw(credentials, "s/3"));
            Assert.Equal(new string('x', 100), credentials.Get("s"));

            credentials.Delete("s/2"); // a part gone: an error, not a cut secret
            Assert.Throws<InvalidDataException>(() => credentials.Get("s"));
        }
        finally
        {
            credentials.Delete("s");
        }
        Assert.Null(Raw(credentials, "s"));
        Assert.Null(Raw(credentials, "s/1"));
        Assert.Null(Raw(credentials, "s/3"));
    }

    [Fact]
    [SupportedOSPlatform("windows")] // a Windows-only skip guard does not reach the analyzer inside a lambda
    public void A_secret_may_not_begin_with_nul()
    {
        if (!OperatingSystem.IsWindows()) { Assert.Skip("Credential Manager is Windows only"); return; }
        var credentials = new Credentials("com.example.app", BuildKind.Test);
        Assert.Throws<ArgumentException>(() => credentials.Set("a", "\0x"));
        Assert.Null(credentials.Get("a"));
    }

    // One entry as stored, with no joining of parts.
    [SupportedOSPlatform("windows")]
    private static unsafe string? Raw(Credentials credentials, string account)
    {
        if (!Native.CredRead(credentials.TargetName(account), Native.CredTypeGeneric, 0, out var raw)) return null;
        try
        {
            var c = (Native.Credential*)raw;
            return System.Text.Encoding.Unicode.GetString(new ReadOnlySpan<byte>((void*)c->CredentialBlob, (int)c->CredentialBlobSize));
        }
        finally
        {
            Native.CredFree(raw);
        }
    }
}
