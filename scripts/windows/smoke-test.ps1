<#
.SYNOPSIS
    End-to-end check of hf-mount on a real Windows machine.

.DESCRIPTION
    Mounts a public Hugging Face repository on a drive letter through the
    hf-mount CLI and background supervisor, then verifies with ordinary
    Windows tools that:
      * the drive appears and lists the repository,
      * file bytes equal the Hub's own download (whole small file, and byte
        ranges of a large file, including one hundreds of MB into it),
      * the repository was NOT downloaded in full (cache stays small),
      * writes to a repository mount are refused,
      * `hf-mount status` reports the mount, `hf-mount stop` removes it and
        the letter can be mounted again.

    Requires WinFsp (scripts/windows/install-winfsp.ps1) and network access.
    Exit code 0 means every check passed; anything else prints the failing
    check. Used by CI and suitable for manual validation.

.EXAMPLE
    ./scripts/windows/smoke-test.ps1 -Exe "C:\Program Files\hf-mount\hf-mount.exe"
#>
[CmdletBinding()]
param(
    [string]$Exe = 'hf-mount.exe',
    [string]$Repo = 'openai-community/gpt2',
    [string]$SmallFile = 'config.json',
    [string]$LargeFile = 'model.safetensors',
    [string]$Letter
)

$ErrorActionPreference = 'Stop'
$script:failures = 0

function Check([string]$Name, [scriptblock]$Test) {
    try {
        $result = & $Test
        if ($result -eq $false) { throw 'check returned false' }
        Write-Host "PASS  $Name"
    } catch {
        $script:failures++
        Write-Host "FAIL  $Name — $($_.Exception.Message)" -ForegroundColor Red
    }
}

function HfMount([string[]]$Arguments) {
    $out = & $Exe @Arguments 2>&1
    [pscustomobject]@{ Code = $LASTEXITCODE; Out = ($out -join "`n") }
}

function Sha256Hex([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    ([BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-', '')
}

$http = [System.Net.Http.HttpClient]::new()
$http.DefaultRequestHeaders.UserAgent.ParseAdd('hf-mount-smoke-test')
function HubRange([string]$Path, [long]$Start, [long]$Length) {
    $url = "https://huggingface.co/$Repo/resolve/main/$Path"
    $req = [System.Net.Http.HttpRequestMessage]::new('GET', $url)
    if ($Length -gt 0) { $req.Headers.Range = [System.Net.Http.Headers.RangeHeaderValue]::new($Start, $Start + $Length - 1) }
    $resp = $http.SendAsync($req).GetAwaiter().GetResult()
    $resp.EnsureSuccessStatusCode() | Out-Null
    $resp.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
}

function DriveRange([string]$Path, [long]$Start, [int]$Length) {
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $fs.Seek($Start, 'Begin') | Out-Null
        $buf = New-Object byte[] $Length
        $read = 0
        while ($read -lt $Length) {
            $n = $fs.Read($buf, $read, $Length - $read)
            if ($n -eq 0) { break }
            $read += $n
        }
        if ($read -ne $Length) { throw "short read: $read of $Length bytes" }
        $buf
    } finally { $fs.Dispose() }
}

Write-Host (& $Exe --version | Select-Object -First 1)

$doctor = HfMount @('--json', 'doctor')
Check 'WinFsp is installed (hf-mount doctor)' { ($doctor.Out | ConvertFrom-Json).winfsp.installed }

if (-not $Letter) {
    $free = (HfMount @('--json', 'drives')).Out | ConvertFrom-Json
    $Letter = ($free.free | Select-Object -Last 1).TrimEnd(':')
}
$Drive = "${Letter}:"
$Root = "${Letter}:\"
Write-Host "Using drive $Drive for $Repo"

try {
    $start = HfMount @('--json', 'start', 'repo', $Repo, $Drive)
    Check "hf-mount start exits 0" { $start.Code -eq 0 }
    if ($start.Code -ne 0) { Write-Host $start.Out; throw 'mount failed; remaining checks skipped' }
    $mount = ($start.Out | ConvertFrom-Json).mount
    Check 'mount is reported read-only' { $mount.spec.read_only -eq $true }

    Check "drive $Root exists" { Test-Path $Root }

    $listing = Get-ChildItem -LiteralPath $Root
    Check "Get-ChildItem lists $SmallFile and $LargeFile" {
        ($listing.Name -contains $SmallFile) -and ($listing.Name -contains $LargeFile)
    }
    $cmdDir = cmd /c "dir /b $Root"
    Check 'cmd dir lists the repository' { $cmdDir -contains $SmallFile }

    $hubSmall = HubRange $SmallFile 0 0
    $driveSmall = [System.IO.File]::ReadAllBytes((Join-Path $Root $SmallFile))
    Check "$SmallFile bytes equal the Hub download ($($hubSmall.Length) bytes)" {
        (Sha256Hex $driveSmall) -eq (Sha256Hex $hubSmall)
    }

    $large = Get-Item -LiteralPath (Join-Path $Root $LargeFile)
    Check "$LargeFile has a size ($($large.Length) bytes)" { $large.Length -gt 100MB }
    foreach ($offset in @(0, 123457, [long]($large.Length * 0.75))) {
        $len = 1MB
        $driveBytes = DriveRange $large.FullName $offset $len
        $hubBytes = HubRange $LargeFile $offset $len
        Check "$LargeFile bytes at offset $offset equal the Hub range" {
            (Sha256Hex $driveBytes) -eq (Sha256Hex $hubBytes)
        }
    }

    $repoBytes = ($listing | Where-Object { -not $_.PSIsContainer } | Measure-Object Length -Sum).Sum
    $cacheRoot = Join-Path $env:LOCALAPPDATA 'hf-mount\cache'
    $cacheKey = 'models--' + $Repo.Replace('/', '--')
    $cacheBytes = (Get-ChildItem -Path (Join-Path $cacheRoot "$cacheKey*") -Recurse -File -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
    if (-not $cacheBytes) { $cacheBytes = 0 }
    Write-Host ("Repository top-level files: {0:N0} MB; local cache: {1:N0} MB" -f ($repoBytes / 1MB), ($cacheBytes / 1MB))
    Check 'repository was not downloaded in full (cache < 25% of repo size)' { $cacheBytes -lt ($repoBytes * 0.25) }

    $writeFailed = $false
    try { Set-Content -LiteralPath (Join-Path $Root 'should-fail.txt') -Value 'x' -ErrorAction Stop } catch { $writeFailed = $true }
    Check 'writes to a repository mount are refused' { $writeFailed -and -not (Test-Path (Join-Path $Root 'should-fail.txt')) }

    $status = HfMount @('--json', 'status')
    $mounts = ($status.Out | ConvertFrom-Json).mounts
    Check 'hf-mount status shows the mount as mounted' {
        ($mounts | Where-Object { $_.spec.target -eq $Drive -and $_.state -eq 'mounted' }).Count -eq 1
    }

    $dup = HfMount @('--json', 'start', 'repo', $Repo, $Drive)
    Check 'mounting the same letter again is refused (exit 6)' { $dup.Code -eq 6 }

    $stop = HfMount @('--json', 'stop', $Drive)
    Check 'hf-mount stop exits 0' { $stop.Code -eq 0 }
    Start-Sleep -Milliseconds 500
    Check "drive $Root is gone after stop" { -not (Test-Path $Root) }

    $again = HfMount @('--json', 'start', 'repo', $Repo, $Drive)
    Check 're-mounting the same letter works' { $again.Code -eq 0 -and (Test-Path (Join-Path $Root $SmallFile)) }
    $stop2 = HfMount @('--json', 'stop', $Drive)
    Check 'second stop exits 0' { $stop2.Code -eq 0 }

    $missing = HfMount @('--json', 'stop', $Drive)
    Check 'stopping an unmounted letter exits 9' { $missing.Code -eq 9 }
} finally {
    & $Exe stop --all --force *> $null
}

if ($script:failures -gt 0) {
    Write-Host "$script:failures check(s) failed" -ForegroundColor Red
    exit 1
}
Write-Host 'All checks passed'
