<#
.SYNOPSIS
    Install the desktop app from its NSIS installer, check what was
    installed, start the app, then uninstall it and check that user data
    survives. Used by CI; safe to run on a test machine.

.PARAMETER Installer
    Path to hf-mount Desktop_<version>_x64-setup.exe.

.PARAMETER CliSha256
    Expected SHA-256 of the hf-mount.exe shipped in the portable archive.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Installer,
    [ValidatePattern('^[0-9A-Fa-f]{64}$')][string]$CliSha256
)

$ErrorActionPreference = 'Stop'
function Step([string]$m) { Write-Host "`n== $m" }

$dir = Join-Path $env:LOCALAPPDATA 'hf-mount Desktop'
$data = Join-Path $env:LOCALAPPDATA 'hf-mount'

# Tauri's single-instance plugin has a visible, transparent helper HWND.
# Process.MainWindowHandle can return it even before we create the main window.
Add-Type @'
using System;
using System.Runtime.InteropServices;

public static class HfMountDesktopTest {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string className, string title);
    [DllImport("user32.dll")]
    static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")]
    static extern bool IsWindowVisible(IntPtr window);

    public static bool HasMainWindow(uint expectedPid) {
        IntPtr window = IntPtr.Zero;
        while ((window = FindWindowEx(IntPtr.Zero, window, null, "hf-mount")) != IntPtr.Zero) {
            uint pid;
            GetWindowThreadProcessId(window, out pid);
            if (pid == expectedPid && IsWindowVisible(window)) return true;
        }
        return false;
    }
}
'@

Step "Silent per-user install"
$p = Start-Process -FilePath $Installer -ArgumentList '/S' -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "installer exited with $($p.ExitCode)" }
foreach ($f in 'hf-mount-desktop.exe', 'hf-mount.exe', 'uninstall.exe',
    'winfsp\winfsp-2.1.25156.msi', 'licenses\WinFsp-License.txt', 'licenses\hf-mount-LICENSE.txt',
    'licenses\THIRD_PARTY_NOTICES.md', 'licenses\rust-dependencies.txt', 'licenses\npm-dependencies.txt') {
    $path = Join-Path $dir $f
    if (-not (Test-Path -LiteralPath $path)) { throw "not installed: $path" }
    Write-Host "ok  $f"
}

Step "Bundled WinFsp installer is the pinned, unmodified MSI"
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dir 'winfsp\winfsp-2.1.25156.msi')).Hash.ToLowerInvariant()
if ($hash -ne '073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a') { throw "unexpected WinFsp MSI hash $hash" }
Write-Host "ok  $hash"

Step "Installed CLI runs"
$cli = Join-Path $dir 'hf-mount.exe'
if ($CliSha256 -and (Get-FileHash -Algorithm SHA256 -LiteralPath $cli).Hash -ne $CliSha256) {
    throw 'Installed CLI does not match the portable CLI from this build'
}
& $cli --version
if ($LASTEXITCODE -ne 0) { throw "hf-mount --version exited $LASTEXITCODE" }

Step "Desktop app starts (tray, window hidden) and keeps running"
$app = Start-Process -FilePath (Join-Path $dir 'hf-mount-desktop.exe') -ArgumentList '--minimized' -PassThru
Start-Sleep -Seconds 20
if ($app.HasExited) { throw "the desktop app exited early with code $($app.ExitCode)" }
$log = Join-Path $data 'logs\desktop.log'
if (-not (Test-Path -LiteralPath $log)) { throw "the desktop app did not create $log" }
Write-Host "ok  running as pid $($app.Id); log at $log"
Get-Content -LiteralPath $log -Tail 20
$webviewFilter = "ParentProcessId = $($app.Id) AND Name = 'msedgewebview2.exe'"
if ([HfMountDesktopTest]::HasMainWindow($app.Id)) { throw 'tray-only launch opened the main window' }
if (Get-CimInstance Win32_Process -Filter $webviewFilter) { throw 'tray-only launch started WebView2' }

Step "A repeated minimized launch keeps the existing instance in the tray"
$second = Start-Process -FilePath (Join-Path $dir 'hf-mount-desktop.exe') -ArgumentList '--minimized' -PassThru
if (-not $second.WaitForExit(15000) -or $second.ExitCode -ne 0) { throw 'second minimized launch did not exit cleanly' }
# Check long enough to catch asynchronous window creation after the second process exits.
$deadline = [DateTime]::UtcNow.AddSeconds(2)
do {
    $app.Refresh()
    if ($app.HasExited) { throw 'the first desktop process exited after a second minimized launch' }
    if ([HfMountDesktopTest]::HasMainWindow($app.Id)) { throw 'a second minimized launch opened the main window' }
    if (Get-CimInstance Win32_Process -Filter $webviewFilter) { throw 'a second minimized launch started WebView2' }
    Start-Sleep -Milliseconds 250
} while ([DateTime]::UtcNow -lt $deadline)

Step "Opening the installed app creates its first window and WebView2"
Start-Process -FilePath (Join-Path $dir 'hf-mount-desktop.exe')
$deadline = [DateTime]::UtcNow.AddSeconds(45)
do {
    $app.Refresh()
    if ($app.HasExited) { throw 'the tray app exited during restore' }
    $opened = [HfMountDesktopTest]::HasMainWindow($app.Id) -and (Get-CimInstance Win32_Process -Filter $webviewFilter)
    if ($opened) { break }
    Start-Sleep -Milliseconds 250
} while ([DateTime]::UtcNow -lt $deadline)
if (-not $opened) { throw 'the running tray instance did not open its window and WebView2' }
Stop-Process -Id $app.Id -Force
Start-Sleep -Seconds 2

Step "Silent uninstall keeps settings, logs and cache"
New-Item -ItemType Directory -Force -Path $data | Out-Null
$marker = Join-Path $data 'uninstall-test-marker.txt'
Set-Content -LiteralPath $marker -Value 'user data'
$p = Start-Process -FilePath (Join-Path $dir 'uninstall.exe') -ArgumentList '/S', "_?=$dir" -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "uninstaller exited with $($p.ExitCode)" }
if (Test-Path -LiteralPath (Join-Path $dir 'hf-mount-desktop.exe')) { throw 'the app was not removed' }
if (-not (Test-Path -LiteralPath $marker)) { throw 'the uninstaller removed user data' }
Remove-Item -LiteralPath $marker
Write-Host "ok  app removed; $data kept"
