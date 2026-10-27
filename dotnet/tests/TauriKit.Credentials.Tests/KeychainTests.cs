using System.Diagnostics;
using System.Runtime.Versioning;
using System.Text;

namespace TauriKit.Credentials.Tests;

// The macOS login keychain. Every entry sits under a service of this process's own, made and read by this process, so the
// keychain asks nobody anything and a person's entries are never touched.
public sealed class KeychainTests
{
    private static string Service => $"tauri-kit.credentials-tests.{Environment.ProcessId}";

    [Fact]
    [SupportedOSPlatform("macos")]
    public void A_secret_is_stored_read_replaced_and_removed()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("The keychain is macOS only"); return; }
        var credentials = new Credentials(Service, BuildKind.Test);
        const string Account = "api-key";
        try
        {
            Assert.Null(credentials.Get(Account));
            Assert.False(credentials.Contains(Account));

            credentials.Set(Account, "s3cret — 비밀");
            Assert.Equal("s3cret — 비밀", credentials.Get(Account));
            Assert.True(credentials.Contains(Account));

            credentials.Set(Account, "replaced"); // the item is there: it is updated, not added twice
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
    [SupportedOSPlatform("macos")]
    public void An_item_is_a_generic_password_the_crate_reads_with_the_secret_as_utf8()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("The keychain is macOS only"); return; }
        var credentials = new Credentials(Service, BuildKind.Test);
        Assert.Equal(Service + ".test", credentials.Service);
        try
        {
            credentials.Set("api-key", "s3cret — 비밀");

            // The attributes only (no -w): the secret itself is not asked for, so the tool needs no permission.
            var (code, output) = Security("find-generic-password", "-s", credentials.Service, "-a", "api-key");
            Assert.Equal(0, code);
            Assert.Contains("\"genp\"", output);
            Assert.Contains($"\"svce\"<blob>=\"{credentials.Service}\"", output);
            Assert.Contains("\"acct\"<blob>=\"api-key\"", output);

            Assert.Equal(Encoding.UTF8.GetBytes("s3cret — 비밀"), KeychainStore.ReadBytes(credentials.Service, "api-key"));
        }
        finally
        {
            credentials.Delete("api-key");
        }

        Assert.NotEqual(0, Security("find-generic-password", "-s", credentials.Service, "-a", "api-key").Code);
    }

    [Fact]
    [SupportedOSPlatform("macos")]
    public void A_session_longer_than_a_windows_entry_is_one_item()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("The keychain is macOS only"); return; }
        var credentials = new Credentials(Service, BuildKind.Test);
        var secret = string.Concat(Enumerable.Repeat("eyJhbGciOiJSUzI1NiIsImtpZCI6IjEifQ.😀", 120));
        try
        {
            credentials.Set("long", secret);
            Assert.Equal(secret, credentials.Get("long"));
            Assert.Null(KeychainStore.ReadBytes(credentials.Service, "long/1")); // nothing was cut into parts
        }
        finally
        {
            credentials.Delete("long");
        }

        Assert.Null(credentials.Get("long"));
    }

    [Fact]
    [SupportedOSPlatform("macos")]
    public void A_secret_may_not_begin_with_nul_and_a_name_may_not_be_empty()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("The keychain is macOS only"); return; }
        var credentials = new Credentials(Service, BuildKind.Test);
        Assert.Throws<ArgumentException>(() => credentials.Set("a", "\0x"));
        Assert.Throws<ArgumentException>(() => credentials.Set("", "x"));
        Assert.Null(credentials.Get("a"));
    }

    [Fact]
    [SupportedOSPlatform("macos")]
    public void Two_services_do_not_see_each_others_secrets()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("The keychain is macOS only"); return; }
        var one = new Credentials(Service + ".one", BuildKind.Test);
        var other = new Credentials(Service + ".other", BuildKind.Test);
        try
        {
            one.Set("k", "1");
            Assert.Null(other.Get("k"));
            other.Set("k", "2");
            Assert.Equal("1", one.Get("k"));
        }
        finally
        {
            one.Delete("k");
            other.Delete("k");
        }
    }

    private static (int Code, string Output) Security(params string[] args)
    {
        var start = new ProcessStartInfo("/usr/bin/security") { RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var arg in args) start.ArgumentList.Add(arg);
        using var process = Process.Start(start)!;
        var output = process.StandardOutput.ReadToEnd() + process.StandardError.ReadToEnd();
        process.WaitForExit();
        return (process.ExitCode, output);
    }
}
