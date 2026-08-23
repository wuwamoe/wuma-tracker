using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.ServiceProcess;
using Microsoft.Win32;

namespace WumaTracker.Setup;

public enum SetupMode
{
    Install,
    Uninstall,
}

// Plain class, not a record — records' positional syntax needs
// System.Runtime.CompilerServices.IsExternalInit, which only modern .NET
// provides out of the box (net472 doesn't have it without a hand-rolled
// polyfill, not worth it for one tiny DTO).
public sealed class InstallProgress
{
    public int Percent { get; }
    public string Message { get; }

    public InstallProgress(int percent, string message)
    {
        Percent = percent;
        Message = message;
    }
}

/// <summary>
/// The actual install/uninstall work, ported from the retired nsDialogs
/// attempt (src-tauri/windows/nsis-custom/main.nsi in git history) —
/// same behavior, C# instead of NSIS script. See
/// specs/0005-wpf-installer.md in the wuma-base workspace.
/// </summary>
public static class Installer
{
    private const string ProductName = "명조 맵스 트래커";
    private const string Manufacturer = "wumadevs";
    private const string MainBinaryName = "wuma-tracker.exe";
    private const string DriverSysName = "WumaDisplayService.sys";
    private const string RegisterScriptName = "register-driver.ps1";
    private const string UnregisterScriptName = "unregister-driver.ps1";
    private const string UninstallKeyPath =
        @"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\WumaTracker";

    // Fixed, non-configurable — see specs/0005-wpf-installer.md's "no
    // directory picker" reasoning: a user-changeable path is one more
    // thing the *next* update has to get right.
    public static readonly string InstallDir = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "WumaTracker");

    // Environment.ProcessPath is .NET 6+ only — net472 needs the
    // Process.MainModule route instead.
    public static string? CurrentExePath => Process.GetCurrentProcess().MainModule?.FileName;

    public static void Run(IProgress<InstallProgress>? progress = null)
    {
        progress?.Report(new InstallProgress(5, "실행 중인 프로그램을 종료하는 중..."));
        StopRunningApp();

        progress?.Report(new InstallProgress(15, "이전 설치를 확인하는 중..."));
        RemovePriorMsiInstall();

        // A driver service already running from a prior install (the
        // update case, not just a fresh install) holds WumaDisplayService.sys
        // open — overwriting it below would fail with "file in use by
        // another process" otherwise. register-driver.ps1 (called later,
        // after the new .sys is in place) does a full stop+delete+recreate,
        // but that's too late for *this* problem: the file has to be
        // unlocked before we can even copy the new one over it. Just
        // stopping it here (not deleting the registration) is enough to
        // release the file handle; non-fatal since a fresh install has no
        // service to stop yet. ServiceController (not sc.exe + manual
        // polling) so we can actually wait for STOPPED, not just for the
        // stop *request* to be accepted.
        StopDriverServiceIfRunning();

        progress?.Report(new InstallProgress(40, "파일을 복사하는 중..."));
        Directory.CreateDirectory(InstallDir);
        ExtractPayload(MainBinaryName);
        ExtractPayload(DriverSysName);
        ExtractPayload(RegisterScriptName);
        ExtractPayload(UnregisterScriptName);
        CopySelfForUninstall();

        progress?.Report(new InstallProgress(60, "등록 정보를 기록하는 중..."));
        var version = FileVersionInfo.GetVersionInfo(Path.Combine(InstallDir, MainBinaryName))
            .FileVersion ?? "0.0.0";
        WriteUninstallRegistry(version);

        progress?.Report(new InstallProgress(75, "바로가기를 만드는 중..."));
        CreateShortcuts();

        progress?.Report(new InstallProgress(85, "드라이버를 등록하는 중..."));
        RunPowerShellScript(
            Path.Combine(InstallDir, RegisterScriptName),
            $"-SysPath \"{Path.Combine(InstallDir, DriverSysName)}\"",
            nonFatal: true);

        progress?.Report(new InstallProgress(100, "설치가 완료되었습니다"));
    }

    /// <summary>
    /// Launches the just-installed app, optionally forwarding whatever
    /// arguments it had before an in-place update (tauri-plugin-updater's
    /// `/ARGS ...` — see App.xaml.cs's flag parsing).
    /// </summary>
    public static void LaunchApp(string arguments = "")
    {
        Process.Start(new ProcessStartInfo
        {
            FileName = Path.Combine(InstallDir, MainBinaryName),
            Arguments = arguments,
            UseShellExecute = true,
        });
    }

    public static void Uninstall(IProgress<InstallProgress>? progress = null)
    {
        progress?.Report(new InstallProgress(10, "실행 중인 프로그램을 종료하는 중..."));
        StopRunningApp();

        progress?.Report(new InstallProgress(30, "드라이버를 해제하는 중..."));
        var unregisterScript = Path.Combine(InstallDir, UnregisterScriptName);
        if (File.Exists(unregisterScript))
        {
            RunPowerShellScript(unregisterScript, arguments: "", nonFatal: true);
        }

        progress?.Report(new InstallProgress(60, "파일을 삭제하는 중..."));
        DeleteShortcuts();
        if (Directory.Exists(InstallDir))
        {
            try { Directory.Delete(InstallDir, recursive: true); }
            catch { /* best effort — a locked file shouldn't block the rest */ }
        }

        progress?.Report(new InstallProgress(90, "등록 정보를 삭제하는 중..."));
        using var uninstallRoot = Registry.LocalMachine.OpenSubKey(
            @"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", writable: true);
        uninstallRoot?.DeleteSubKeyTree("WumaTracker", throwOnMissingSubKey: false);

        // If this is the %TEMP% copy App.xaml.cs relaunched itself as (the
        // normal case — the original copy in InstallDir just got deleted
        // above, along with itself), clean up after it's gone: it can't
        // delete its own running exe, but a detached helper process that
        // outlives it can.
        ScheduleSelfDeleteIfRunningFromTemp();

        progress?.Report(new InstallProgress(100, "제거가 완료되었습니다"));
    }

    private static void ScheduleSelfDeleteIfRunningFromTemp()
    {
        var currentExePath = CurrentExePath;
        if (currentExePath is null) return;
        if (!Path.GetFullPath(Path.GetDirectoryName(currentExePath) ?? "")
                .Equals(Path.GetFullPath(Path.GetTempPath()), StringComparison.OrdinalIgnoreCase))
        {
            return;
        }

        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = "cmd.exe",
                Arguments = $"/C ping 127.0.0.1 -n 3 >nul & del /f /q \"{currentExePath}\"",
                UseShellExecute = false,
                CreateNoWindow = true,
                WindowStyle = ProcessWindowStyle.Hidden,
            });
        }
        catch
        {
            // best effort — worst case a ~190MB temp file lingers, not
            // worth failing the uninstall over.
        }
    }

    private static void StopRunningApp()
    {
        foreach (var process in Process.GetProcessesByName("wuma-tracker"))
        {
            try
            {
                process.Kill();
                process.WaitForExit(3000);
            }
            catch
            {
                // best effort, same as CloseAppDeferred in the WiX MSI
            }
        }
    }

    private static void StopDriverServiceIfRunning()
    {
        try
        {
            using var service = new ServiceController("WumaDisplayService");
            if (service.Status != ServiceControllerStatus.Stopped
                && service.Status != ServiceControllerStatus.StopPending)
            {
                service.Stop();
            }
            service.WaitForStatus(ServiceControllerStatus.Stopped, TimeSpan.FromSeconds(10));
        }
        catch (InvalidOperationException)
        {
            // no such service — fresh install, nothing to stop
        }
        catch
        {
            // best effort — if this fails, ExtractPayload's own File.Create
            // will surface the real "still in use" error anyway, which is
            // more useful than swallowing it silently here.
        }
    }

    /// <summary>
    /// Detect a prior MSI install (see issues/WUMA-10 in wuma-base for the
    /// full "installation package could not be opened" writeup) and remove
    /// it. Unlike the retired 32-bit NSIS attempt, this process is native
    /// 64-bit, so HKLM\...\Uninstall here already resolves to the same
    /// native registry view the (64-bit) MSI itself registered under — no
    /// WOW6432Node redirection to route around.
    /// </summary>
    private static void RemovePriorMsiInstall()
    {
        using var uninstallRoot = Registry.LocalMachine.OpenSubKey(
            @"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall");
        if (uninstallRoot is null) return;

        foreach (var productCode in uninstallRoot.GetSubKeyNames())
        {
            using var entry = uninstallRoot.OpenSubKey(productCode);
            if (entry is null) continue;

            var displayName = entry.GetValue("DisplayName") as string;
            if (displayName != ProductName) continue;

            var publisher = entry.GetValue("Publisher") as string;
            if (publisher != Manufacturer) continue;

            // Written by Windows Installer itself for every product it
            // registers — distinguishes this from an NSIS/WPF install that
            // happens to share the same DisplayName/Publisher.
            var windowsInstaller = entry.GetValue("WindowsInstaller");
            if (windowsInstaller is not int flag || flag != 1) continue;

            // productCode (the registry key name) IS the MSI ProductCode
            // GUID for an MSI-registered product — reuse it directly
            // instead of parsing UninstallString, and force /X /qn
            // ourselves instead of whatever verb/mode happens to be
            // registered.
            RunProcess("msiexec.exe", $"/X{productCode} /qn /norestart", nonFatal: true);
            break;
        }
    }

    /// <summary>
    /// Files this installer places into Program Files are embedded into
    /// the published exe as resources by build-installer.ps1 (payload\ is
    /// populated before `dotnet publish`, see the .csproj) — keeps
    /// distribution as one file, same as the MSI/NSIS artifacts.
    /// </summary>
    private static void ExtractPayload(string fileName)
    {
        var assembly = Assembly.GetExecutingAssembly();
        using var stream = assembly.GetManifestResourceStream($"payload.{fileName}")
            ?? throw new FileNotFoundException(
                $"설치 페이로드가 이 실행 파일에 포함되어 있지 않습니다: {fileName} " +
                "(build-installer.ps1로 빌드했는지 확인하세요)");
        // build-installer.ps1 Brotli-compresses each payload file before
        // embedding it (via installer/PayloadCompressor) — net472 has no
        // PublishSingleFile-level compression to fall back on, and an
        // embedded resource is otherwise stored byte-for-byte as-is.
        // Brotli over gzip/deflate: measured on the real wuma-tracker.exe
        // payload, it matches xz/LZMA2 to within ~1% (7.1MB vs 6.8MB) —
        // gzip only got that same file down to 10.1MB.
        using var brotli = new BrotliSharpLib.BrotliStream(stream, CompressionMode.Decompress);
        using var output = File.Create(Path.Combine(InstallDir, fileName));
        brotli.CopyTo(output);
    }

    /// <summary>
    /// UninstallString points back at this installer with --uninstall
    /// (see WriteUninstallRegistry) instead of a separate uninstall.exe —
    /// so a copy of the running exe has to actually exist at that fixed
    /// name in InstallDir, however the downloaded/renamed original was
    /// named (build-installer.ps1's output is
    /// WumaTracker_&lt;version&gt;_x64-setup.exe, not WumaTracker.Setup.exe).
    /// </summary>
    private static void CopySelfForUninstall()
    {
        var currentExePath = CurrentExePath
            ?? throw new InvalidOperationException("현재 실행 파일 경로를 확인할 수 없습니다.");
        File.Copy(currentExePath, Path.Combine(InstallDir, "WumaTracker.Setup.exe"), overwrite: true);
    }

    private static void WriteUninstallRegistry(string version)
    {
        using var key = Registry.LocalMachine.CreateSubKey(UninstallKeyPath);
        key.SetValue("DisplayName", ProductName);
        key.SetValue("DisplayVersion", version);
        key.SetValue("Publisher", Manufacturer);
        key.SetValue("InstallLocation", InstallDir);
        key.SetValue("DisplayIcon", Path.Combine(InstallDir, MainBinaryName));
        key.SetValue("UninstallString",
            $"\"{Path.Combine(InstallDir, "WumaTracker.Setup.exe")}\" --uninstall");
        key.SetValue("NoModify", 1, RegistryValueKind.DWord);
        key.SetValue("NoRepair", 1, RegistryValueKind.DWord);
    }

    private static string StartMenuLinkPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.CommonPrograms), $"{ProductName}.lnk");

    private static string DesktopLinkPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.CommonDesktopDirectory), $"{ProductName}.lnk");

    private static void CreateShortcuts()
    {
        var target = Path.Combine(InstallDir, MainBinaryName);
        CreateShortcut(StartMenuLinkPath, target);
        CreateShortcut(DesktopLinkPath, target);
    }

    private static void DeleteShortcuts()
    {
        File.Delete(StartMenuLinkPath);
        File.Delete(DesktopLinkPath);
    }

    private static void CreateShortcut(string linkPath, string targetPath)
    {
        // No COM shortcut API ships with WPF by default; shelling out to
        // WScript.Shell (as PowerShell) is the same approach the rest of
        // this project already uses for anything Windows-Installer-adjacent.
        var script =
            $"$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{linkPath}'); " +
            $"$s.TargetPath = '{targetPath}'; $s.Save()";
        RunProcess("powershell.exe",
            $"-NoProfile -WindowStyle Hidden -Command \"{script}\"",
            nonFatal: true);
    }

    private static void RunPowerShellScript(string scriptPath, string arguments, bool nonFatal)
    {
        RunProcess("powershell.exe",
            $"-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{scriptPath}\" {arguments}",
            nonFatal);
    }

    /// <summary>
    /// Non-fatal by design where the caller says so — e.g. a stale
    /// Add/Remove Programs entry left behind by a prior MSI that can't
    /// uninstall itself cleanly (its own cache/registry state already
    /// corrupt — the exact failure mode issues/WUMA-10 exists to get away
    /// from) is cosmetic; refusing to install the update is the actual bug.
    /// </summary>
    private static void RunProcess(string fileName, string arguments, bool nonFatal)
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo
            {
                FileName = fileName,
                Arguments = arguments,
                UseShellExecute = false,
                CreateNoWindow = true,
            });
            process?.WaitForExit();
            if (process is { ExitCode: not 0 } && !nonFatal)
            {
                throw new InvalidOperationException(
                    $"{fileName} exited with code {process.ExitCode}");
            }
        }
        catch when (nonFatal)
        {
            // logged nowhere yet — acceptable for a first pass; the caller
            // continues either way.
        }
    }
}
