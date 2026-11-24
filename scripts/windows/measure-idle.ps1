<#
.SYNOPSIS
    Measure an idle interval for the desktop, its WebView2 processes and the mount supervisor.
.EXAMPLE
    .\measure-idle.ps1 -DesktopProcessId 1234 -SupervisorProcessId 5678 -Seconds 600 | Format-Table
.DESCRIPTION
    Supply the PIDs for the session under test. Either component may be absent.
    Take two process snapshots without attaching to or polling the measured applications.
    CPU milliseconds are cumulative across cores. A process group that changes between
    samples has incomplete CPU accounting and gets a blank CPU value. Private bytes add
    across processes. Summed working sets can include shared pages more than once.
    A helper that starts and exits between both snapshots is absent. Use ETW to capture it.
#>
[CmdletBinding()]
param(
    [ValidateRange(0, 2147483647)][int]$DesktopProcessId,
    [ValidateRange(0, 2147483647)][int]$SupervisorProcessId,
    [ValidateRange(1, 86400)][int]$Seconds = 600
)

$ErrorActionPreference = 'Stop'
if (-not ($DesktopProcessId -or $SupervisorProcessId)) { throw 'Supply at least one process ID.' }

function Get-Snapshot {
    $byId = @{}
    $properties = 'ProcessId', 'ParentProcessId', 'Name', 'CreationDate', 'KernelModeTime', 'UserModeTime',
        'PrivatePageCount', 'WorkingSetSize', 'HandleCount', 'ThreadCount'
    foreach ($process in (Get-CimInstance Win32_Process -Property $properties)) {
        $byId[[int]$process.ProcessId] = $process
    }
    foreach ($process in $byId.Values) {
        if ($process.ProcessId -eq $DesktopProcessId -and $process.Name -eq 'hf-mount-desktop.exe') {
            $group = 'Desktop'
        } elseif ($process.ProcessId -eq $SupervisorProcessId -and $process.Name -eq 'hf-mount.exe') {
            $group = 'Supervisor'
        } elseif ($DesktopProcessId -and $process.Name -eq 'msedgewebview2.exe') {
            $ancestor = $process
            while ($ancestor -and $ancestor.ProcessId -ne $DesktopProcessId) {
                $parent = $byId[[int]$ancestor.ParentProcessId]
                if (-not $parent -or $parent.CreationDate -gt $ancestor.CreationDate -or $parent -eq $ancestor) { break }
                $ancestor = $parent
            }
            if ($ancestor.ProcessId -ne $DesktopProcessId -or $ancestor.Name -ne 'hf-mount-desktop.exe') { continue }
            $group = 'WebView2'
        } else { continue }

        [pscustomobject]@{
            Group = $group
            Key = '{0}/{1}' -f $process.ProcessId, $process.CreationDate.Ticks
            CpuMs = ([double]$process.KernelModeTime + [double]$process.UserModeTime) / 10000
            PrivateBytes = [long]$process.PrivatePageCount
            WorkingSetBytes = [long]$process.WorkingSetSize
            Handles = [int]$process.HandleCount
            Threads = [int]$process.ThreadCount
        }
    }
}

function Get-Sum($Rows, [string]$Property) {
    return [double](($Rows | Measure-Object -Property $Property -Sum).Sum)
}

$before = @(Get-Snapshot)
if ($DesktopProcessId -and 'Desktop' -notin $before.Group) { throw 'Desktop PID was not found.' }
if ($SupervisorProcessId -and 'Supervisor' -notin $before.Group) { throw 'Supervisor PID was not found.' }
$clock = [Diagnostics.Stopwatch]::StartNew()
Start-Sleep -Seconds $Seconds
$after = @(Get-Snapshot)
$clock.Stop()

foreach ($group in 'Desktop', 'WebView2', 'Supervisor', 'Total') {
    $start = @($before | Where-Object { $group -eq 'Total' -or $_.Group -eq $group })
    $end = @($after | Where-Object { $group -eq 'Total' -or $_.Group -eq $group })
    $same = (($start.Key | Sort-Object) -join ',') -eq (($end.Key | Sort-Object) -join ',')
    [pscustomobject]@{
        Group = $group
        Seconds = [math]::Round($clock.Elapsed.TotalSeconds, 2)
        ProcessesStart = $start.Count
        ProcessesEnd = $end.Count
        CpuMs = if ($same) { [math]::Round((Get-Sum $end CpuMs) - (Get-Sum $start CpuMs), 2) }
        PrivateBytesStart = [long](Get-Sum $start PrivateBytes)
        PrivateBytesEnd = [long](Get-Sum $end PrivateBytes)
        WorkingSetBytesStart = [long](Get-Sum $start WorkingSetBytes)
        WorkingSetBytesEnd = [long](Get-Sum $end WorkingSetBytes)
        HandlesStart = [int](Get-Sum $start Handles)
        HandlesEnd = [int](Get-Sum $end Handles)
        ThreadsStart = [int](Get-Sum $start Threads)
        ThreadsEnd = [int](Get-Sum $end Threads)
    }
}
