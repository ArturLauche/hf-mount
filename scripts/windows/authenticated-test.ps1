<#
.SYNOPSIS
    Opt-in end-to-end checks that need a Hugging Face token: reading a private
    repository and a write round trip through a bucket.

.DESCRIPTION
    The token is taken from the HF_TOKEN environment variable (never from the
    command line) and passed to the supervisor over its private pipe by
    hf-mount itself.

    Bucket check (when -Bucket is given): mounts the bucket read-write, writes
    files ONLY inside a new folder named hf-mount-ci-<random>, unmounts (which
    waits for the upload), mounts the bucket again with an empty cache and
    compares the bytes, then deletes that folder and nothing else. Use a
    bucket dedicated to testing.

    Private repository check (when -PrivateRepo is given): mounts it and reads
    one file.

.EXAMPLE
    $env:HF_TOKEN = '<token with write access to the test bucket>'
    ./scripts/windows/authenticated-test.ps1 -Exe hf-mount.exe -Bucket me/hf-mount-ci
#>
[CmdletBinding()]
param(
    [string]$Exe = 'hf-mount.exe',
    [string]$Bucket,
    [string]$PrivateRepo,
    [string]$PrivateFile = 'README.md'
)

$ErrorActionPreference = 'Stop'
if (-not $env:HF_TOKEN) { throw 'HF_TOKEN is not set; this test needs a token.' }
if (-not $Bucket -and -not $PrivateRepo) { throw 'nothing to test: pass -Bucket and/or -PrivateRepo' }

function HfMount([string[]]$Arguments) {
    & $Exe @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "hf-mount $($Arguments -join ' ') exited $LASTEXITCODE" }
}

function FreeLetter {
    $used = (Get-PSDrive -PSProvider FileSystem).Name
    foreach ($l in 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W') { if ($used -notcontains $l) { return "${l}:" } }
    throw 'no free drive letter'
}

if ($PrivateRepo) {
    $letter = FreeLetter
    Write-Host "== private repository $PrivateRepo at $letter"
    HfMount @('start', 'repo', $PrivateRepo, $letter)
    try {
        $bytes = [System.IO.File]::ReadAllBytes("$letter\$PrivateFile")
        if ($bytes.Length -eq 0) { throw "$PrivateFile is empty" }
        Write-Host "PASS  read $($bytes.Length) bytes of $PrivateFile"
    } finally {
        & $Exe stop $letter | Out-Host
    }
}

if ($Bucket) {
    $folder = 'hf-mount-ci-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
    $letter = FreeLetter
    $payload = [byte[]]::new(3 * 1024 * 1024 + 17)
    [System.Random]::new(42).NextBytes($payload)
    $small = "written by hf-mount CI at $(Get-Date -Format o)`n"

    Write-Host "== bucket $Bucket at $letter, test folder $folder"
    HfMount @('start', 'bucket', $Bucket, $letter)
    try {
        New-Item -ItemType Directory -Path "$letter\$folder\nested" | Out-Null
        [System.IO.File]::WriteAllBytes("$letter\$folder\nested\random.bin", $payload)
        Set-Content -LiteralPath "$letter\$folder\note.txt" -Value $small -NoNewline -Encoding utf8
        # Rename within the folder (server-side copy + delete).
        Rename-Item -LiteralPath "$letter\$folder\note.txt" -NewName 'note-renamed.txt'
        & $Exe --json status | Out-Host
    } finally {
        # Unmounting waits for the uploads and fails if one did not complete.
        HfMount @('stop', $letter)
    }

    $freshCache = Join-Path ([System.IO.Path]::GetTempPath()) "hf-mount-verify-$folder"
    Write-Host "== verify through a new mount with an empty cache ($freshCache)"
    HfMount @('start', 'bucket', $Bucket, $letter, '--cache-dir', $freshCache)
    try {
        $back = [System.IO.File]::ReadAllBytes("$letter\$folder\nested\random.bin")
        if ($back.Length -ne $payload.Length) { throw "size $($back.Length) != $($payload.Length)" }
        $sha = [System.Security.Cryptography.SHA256]::Create()
        $want = [Convert]::ToHexString($sha.ComputeHash($payload))
        $got = [Convert]::ToHexString($sha.ComputeHash($back))
        if ($got -ne $want) { throw "content differs (sha256 $got, expected $want)" }
        Write-Host 'PASS  random.bin round trip'
        $note = Get-Content -LiteralPath "$letter\$folder\note-renamed.txt" -Raw
        if ($note -ne $small) { throw "note-renamed.txt content differs: $note" }
        if (Test-Path -LiteralPath "$letter\$folder\note.txt") { throw 'the rename left the old name behind' }
        Write-Host 'PASS  rename round trip'
    } finally {
        # Remove only the folder this test created.
        if (Test-Path -LiteralPath "$letter\$folder") {
            Remove-Item -LiteralPath "$letter\$folder" -Recurse -Force
        }
        HfMount @('stop', $letter)
        Remove-Item -Recurse -Force $freshCache -ErrorAction SilentlyContinue
    }
    Write-Host 'PASS  test folder removed'
}
