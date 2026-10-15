<#
.SYNOPSIS
    Install the desktop app from its NSIS installer, check what was
    installed, start the app, then uninstall it and check that user data
    survives. Used by CI; safe to run on a test machine.

.PARAMETER Installer
    Path to hf-mount Desktop_<version>_x64-setup.exe.
#>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Installer)

$ErrorActionPreference = 'Stop'
function Step([string]$m) { Write-Host "`n== $m" }

$dir = Join-Path $env:LOCALAPPDATA 'hf-mount Desktop'
$data = Join-Path $env:LOCALAPPDATA 'hf-mount'

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
& (Join-Path $dir 'hf-mount.exe') --version
if ($LASTEXITCODE -ne 0) { throw "hf-mount --version exited $LASTEXITCODE" }

Step "Desktop app starts (tray, window hidden) and keeps running"
$app = Start-Process -FilePath (Join-Path $dir 'hf-mount-desktop.exe') -ArgumentList '--minimized' -PassThru
Start-Sleep -Seconds 20
if ($app.HasExited) { throw "the desktop app exited early with code $($app.ExitCode)" }
$log = Join-Path $data 'logs\desktop.log'
if (-not (Test-Path -LiteralPath $log)) { throw "the desktop app did not create $log" }
Write-Host "ok  running as pid $($app.Id); log at $log"
Get-Content -LiteralPath $log -Tail 20
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
