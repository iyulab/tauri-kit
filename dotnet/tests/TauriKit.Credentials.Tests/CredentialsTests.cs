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
    public void A_secret_longer_than_the_store_allows_is_refused_before_writing()
    {
        if (!OperatingSystem.IsWindows()) { Assert.Skip("Credential Manager is Windows only"); return; }
        var credentials = new Credentials("com.example.app", BuildKind.Test);
        var refused = false;
        try
        {
            credentials.Set("a", new string('x', 1281));
        }
        catch (ArgumentException)
        {
            refused = true;
        }
        Assert.True(refused);
        Assert.Null(credentials.Get("a"));
    }
}
