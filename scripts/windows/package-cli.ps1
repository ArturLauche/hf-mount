<#
.SYNOPSIS
    Build the portable command-line archive hf-mount-<version>-windows-x64.zip.

.DESCRIPTION
    Contents: hf-mount.exe, a short README, the hf-mount license, the WinFsp
    license and notice, third-party notices, the list of Rust dependencies
    with their licenses, and install-winfsp.ps1. WinFsp itself is not in the
    archive; install-winfsp.ps1 downloads the official, checksum-verified MSI.

.PARAMETER Exe
    Path to the release hf-mount.exe.

.PARAMETER OutDir
    Directory that receives the zip.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$OutDir
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$version = (Select-String -LiteralPath (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
$name = "hf-mount-$version-windows-x64"
$stage = Join-Path ([System.IO.Path]::GetTempPath()) $name
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $stage 'licenses') | Out-Null

Copy-Item -LiteralPath $Exe -Destination (Join-Path $stage 'hf-mount.exe')
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $stage 'LICENSE.txt')
Copy-Item -LiteralPath (Join-Path $root 'licenses\WinFsp-License.txt') -Destination (Join-Path $stage 'licenses')
Copy-Item -LiteralPath (Join-Path $root 'THIRD_PARTY_NOTICES.md') -Destination (Join-Path $stage 'licenses')
Copy-Item -LiteralPath (Join-Path $root 'scripts\windows\install-winfsp.ps1') -Destination $stage

# Every crate compiled into hf-mount.exe for this platform, with its license.
$meta = cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc `
    --manifest-path (Join-Path $root 'Cargo.toml') | ConvertFrom-Json
$lines = $meta.packages | Sort-Object name, version | ForEach-Object {
    $license = if ($_.license) { $_.license } elseif ($_.license_file) { "see $($_.license_file)" } else { 'unspecified' }
    '{0} {1}  {2}' -f $_.name, $_.version, $license
}
Set-Content -LiteralPath (Join-Path $stage 'licenses\rust-dependencies.txt') -Encoding utf8 -Value (
    @("Rust crates in the hf-mount $version dependency graph for x86_64-pc-windows-msvc (name, version, license).", '') + $lines)

$readme = @"
hf-mount $version for Windows (x64) - portable command line
=============================================================

Mount Hugging Face models, datasets and buckets as Windows drive letters.

1. Install WinFsp once (needs administrator approval):
     powershell -ExecutionPolicy Bypass -File .\install-winfsp.ps1
   or: winget install WinFsp.WinFsp

2. Mount and use:
     hf-mount start repo openai-community/gpt2 H:
     dir H:\
     hf-mount status
     hf-mount stop H:

   Private repositories and buckets: hf-mount auth login
   Help: hf-mount --help, hf-mount doctor

This build is not code-signed; Windows SmartScreen may ask for confirmation.

WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos
https://github.com/winfsp/winfsp (see licenses\WinFsp-License.txt)
hf-mount is licensed under the Apache License 2.0 (LICENSE.txt).
"@
Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding utf8 -Value $readme

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$zip = Join-Path (Resolve-Path $OutDir) "$name.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Write-Host "Created $zip"
Get-ChildItem -Recurse $stage | Select-Object -ExpandProperty FullName
