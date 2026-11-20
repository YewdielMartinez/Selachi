<#
.SYNOPSIS
    Measures startup time, memory and idle CPU of a GUI application (and its whole process tree).

.DESCRIPTION
    Part of the Selachi benchmark harness (docs/PLAN.md section 10, Fase 0).

    For each run the script:
      1. Snapshots the running PIDs, starts the target and starts a stopwatch.
      2. Startup time = launch -> first visible, un-cloaked, top-level window owned by any process of
         the tracked tree (EnumWindows by PID; no dependency on MainWindowHandle).
      3. Waits a settle period (default 5 s) so child processes (WebView2, OpenConsole, the shell...) appear.
      4. Samples the tracked tree once per second for -IdleSeconds and reports
           - Working Set and Private Bytes, TOTAL tree and APP-ONLY (shell processes and their
             descendants are excluded so that different terminals are compared fairly),
           - average idle CPU % (normalised to one core and to all logical cores).
         Memory numbers are the MEDIAN over the idle samples.
      5. Stops ONLY the processes it started (see SAFETY) and appends one CSV row.
    Finally it prints a summary table with the median over all runs.

    PROCESS TRACKING
      The tracked set = the PID returned by Start-Process + every descendant (Win32_Process
      ParentProcessId, polled repeatedly because children appear late) + processes whose image name
      is in the "adopt list" that did NOT exist before launch and appeared during the launch phase.
      The adopt list handles launchers that re-parent or hand off to another process:
        wt.exe    -> WindowsTerminal.exe       warp.exe -> warp.exe
        notepad.exe -> notepad.exe (store app launcher)
      Extra names can be given with -AdoptName; -SingleInstance adds the exe's own image name.

    SAFETY
      * Processes are only ever stopped by PID, and only PIDs that the script itself started or
        discovered as descendants/adopted-new processes (PID + start-time validated against PID reuse).
        Stop-Process is NEVER used by name.
      * If an instance of a single-instance target (the adopt list, e.g. WindowsTerminal.exe, warp.exe)
        is already running before the run, it is not touched: a message asks you to close it and the
        run is skipped.
      * Residual risk: adoption-by-name is only active for the launch phase (until the window is seen
        + 1.5 s). If you manually open the same application during that window, it could be adopted.

.PARAMETER Exe
    Path to an executable or an app execution alias / command resolvable by Get-Command (wt.exe, notepad.exe).

.PARAMETER ArgumentList
    Arguments for the target (alias: -Args).

.PARAMETER Name
    Label used in CSV and the summary table. Default: exe base name.

.PARAMETER IdleSeconds
    Length of the idle sampling window. Default 30.

.PARAMETER Runs
    Number of independent runs (launch -> measure -> kill). Default 3.

.PARAMETER OutCsv
    CSV file to append to. Default: bench/results/results.csv (next to this script).

.PARAMETER SettleSeconds
    Wait between "window visible" and the start of idle sampling. Default 5.

.PARAMETER StartupTimeoutSeconds
    Max wait for the first visible window. Default 30.

.PARAMETER SampleIntervalSeconds
    Sampling period during the idle window. Default 1.

.PARAMETER ShellNames
    Image names considered "the shell" (excluded, with their descendants, from the app-only numbers).

.PARAMETER AdoptName
    Additional image names to adopt if they appear after launch (launchers that re-parent).

.PARAMETER SingleInstance
    Treat the target as single-instance: refuse to run if its image name is already running.

.PARAMETER PassThru
    Emit the summary (median) object on the pipeline (used by compare.ps1).

.EXAMPLE
    ./bench/measure.ps1 -Exe target/release/selachi.exe -Name selachi -IdleSeconds 30 -Runs 3

.EXAMPLE
    ./bench/measure.ps1 -Exe wt.exe -Name wt -Runs 1 -IdleSeconds 10 -Verbose
#>
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory, Position = 0)][string]$Exe,
    [Alias('Args')][string[]]$ArgumentList = @(),
    [string]$Name,
    [ValidateRange(1, 3600)][int]$IdleSeconds = 30,
    [ValidateRange(1, 100)][int]$Runs = 3,
    [string]$OutCsv,
    [ValidateRange(0, 600)][int]$SettleSeconds = 5,
    [ValidateRange(1, 600)][int]$StartupTimeoutSeconds = 30,
    [ValidateRange(0.2, 60)][double]$SampleIntervalSeconds = 1,
    [string[]]$ShellNames = @('pwsh.exe', 'powershell.exe', 'cmd.exe', 'bash.exe', 'wsl.exe', 'wslhost.exe',
        'nu.exe', 'fish.exe', 'zsh.exe', 'sh.exe'),
    [string[]]$AdoptName = @(),
    [switch]$SingleInstance,
    [switch]$PassThru
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$inv = [System.Globalization.CultureInfo]::InvariantCulture

if (-not $OutCsv) { $OutCsv = Join-Path $PSScriptRoot 'results/results.csv' }

# ---------------------------------------------------------------------------------------------
# Win32 helper: PIDs owning at least one visible, un-cloaked, top-level (un-owned) window.
# ---------------------------------------------------------------------------------------------
if (-not ('BenchWin' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public static class BenchWin
{
    private delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] private static extern int GetWindowLong(IntPtr hWnd, int idx);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("dwmapi.dll")] private static extern int DwmGetWindowAttribute(IntPtr hWnd, int attr, out int val, int size);

    [StructLayout(LayoutKind.Sequential)] private struct RECT { public int L, T, R, B; }

    public static uint[] VisibleTopLevelPids()
    {
        var set = new HashSet<uint>();
        EnumWindows((h, l) =>
        {
            if (!IsWindowVisible(h)) return true;
            if (GetWindow(h, 4 /*GW_OWNER*/) != IntPtr.Zero) return true;
            int ex = GetWindowLong(h, -20 /*GWL_EXSTYLE*/);
            bool tool = (ex & 0x80) != 0, app = (ex & 0x40000) != 0;
            if (tool && !app) return true;
            int cloaked;
            if (DwmGetWindowAttribute(h, 13 /*DWMWA_CLOAKED*/, out cloaked, 4) == 0 && cloaked != 0) return true;
            RECT r;
            if (!GetWindowRect(h, out r) || r.R <= r.L || r.B <= r.T) return true;
            uint pid; GetWindowThreadProcessId(h, out pid);
            set.Add(pid);
            return true;
        }, IntPtr.Zero);
        var a = new uint[set.Count]; set.CopyTo(a); return a;
    }
}
'@
}

# ---------------------------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------------------------
function Get-Median {
    param($Values)
    $v = @($Values | Where-Object { $null -ne $_ } | Sort-Object)
    if ($v.Count -eq 0) { return $null }
    $m = [int][math]::Floor($v.Count / 2)
    if ($v.Count % 2 -eq 1) { return [double]$v[$m] }
    return ([double]$v[$m - 1] + [double]$v[$m]) / 2
}

# Sum of a numeric row property, optionally excluding shell rows.
function Get-Sum($Rows, [string]$Prop, [bool]$AppOnly) {
    $t = 0.0
    foreach ($r in $Rows) { if (-not ($AppOnly -and $r.Shell)) { $t += [double]$r.$Prop } }
    return $t
}

function Format-Num($v, [string]$fmt = '0.0') {
    if ($null -eq $v) { return '' }
    return ([double]$v).ToString($fmt, $inv)
}

# One CIM query gives tree structure, memory and CPU times for every process.
function Get-ProcTable {
    Get-CimInstance -ClassName Win32_Process -Property ProcessId, ParentProcessId, Name, CreationDate,
        WorkingSetSize, PrivatePageCount, KernelModeTime, UserModeTime -Verbose:$false
}

# Tracked set: pid -> creation time (DateTime). Script-scoped, reset every run.
$script:Tracked = @{}
$script:Before = [System.Collections.Generic.HashSet[int]]::new()
$script:LaunchTime = [datetime]::Now
$script:AdoptSet = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$script:AdoptActive = $false

function Update-Tracked {
    param($Table)
    $tol = 2.0
    # Drop entries whose PID was reused by an unrelated process.
    foreach ($row in $Table) {
        $procId = [int]$row.ProcessId
        if ($script:Tracked.ContainsKey($procId) -and
            [math]::Abs(($row.CreationDate - $script:Tracked[$procId]).TotalSeconds) -gt $tol) {
            $script:Tracked.Remove($procId)
        }
    }
    do {
        $added = $false
        foreach ($row in $Table) {
            $procId = [int]$row.ProcessId
            if ($script:Tracked.ContainsKey($procId)) { continue }
            $ppid = [int]$row.ParentProcessId
            $take = $false
            if ($script:Tracked.ContainsKey($ppid) -and $row.CreationDate -ge $script:Tracked[$ppid].AddSeconds(-$tol)) {
                $take = $true     # descendant of something we track
            }
            elseif ($script:AdoptActive -and $script:AdoptSet.Contains([string]$row.Name) -and
                -not $script:Before.Contains($procId) -and $row.CreationDate -ge $script:LaunchTime.AddSeconds(-1)) {
                $take = $true     # new process of an adopted image name (re-parented launcher)
            }
            if ($take) { $script:Tracked[$procId] = $row.CreationDate; $added = $true }
        }
    } while ($added)
}

# One measurement sample of the tracked tree.
function Get-TreeSample {
    param([int]$RootPid, $ShellSet, $Stopwatch)
    $table = Get-ProcTable
    Update-Tracked $table
    $live = @{}
    foreach ($row in $table) {
        if ($script:Tracked.ContainsKey([int]$row.ProcessId)) { $live[[int]$row.ProcessId] = $row }
    }
    $rows = foreach ($procId in $live.Keys) {
        $r = $live[$procId]
        # Shell = a shell image or anything below one (within the tracked tree). The root is never a "shell".
        $isShell = $false
        $cur = $procId; $guard = 0
        while ($live.ContainsKey($cur) -and $guard++ -lt 64) {
            if ($cur -ne $RootPid -and $ShellSet.Contains([string]$live[$cur].Name)) { $isShell = $true; break }
            $cur = [int]$live[$cur].ParentProcessId
        }
        [pscustomobject]@{
            Pid     = $procId
            Name    = [string]$r.Name
            Parent  = [int]$r.ParentProcessId
            Shell   = $isShell
            WS      = [double]$r.WorkingSetSize
            Private = [double]$r.PrivatePageCount
            Cpu     = ([double]([uint64]$r.KernelModeTime + [uint64]$r.UserModeTime)) / 1e7   # seconds
        }
    }
    [pscustomobject]@{ T = $Stopwatch.Elapsed.TotalSeconds; Rows = @($rows) }
}

# Returns the live Process for a tracked PID, or $null if gone / PID reused.
function Get-TrackedProcess([int]$ProcId) {
    $p = Get-Process -Id $ProcId -ErrorAction SilentlyContinue
    if (-not $p) { return $null }
    try {
        if ([math]::Abs(($p.StartTime - $script:Tracked[$ProcId]).TotalSeconds) -gt 2.0) { return $null }
    } catch { return $null }
    return $p
}

# Stops only tracked PIDs: polite WM_CLOSE first, then Stop-Process -Id for survivors.
function Stop-Tracked {
    try { Update-Tracked (Get-ProcTable) } catch { }
    $script:AdoptActive = $false
    foreach ($procId in @($script:Tracked.Keys)) {
        $p = Get-TrackedProcess $procId
        if ($p) { try { if ($p.MainWindowHandle -ne 0) { [void]$p.CloseMainWindow() } } catch { } }
    }
    $deadline = [datetime]::Now.AddSeconds(3)
    while ([datetime]::Now -lt $deadline -and
        @($script:Tracked.Keys | Where-Object { Get-TrackedProcess $_ }).Count -gt 0) { Start-Sleep -Milliseconds 100 }
    foreach ($procId in @($script:Tracked.Keys)) {
        if (Get-TrackedProcess $procId) { Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue }
    }
    $deadline = [datetime]::Now.AddSeconds(5)
    while ([datetime]::Now -lt $deadline -and
        @($script:Tracked.Keys | Where-Object { Get-TrackedProcess $_ }).Count -gt 0) { Start-Sleep -Milliseconds 100 }
    $left = @($script:Tracked.Keys | Where-Object { Get-TrackedProcess $_ })
    if ($left.Count -gt 0) { Write-Warning "Could not stop PIDs started by this script: $($left -join ', ')" }
}

# ---------------------------------------------------------------------------------------------
# Resolve target, derive adopt list
# ---------------------------------------------------------------------------------------------
$resolved = $null
if (Test-Path -LiteralPath $Exe -PathType Leaf) { $resolved = (Resolve-Path -LiteralPath $Exe).Path }
else {
    $cmd = Get-Command $Exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd) { $resolved = $cmd.Source }
}
if (-not $resolved) { throw "Executable not found: $Exe" }

$exeLeaf = [IO.Path]::GetFileName($resolved)              # e.g. wt.exe
$exeBase = [IO.Path]::GetFileNameWithoutExtension($resolved)
if (-not $Name) { $Name = $exeBase }

# Known launchers whose real process has a different image name (or that are single instance).
$knownLaunchers = @{
    'wt.exe'      = @('WindowsTerminal.exe')
    'wtd.exe'     = @('WindowsTerminalPreview.exe')
    'warp.exe'    = @('warp.exe')
    'notepad.exe' = @('notepad.exe')
}
$adopt = [System.Collections.Generic.List[string]]::new()
if ($knownLaunchers.ContainsKey($exeLeaf.ToLowerInvariant())) { $adopt.AddRange([string[]]$knownLaunchers[$exeLeaf.ToLowerInvariant()]) }
if ($SingleInstance) { $adopt.Add($exeLeaf) }
foreach ($a in $AdoptName) { $adopt.Add($(if ($a -notmatch '\.exe$') { "$a.exe" } else { $a })) }
foreach ($a in ($adopt | Select-Object -Unique)) { [void]$script:AdoptSet.Add($a) }
$adoptBase = @($script:AdoptSet | ForEach-Object { [IO.Path]::GetFileNameWithoutExtension($_) })
$shellSet = [System.Collections.Generic.HashSet[string]]::new([string[]]$ShellNames, [StringComparer]::OrdinalIgnoreCase)
$logical = [Environment]::ProcessorCount

Write-Host "== $Name : $resolved $($ArgumentList -join ' ')" -ForegroundColor Cyan
Write-Host ("   runs={0} idle={1}s settle={2}s adopt=[{3}]" -f $Runs, $IdleSeconds, $SettleSeconds, ($script:AdoptSet -join ', '))

# ---------------------------------------------------------------------------------------------
# Runs
# ---------------------------------------------------------------------------------------------
$results = [System.Collections.Generic.List[object]]::new()

for ($run = 1; $run -le $Runs; $run++) {
    # SAFETY: never touch a pre-existing single-instance process.
    if ($adoptBase.Count -gt 0) {
        $existing = @(Get-Process -Name $adoptBase -ErrorAction SilentlyContinue)
        if ($existing.Count -gt 0) {
            Write-Warning ("'{0}' is already running (PID {1}). It is NOT touched. Close it and re-run; skipping '{2}'." -f
                ($existing[0].ProcessName + '.exe'), (($existing | ForEach-Object Id) -join ', '), $Name)
            break
        }
    }

    Write-Host ("-- run {0}/{1}" -f $run, $Runs)
    $script:Tracked = @{}
    $script:Before = [System.Collections.Generic.HashSet[int]]::new()
    Get-Process | ForEach-Object { [void]$script:Before.Add($_.Id) }
    $startupMs = $null
    $samples = [System.Collections.Generic.List[object]]::new()
    $failed = $null

    try {
        $splat = @{ FilePath = $resolved; PassThru = $true }
        if ($ArgumentList.Count -gt 0) { $splat.ArgumentList = $ArgumentList }
        $script:LaunchTime = [datetime]::Now
        $script:AdoptActive = ($script:AdoptSet.Count -gt 0)
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $proc = Start-Process @splat
        $rootPid = $proc.Id
        $rootStart = try { $proc.StartTime } catch { $script:LaunchTime }
        $script:Tracked[$rootPid] = $rootStart

        # --- startup: poll for first visible window owned by the tracked tree ---
        $lastFull = -1000
        while ($sw.ElapsedMilliseconds -lt $StartupTimeoutSeconds * 1000) {
            if ($adoptBase.Count -gt 0) {     # cheap adoption path (no CIM) to keep latency low
                foreach ($p in @(Get-Process -Name $adoptBase -ErrorAction SilentlyContinue)) {
                    if ($script:Before.Contains($p.Id) -or $script:Tracked.ContainsKey($p.Id)) { continue }
                    try { if ($p.StartTime -ge $script:LaunchTime.AddSeconds(-1)) { $script:Tracked[$p.Id] = $p.StartTime } } catch { }
                }
            }
            # The CIM process-table query takes 100-300 ms and blocks this loop, which used to add up to
            # that much to startup_ms. During the first 1.5 s only the cheap paths run (root PID + adopt-by-name);
            # CIM tree discovery kicks in afterwards for launchers whose window belongs to a descendant.
            if ($sw.ElapsedMilliseconds -gt 1500 -and $sw.ElapsedMilliseconds - $lastFull -ge 300) {
                Update-Tracked (Get-ProcTable); $lastFull = $sw.ElapsedMilliseconds
            }
            foreach ($wp in [BenchWin]::VisibleTopLevelPids()) {
                if ($script:Tracked.ContainsKey([int]$wp)) { $startupMs = $sw.Elapsed.TotalMilliseconds; break }
            }
            if ($null -ne $startupMs) { break }
            Start-Sleep -Milliseconds 5
        }
        if ($null -eq $startupMs) { Write-Warning "No visible window within ${StartupTimeoutSeconds}s (startup_ms left empty)." }
        else { Write-Host ("   startup: {0:N0} ms" -f $startupMs) }

        # Grace period: keep adopting launcher hand-offs / children for 1.5 s, then freeze adoption.
        $graceEnd = $sw.ElapsedMilliseconds + 1500
        while ($sw.ElapsedMilliseconds -lt $graceEnd) {
            Update-Tracked (Get-ProcTable); Start-Sleep -Milliseconds 250
        }
        $script:AdoptActive = $false
        if (@($script:Tracked.Keys | Where-Object { Get-TrackedProcess $_ }).Count -eq 0) {
            throw 'Target process tree is not running (launcher exited without a tracked process).'
        }

        # --- settle ---
        $settleEnd = [datetime]::Now.AddSeconds($SettleSeconds)
        while ([datetime]::Now -lt $settleEnd) { Update-Tracked (Get-ProcTable); Start-Sleep -Milliseconds 500 }

        # --- idle sampling ---
        $idle = [System.Diagnostics.Stopwatch]::StartNew()
        $k = 0
        while ($true) {
            $samples.Add((Get-TreeSample -RootPid $rootPid -ShellSet $shellSet -Stopwatch $idle))
            $k++
            if ($idle.Elapsed.TotalSeconds -ge $IdleSeconds) { break }
            $wait = ($k * $SampleIntervalSeconds) - $idle.Elapsed.TotalSeconds
            if ($wait -gt 0) { Start-Sleep -Milliseconds ([int]($wait * 1000)) }
        }
    }
    catch { $failed = $_.Exception.Message; Write-Warning "Run $run failed: $failed" }
    finally { Stop-Tracked }

    if ($failed -or $samples.Count -lt 1) { continue }

    # --- aggregate ---
    $mb = 1MB
    $wsTotal = Get-Median ($samples | ForEach-Object { (Get-Sum $_.Rows 'WS' $false) / $mb })
    $prTotal = Get-Median ($samples | ForEach-Object { (Get-Sum $_.Rows 'Private' $false) / $mb })
    $wsApp = Get-Median ($samples | ForEach-Object { (Get-Sum $_.Rows 'WS' $true) / $mb })
    $prApp = Get-Median ($samples | ForEach-Object { (Get-Sum $_.Rows 'Private' $true) / $mb })

    # CPU: per-PID deltas between consecutive samples (new PIDs count their full CPU time).
    $cpuTot = 0.0; $cpuApp = 0.0
    for ($i = 1; $i -lt $samples.Count; $i++) {
        $prev = @{}; foreach ($r in $samples[$i - 1].Rows) { $prev[$r.Pid] = $r.Cpu }
        foreach ($r in $samples[$i].Rows) {
            $d = if ($prev.ContainsKey($r.Pid)) { [math]::Max(0.0, $r.Cpu - $prev[$r.Pid]) } else { $r.Cpu }
            $cpuTot += $d
            if (-not $r.Shell) { $cpuApp += $d }
        }
    }
    $elapsed = $samples[$samples.Count - 1].T - $samples[0].T
    $cpuOne = if ($elapsed -gt 0) { 100.0 * $cpuTot / $elapsed } else { $null }
    $cpuAppOne = if ($elapsed -gt 0) { 100.0 * $cpuApp / $elapsed } else { $null }
    $cpuAll = if ($null -ne $cpuOne) { $cpuOne / $logical } else { $null }
    $last = $samples[$samples.Count - 1].Rows

    $row = [pscustomobject][ordered]@{
        timestamp            = (Get-Date).ToString('s')
        name                 = $Name
        run                  = $run
        startup_ms           = Format-Num $startupMs '0'
        ws_mb_total          = Format-Num $wsTotal
        private_mb_total     = Format-Num $prTotal
        ws_mb_app            = Format-Num $wsApp
        private_mb_app       = Format-Num $prApp
        cpu_idle_pct         = Format-Num $cpuOne '0.00'
        process_count        = $last.Count
        cpu_idle_pct_allcores = Format-Num $cpuAll '0.000'
        cpu_idle_pct_app     = Format-Num $cpuAppOne '0.00'
        process_count_app    = @($last | Where-Object { -not $_.Shell }).Count
    }
    $results.Add([pscustomobject]@{
            Row = $row; Startup = $startupMs; WsTotal = $wsTotal; PrTotal = $prTotal; WsApp = $wsApp; PrApp = $prApp
            Cpu = $cpuOne; CpuAll = $cpuAll; CpuApp = $cpuAppOne; Count = $last.Count
            CountApp = @($last | Where-Object { -not $_.Shell }).Count
        })

    $dir = Split-Path -Parent $OutCsv
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
    $row | Export-Csv -Path $OutCsv -Append

    Write-Host ("   private app {0:N1} MB | total {1:N1} MB | ws app {2:N1} MB | cpu idle {3:N2}% (1 core) | {4} procs" -f
        $prApp, $prTotal, $wsApp, $cpuOne, $last.Count)
    Write-Verbose ("Per-process breakdown (last sample):`n" + (
            $last | Sort-Object Private -Descending | Select-Object Name, Pid, Parent, Shell,
            @{ n = 'WS_MB'; e = { [math]::Round($_.WS / 1MB, 1) } },
            @{ n = 'Private_MB'; e = { [math]::Round($_.Private / 1MB, 1) } },
            @{ n = 'CPU_s'; e = { [math]::Round($_.Cpu, 2) } } | Format-Table -AutoSize | Out-String))

    Start-Sleep -Seconds 1
}

# ---------------------------------------------------------------------------------------------
# Summary (median of runs)
# ---------------------------------------------------------------------------------------------
if ($results.Count -eq 0) {
    Write-Warning "No successful runs for '$Name'."
    return
}
$summary = [pscustomobject]@{
    PSTypeName     = 'Selachi.Bench.Summary'
    Name           = $Name
    Exe            = $resolved
    Runs           = $results.Count
    StartupMs      = Get-Median ($results | ForEach-Object Startup)
    WsMbTotal      = Get-Median ($results | ForEach-Object WsTotal)
    PrivateMbTotal = Get-Median ($results | ForEach-Object PrTotal)
    WsMbApp        = Get-Median ($results | ForEach-Object WsApp)
    PrivateMbApp   = Get-Median ($results | ForEach-Object PrApp)
    CpuIdlePct     = Get-Median ($results | ForEach-Object Cpu)
    CpuIdlePctAll  = Get-Median ($results | ForEach-Object CpuAll)
    CpuIdlePctApp  = Get-Median ($results | ForEach-Object CpuApp)
    Processes      = Get-Median ($results | ForEach-Object Count)
    ProcessesApp   = Get-Median ($results | ForEach-Object CountApp)
}

Write-Host "`nSummary (median of $($results.Count) run(s)); cpu_% = one core, cpu_all_% = all cores; CSV: $OutCsv" -ForegroundColor Green
$summary | Format-Table Name, Runs,
@{ n = 'startup_ms'; e = { if ($null -ne $_.StartupMs) { '{0:N0}' -f $_.StartupMs } else { 'n/a' } }; a = 'right' },
@{ n = 'priv_app_MB'; e = { '{0:N1}' -f $_.PrivateMbApp }; a = 'right' },
@{ n = 'priv_tot_MB'; e = { '{0:N1}' -f $_.PrivateMbTotal }; a = 'right' },
@{ n = 'ws_app_MB'; e = { '{0:N1}' -f $_.WsMbApp }; a = 'right' },
@{ n = 'ws_tot_MB'; e = { '{0:N1}' -f $_.WsMbTotal }; a = 'right' },
@{ n = 'cpu_%'; e = { '{0:N2}' -f $_.CpuIdlePct }; a = 'right' },
@{ n = 'cpu_app_%'; e = { '{0:N2}' -f $_.CpuIdlePctApp }; a = 'right' },
@{ n = 'cpu_all_%'; e = { '{0:N3}' -f $_.CpuIdlePctAll }; a = 'right' },
@{ n = 'procs'; e = { '{0:N0}' -f $_.Processes }; a = 'right' } -AutoSize | Out-Host

if ($PassThru) { $summary }
