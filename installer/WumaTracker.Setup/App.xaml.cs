using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Windows;

namespace WumaTracker.Setup;

public partial class App : Application
{
    // Runs at the very first touch of this type — guaranteed before
    // OnStartup, i.e. before Installer.cs's ExtractPayload (the only place
    // that actually needs BrotliSharpLib's types) ever gets JITted. Without
    // this, loading BrotliSharpLib.dll's own EmbeddedResource copy (see the
    // .csproj) is pointless — the CLR would already have failed to find the
    // loose reference assembly by then and thrown FileNotFoundException.
    static App()
    {
        AppDomain.CurrentDomain.AssemblyResolve += ResolveEmbeddedAssembly;
    }

    // BrotliSharpLib.dll itself, plus whatever it transitively needs that
    // isn't part of the .NET Framework BCL/GAC (currently just this one —
    // if a future package bump pulls in more, the fix is the same shape:
    // embed the resolved DLL in the .csproj, list its name here).
    private static readonly string[] EmbeddedAssemblyNames =
    {
        "BrotliSharpLib",
        "System.Runtime.CompilerServices.Unsafe",
    };

    private static Assembly? ResolveEmbeddedAssembly(object sender, ResolveEventArgs args)
    {
        var name = new AssemblyName(args.Name).Name;
        if (name is null || Array.IndexOf(EmbeddedAssemblyNames, name) < 0)
        {
            return null;
        }

        using var stream = Assembly.GetExecutingAssembly()
            .GetManifestResourceStream(name + ".dll");
        if (stream is null) return null;

        using var buffer = new MemoryStream();
        stream.CopyTo(buffer);
        return Assembly.Load(buffer.ToArray());
    }

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        var args = e.Args;
        var mode = args.Contains("--uninstall") ? SetupMode.Uninstall : SetupMode.Install;

        // A running exe can't delete the directory it's executing from —
        // Installer.Uninstall needs to remove all of InstallDir, including
        // this exe itself. If we were launched from inside there (the
        // normal case: UninstallString points straight at
        // InstallDir\WumaTracker.Setup.exe), re-launch a copy from %TEMP%
        // first and let *that* copy do the actual work, then exit
        // immediately without showing a window here.
        if (mode == SetupMode.Uninstall && IsRunningFromInstallDir())
        {
            RelaunchFromTempAndExit(args);
            return;
        }

        // tauri-plugin-updater always treats a bare .exe as an NSIS-style
        // installer (it can't tell it apart from any other Windows
        // installer tech) and invokes it with these exact flags for a
        // background auto-update — see WindowsUpdateInstallMode::nsis_args
        // / nsis_restart_after_install_args in tauri-apps/plugins-workspace.
        // Honoring them is what makes latest.json pointing at this exe
        // actually behave for silent/passive updates, instead of popping a
        // UI nobody asked for or failing to relaunch the app afterward.
        //   /S       — silent: no UI at all
        //   /P       — passive: progress-only UI, no interaction
        //   /UPDATE  — this run is an update, not a first install
        //   /R /ARGS <rest> — relaunch the app after install, forwarding
        //                     whatever CLI args it had before the update
        var silent = args.Contains("/S");
        var passive = args.Contains("/P");
        var restartAfterInstall = args.Contains("/R");
        var relaunchArgs = string.Join(" ", ArgsAfter(args, "/ARGS"));

        // Hidden test hook — `--simulate-error` skips the real install/
        // uninstall work and throws instead, so the error UI (2-line
        // ellipsis + "전체 텍스트 복사") can be exercised without actually
        // sabotaging a real install to trigger it.
        var simulateError = args.Contains("--simulate-error");

        if (silent)
        {
            RunHeadlessAndExit(mode, restartAfterInstall, relaunchArgs);
            return;
        }

        var window = new MainWindow(mode, simulateError, unattended: passive,
            restartAfterInstall, relaunchArgs);
        window.Show();
    }

    private static string[] ArgsAfter(string[] args, string flag)
    {
        var index = Array.IndexOf(args, flag);
        return index < 0 ? Array.Empty<string>() : args.Skip(index + 1).ToArray();
    }

    /// <summary>
    /// `/S` means no UI, period — not even the passive progress-only
    /// window. Runs the same Installer logic the window would have, just
    /// with nowhere to report progress to.
    /// </summary>
    private void RunHeadlessAndExit(SetupMode mode, bool restartAfterInstall, string relaunchArgs)
    {
        try
        {
            if (mode == SetupMode.Uninstall)
            {
                Installer.Uninstall();
            }
            else
            {
                Installer.Run();
                if (restartAfterInstall)
                {
                    Installer.LaunchApp(relaunchArgs);
                }
            }
        }
        catch (Exception ex)
        {
            // Silent mode has nowhere to surface an error to — same as a
            // silent msiexec/NSIS run, the caller (the updater) only sees
            // the process exit code, not a dialog. Log to %TEMP% so a
            // failed silent/passive auto-update is actually debuggable
            // after the fact instead of just a mysterious exit code.
            try
            {
                File.AppendAllText(
                    Path.Combine(Path.GetTempPath(), "WumaTracker.Setup.log"),
                    $"[{DateTime.Now:O}] {ex}\n");
            }
            catch
            {
                // if we can't even write the log, there's nothing left to do
            }

            // Environment.Exit, not Shutdown: no window was ever shown, so
            // WPF's dispatcher message loop never started — Shutdown()
            // only makes sense once Run() is actually pumping messages.
            Environment.Exit(1);
            return;
        }

        Environment.Exit(0);
    }

    private static bool IsRunningFromInstallDir()
    {
        var currentExePath = Installer.CurrentExePath;
        if (currentExePath is null) return false;
        return Path.GetFullPath(Path.GetDirectoryName(currentExePath) ?? "")
            .Equals(Path.GetFullPath(Installer.InstallDir), StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>
    /// Copies self to %TEMP% and re-runs the actual uninstall there,
    /// waiting for it to finish instead of firing-and-forgetting it — same
    /// shape as the NSIS `ExecWait '"$TEMP\uninst.exe" _?=$INSTDIR'` trick
    /// this replaces. Two things that version got wrong and this fixes:
    ///   1. UseShellExecute=false (plain CreateProcess) instead of
    ///      ShellExecuteEx — the child inherits this already-elevated
    ///      process's token directly, instead of ShellExecuteEx
    ///      re-evaluating the temp copy's own requireAdministrator manifest
    ///      and popping a second UAC prompt a user can cancel.
    ///   2. WaitForExit + propagate the exit code, instead of Shutdown()ing
    ///      immediately after Process.Start. Programs & Features/the
    ///      updater only consider this done once THIS process exits — if it
    ///      exits before the temp copy has actually removed the app, a
    ///      concurrent operation (e.g. a silent auto-update) can race the
    ///      temp copy's file/registry writes and leave a mismatched state
    ///      behind (see Installer.SetupMutexName for the other half of that
    ///      fix).
    /// </summary>
    private static void RelaunchFromTempAndExit(string[] args)
    {
        var currentExePath = Installer.CurrentExePath!;
        var tempExePath = Path.Combine(Path.GetTempPath(), $"WumaTracker.Setup.{Guid.NewGuid():N}.exe");
        File.Copy(currentExePath, tempExePath, overwrite: true);

        using var process = Process.Start(new ProcessStartInfo
        {
            FileName = tempExePath,
            Arguments = string.Join(" ", args),
            UseShellExecute = false,
        })!;
        process.WaitForExit();

        // Same reasoning as RunHeadlessAndExit: no window was ever shown
        // here, so WPF's dispatcher message loop never started — Shutdown()
        // only makes sense once Run() is actually pumping messages.
        Environment.Exit(process.ExitCode);
    }
}
