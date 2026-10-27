using System.Diagnostics;
using System.Runtime.Versioning;

namespace TauriKit.Credentials.Tests;

public sealed class CodeSignatureTests
{
    [Fact]
    [SupportedOSPlatform("macos")]
    public void A_program_signed_ad_hoc_has_no_team()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("Code signatures are read on macOS only"); return; }
        var copy = Path.Combine(Path.GetTempPath(), $"tauri-kit-adhoc-{Environment.ProcessId}");
        File.Copy("/usr/bin/true", copy, overwrite: true);
        try
        {
            Run("/usr/bin/codesign", "--force", "--sign", "-", copy);
            Assert.Null(CodeSignature.TeamIdentifierOf(copy));
        }
        finally
        {
            File.Delete(copy);
        }
    }

    [Fact]
    [SupportedOSPlatform("macos")]
    public void The_running_program_reports_the_team_of_the_file_it_was_started_from()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("Code signatures are read on macOS only"); return; }
        // Under `dotnet test` that file is the dotnet host: whoever signed it (null when the host is not signed by a team).
        Assert.Equal(CodeSignature.TeamIdentifierOf(Environment.ProcessPath!), CodeSignature.TeamIdentifier);
    }

    [Fact]
    [SupportedOSPlatform("macos")]
    public void A_path_that_is_not_there_or_not_code_has_no_team()
    {
        if (!OperatingSystem.IsMacOS()) { Assert.Skip("Code signatures are read on macOS only"); return; }
        Assert.Null(CodeSignature.TeamIdentifierOf("/nonexistent/tauri-kit/program"));
        var text = Path.GetTempFileName();
        try
        {
            File.WriteAllText(text, "not a program");
            Assert.Null(CodeSignature.TeamIdentifierOf(text));
        }
        finally
        {
            File.Delete(text);
        }
    }

    private static void Run(string tool, params string[] args)
    {
        var start = new ProcessStartInfo(tool) { RedirectStandardError = true, RedirectStandardOutput = true };
        foreach (var arg in args) start.ArgumentList.Add(arg);
        using var process = Process.Start(start)!;
        process.WaitForExit();
        Assert.Equal(0, process.ExitCode);
    }
}
