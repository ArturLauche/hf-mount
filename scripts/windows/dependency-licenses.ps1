<#
.SYNOPSIS
    Write per-package license lists for what a Windows build ships.

.DESCRIPTION
    rust-dependencies.txt: every crate in the dependency graph of -Manifest
    for x86_64-pc-windows-msvc, with its declared license (from
    `cargo metadata`). npm-dependencies.txt (with -NpmLock): every
    production (non-dev) package in that package-lock.json, with its license.
    Used for the portable CLI archive and the desktop installer.

.PARAMETER Manifest
    Cargo.toml of the binary being shipped.

.PARAMETER NpmLock
    package-lock.json of the bundled web UI (optional).

.PARAMETER OutDir
    Directory that receives the lists.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Manifest,
    [string]$NpmLock,
    [Parameter(Mandatory)][string]$OutDir
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$json = cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc --manifest-path $Manifest
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed for $Manifest" }
$meta = $json | ConvertFrom-Json
$root = ($meta.packages | Where-Object { $_.id -eq $meta.resolve.root } | Select-Object -First 1)
$lines = $meta.packages | Where-Object { $_.id -ne $meta.resolve.root } | Sort-Object name, version | ForEach-Object {
    $license = if ($_.license) { $_.license } elseif ($_.license_file) { "see $($_.license_file) in the crate" } else { 'unspecified' }
    '{0} {1}  {2}' -f $_.name, $_.version, $license
}
$title = "Rust crates in the dependency graph of $($root.name) $($root.version) for x86_64-pc-windows-msvc (name, version, license)."
Set-Content -LiteralPath (Join-Path $OutDir 'rust-dependencies.txt') -Encoding utf8 -Value (@($title, '') + $lines)
Write-Host "rust-dependencies.txt: $($lines.Count) crates"

if ($NpmLock) {
    $lock = Get-Content -LiteralPath $NpmLock -Raw | ConvertFrom-Json
    $npm = $lock.packages.PSObject.Properties | Where-Object {
        $_.Name -like 'node_modules/*' -and -not $_.Value.dev
    } | ForEach-Object {
        $name = $_.Name.Substring($_.Name.LastIndexOf('node_modules/') + 'node_modules/'.Length)
        $license = if ($_.Value.license) { $_.Value.license } else { 'unspecified' }
        '{0} {1}  {2}' -f $name, $_.Value.version, $license
    } | Sort-Object -Unique
    $title = "npm packages bundled into the desktop app's web UI (name, version, license)."
    Set-Content -LiteralPath (Join-Path $OutDir 'npm-dependencies.txt') -Encoding utf8 -Value (@($title, '') + $npm)
    Write-Host "npm-dependencies.txt: $($npm.Count) packages"
}
