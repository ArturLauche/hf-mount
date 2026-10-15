<#
.SYNOPSIS
    Download, verify and install the official WinFsp MSI.

.DESCRIPTION
    Installs WinFsp (https://github.com/winfsp/winfsp), the file system driver
    hf-mount uses for native Windows drives. The installer is downloaded from
    the official GitHub release and its SHA-256 is checked against a pinned
    value (also published in the WinFsp winget manifest) before it runs.

    Installing a kernel driver needs administrator rights: run this from an
    elevated PowerShell, or omit -Quiet to get the normal MSI wizard and UAC
    prompt. No reboot is normally required.

    WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos.

.PARAMETER Quiet
    Unattended install (msiexec /qn). Requires an elevated shell.

.PARAMETER MsiPath
    Use an already downloaded MSI instead of downloading it (still verified).
#>
[CmdletBinding()]
param(
    [switch]$Quiet,
    [string]$MsiPath
)

$ErrorActionPreference = 'Stop'

$Version = '2.1.25156'
$Url = 'https://github.com/winfsp/winfsp/releases/download/v2.1/winfsp-2.1.25156.msi'
$Sha256 = '073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a'

$existing = Get-ItemProperty -Path 'HKLM:\SOFTWARE\WOW6432Node\WinFsp' -ErrorAction SilentlyContinue
if ($existing -and $existing.InstallDir -and (Test-Path (Join-Path $existing.InstallDir 'bin\winfsp-x64.dll'))) {
    Write-Host "WinFsp is already installed at $($existing.InstallDir)"
    exit 0
}

if (-not $MsiPath) {
    $MsiPath = Join-Path ([System.IO.Path]::GetTempPath()) "winfsp-$Version.msi"
    Write-Host "Downloading WinFsp $Version from $Url"
    Invoke-WebRequest -Uri $Url -OutFile $MsiPath -UseBasicParsing
}

$actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $MsiPath).Hash.ToLowerInvariant()
if ($actual -ne $Sha256) {
    throw "WinFsp installer checksum mismatch (expected $Sha256, got $actual). Refusing to install."
}
Write-Host "Checksum OK ($actual)"

$log = Join-Path ([System.IO.Path]::GetTempPath()) 'winfsp-install.log'
$msiArgs = @('/i', "`"$MsiPath`"", '/norestart', '/l*v', "`"$log`"")
if ($Quiet) { $msiArgs += '/qn' }
$proc = Start-Process -FilePath 'msiexec.exe' -ArgumentList $msiArgs -Wait -PassThru
# 0 = success, 3010 = success but a reboot is required.
if ($proc.ExitCode -ne 0 -and $proc.ExitCode -ne 3010) {
    if (Test-Path $log) { Get-Content $log -Tail 40 }
    throw "msiexec failed with exit code $($proc.ExitCode) (log: $log)"
}
if ($proc.ExitCode -eq 3010) { Write-Warning 'WinFsp installed; Windows asks for a restart before first use.' }

$installed = Get-ItemProperty -Path 'HKLM:\SOFTWARE\WOW6432Node\WinFsp' -ErrorAction Stop
Write-Host "WinFsp installed at $($installed.InstallDir)"
