<#
.SYNOPSIS
    Runs measure.ps1 for Selachi, Windows Terminal and Warp and writes bench/results/latest.md.

.DESCRIPTION
    Targets (each is skipped with a message when unavailable):
      * Selachi          target/release/selachi.exe (build with: cargo build --release)
      * Windows Terminal wt.exe (app execution alias)
      * Warp             auto-detected: -WarpExe, %LOCALAPPDATA%\Programs\Warp\warp.exe, Program Files,
                         registry App Paths, uninstall keys (InstallLocation), Get-Command warp.exe.

    IMPORTANT: Windows Terminal and Warp are single-instance style apps. Close every running
    instance before running this script. measure.ps1 will NOT touch existing instances; it prints a
    message and skips that target instead.

    Output: bench/results/latest.md (markdown table of medians + machine info) and rows appended to
    bench/results/results.csv. Numbers come from measure.ps1 - see bench/README.md for metric meaning.

.PARAMETER IdleSeconds
    Idle sampling window per run (default 30).

.PARAMETER Runs
    Runs per target (default 3).

.PARAMETER SettleSeconds
    Settle time between first window and idle sampling (default 5).

.PARAMETER Skip
    Targets to skip: selachi, wt, warp. Array or comma-separated: `-Skip warp,wt`.

.PARAMETER SelachiExe
    Override the Selachi binary path.

.PARAMETER WarpExe
    Override the Warp binary path.

.PARAMETER OutCsv
    Raw per-run CSV (default bench/results/results.csv).

.PARAMETER OutMd
    Markdown report (default bench/results/latest.md).

.EXAMPLE
    ./bench/compare.ps1 -IdleSeconds 30 -Runs 3

.EXAMPLE
    ./bench/compare.ps1 -Skip warp -Runs 1 -IdleSeconds 10

.EXAMPLE
    pwsh -File bench/compare.ps1 -Skip warp,wt
#>
#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateRange(1, 3600)][int]$IdleSeconds = 30,
    [ValidateRange(1, 100)][int]$Runs = 3,
    [ValidateRange(0, 600)][int]$SettleSeconds = 5,
    # Accepts `-Skip warp,wt` (array), `-Skip 'warp,wt'` and what `pwsh -File` passes (one comma-joined
    # string). A ValidateSet here rejected the -File form, so the values are normalised and checked below.
    [string[]]$Skip = @(),
    [string]$SelachiExe,
    [string]$WarpExe,
    [string]$OutCsv,
    [string]$OutMd
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Skip = @($Skip | ForEach-Object { $_ -split '[,;\s]+' } | Where-Object { $_ } | ForEach-Object { $_.ToLowerInvariant() })
foreach ($name in $Skip) {
    if ($name -notin 'selachi', 'wt', 'warp') { throw "Unknown -Skip target '$name' (use selachi, wt, warp)." }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$measure = Join-Path $PSScriptRoot 'measure.ps1'
if (-not $OutCsv) { $OutCsv = Join-Path $PSScriptRoot 'results/results.csv' }
if (-not $OutMd) { $OutMd = Join-Path $PSScriptRoot 'results/latest.md' }
if (-not $SelachiExe) { $SelachiExe = Join-Path $repoRoot 'target/release/selachi.exe' }

# ---------------------------------------------------------------------------------------------
function Find-Warp {
    param([string]$Override)
    if ($Override) { if (Test-Path -LiteralPath $Override -PathType Leaf) { return (Resolve-Path -LiteralPath $Override).Path } else { return $null } }

    $candidates = [System.Collections.Generic.List[string]]::new()
    foreach ($base in @($env:LOCALAPPDATA, $env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if ($base) {
            $candidates.Add((Join-Path $base 'Programs\Warp\warp.exe'))
            $candidates.Add((Join-Path $base 'Warp\warp.exe'))
        }
    }
    foreach ($hive in 'HKCU', 'HKLM') {      # App Paths
        $k = "${hive}:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\warp.exe"
        if (Test-Path $k) { $v = (Get-ItemProperty $k).'(default)'; if ($v) { $candidates.Add($v.Trim('"')) } }
    }
    foreach ($hive in 'HKCU', 'HKLM') {      # Uninstall keys
        foreach ($sub in 'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall', 'SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall') {
            $root = "${hive}:\$sub"
            if (-not (Test-Path $root)) { continue }
            foreach ($key in Get-ChildItem $root -ErrorAction SilentlyContinue) {
                $p = Get-ItemProperty $key.PSPath -ErrorAction SilentlyContinue
                if ($p -and $p.PSObject.Properties['DisplayName'] -and $p.DisplayName -like 'Warp*' -and
                    $p.PSObject.Properties['InstallLocation'] -and $p.InstallLocation) {
                    $candidates.Add((Join-Path $p.InstallLocation 'warp.exe'))
                }
            }
        }
    }
    $cmd = Get-Command warp.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd) { $candidates.Add($cmd.Source) }
    foreach ($c in $candidates) { if ($c -and (Test-Path -LiteralPath $c -PathType Leaf)) { return (Resolve-Path -LiteralPath $c).Path } }
    return $null
}

function Get-MachineInfo {
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    $cs = Get-CimInstance Win32_ComputerSystem
    $cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $disp = if ($cv.PSObject.Properties['DisplayVersion']) { $cv.DisplayVersion } else { '' }
    # Registry ProductName still says 'Windows 10' on Windows 11.
    $prod = if ([int]$cv.CurrentBuildNumber -ge 22000) { $cv.ProductName -replace 'Windows 10', 'Windows 11' } else { $cv.ProductName }
    [pscustomobject]@{
        Cpu     = ($cpu.Name -replace '\s+', ' ').Trim()
        Cores   = "$($cpu.NumberOfCores) cores / $($cpu.NumberOfLogicalProcessors) threads"
        RamGb   = [math]::Round($cs.TotalPhysicalMemory / 1GB, 1)
        Windows = "$prod $disp (build $($cv.CurrentBuildNumber).$($cv.UBR))"
        Gpu     = ((Get-CimInstance Win32_VideoController | ForEach-Object Name) -join '; ')
        Date    = (Get-Date).ToString('yyyy-MM-dd HH:mm zzz')
    }
}

function Get-FileVer([string]$Path) {
    if ([IO.Path]::GetFileName($Path) -ieq 'wt.exe') {       # app execution alias: ask the Appx package
        try { $pkg = Get-AppxPackage -Name Microsoft.WindowsTerminal -ErrorAction Stop | Select-Object -First 1; if ($pkg) { return [string]$pkg.Version } } catch { }
    }
    try { $v = (Get-Item -LiteralPath $Path).VersionInfo.ProductVersion; if ($v) { return $v } } catch { }
    return 'n/a'
}

# ---------------------------------------------------------------------------------------------
# Build target list
# ---------------------------------------------------------------------------------------------
$targets = [System.Collections.Generic.List[object]]::new()
$notes = [System.Collections.Generic.List[string]]::new()

if ('selachi' -in $Skip) { $notes.Add('Selachi: skipped by -Skip.') }
elseif (Test-Path -LiteralPath $SelachiExe -PathType Leaf) {
    $targets.Add(@{ Name = 'Selachi'; Exe = (Resolve-Path -LiteralPath $SelachiExe).Path; Extra = @{} })
}
else {
    $msg = "Selachi not built ($SelachiExe missing). Run: cargo build --release. Skipping."
    Write-Warning $msg; $notes.Add($msg)
}

if ('wt' -in $Skip) { $notes.Add('Windows Terminal: skipped by -Skip.') }
elseif (Get-Command wt.exe -CommandType Application -ErrorAction SilentlyContinue) {
    $targets.Add(@{ Name = 'Windows Terminal'; Exe = 'wt.exe'; Extra = @{} })
}
else {
    $msg = 'Windows Terminal (wt.exe) not found. Skipping.'
    Write-Warning $msg; $notes.Add($msg)
}

if ('warp' -in $Skip) { $notes.Add('Warp: skipped by -Skip.') }
else {
    $warp = Find-Warp -Override $WarpExe
    if ($warp) { $targets.Add(@{ Name = 'Warp'; Exe = $warp; Extra = @{ SingleInstance = $true } }) }
    else {
        $msg = 'Warp not installed / not found (use -WarpExe <path>). Skipping.'
        Write-Warning $msg; $notes.Add($msg)
    }
}

if ($targets.Count -eq 0) { throw 'Nothing to measure.' }

# ---------------------------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------------------------
$summaries = [System.Collections.Generic.List[object]]::new()
foreach ($t in $targets) {
    $extra = $t.Extra
    $out = & $measure -Exe $t.Exe -Name $t.Name -IdleSeconds $IdleSeconds -Runs $Runs -SettleSeconds $SettleSeconds `
        -OutCsv $OutCsv -PassThru @extra
    $s = @($out) | Where-Object { $_ -and $_.PSObject.TypeNames -contains 'Selachi.Bench.Summary' } | Select-Object -Last 1
    if ($s) { $s | Add-Member -NotePropertyName Version -NotePropertyValue (Get-FileVer $s.Exe) -Force; $summaries.Add($s) }
    else { $notes.Add("$($t.Name): no successful runs (already running instance, launch failure or no window). See console output.") }
    Start-Sleep -Seconds 2
}

# ---------------------------------------------------------------------------------------------
# Markdown report
# ---------------------------------------------------------------------------------------------
$mi = Get-MachineInfo
$f1 = { param($v, $f = '0.0') if ($null -eq $v) { 'n/a' } else { ([double]$v).ToString($f, [cultureinfo]::InvariantCulture) } }
$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine('# Selachi benchmark - latest results')
[void]$sb.AppendLine()
[void]$sb.AppendLine("Generated by ``bench/compare.ps1`` on $($mi.Date). Median of $Runs run(s), idle window $IdleSeconds s, settle $SettleSeconds s.")
[void]$sb.AppendLine()
[void]$sb.AppendLine('## Results (median)')
[void]$sb.AppendLine()
[void]$sb.AppendLine('| Terminal | Version | Startup (ms) | Private app (MB) | Private total (MB) | WS app (MB) | WS total (MB) | Idle CPU app (% of 1 core) | Idle CPU total (% of 1 core) | Procs (app/total) |')
[void]$sb.AppendLine('|---|---|---:|---:|---:|---:|---:|---:|---:|---:|')
foreach ($s in $summaries) {
    $row = '| {0} | {1} | {2} | {3} | {4} | {5} | {6} | {7} | {8} | {9}/{10} |' -f $s.Name, $s.Version,
    (& $f1 $s.StartupMs '0'), (& $f1 $s.PrivateMbApp), (& $f1 $s.PrivateMbTotal), (& $f1 $s.WsMbApp), (& $f1 $s.WsMbTotal),
    (& $f1 $s.CpuIdlePctApp '0.00'), (& $f1 $s.CpuIdlePct '0.00'), (& $f1 $s.ProcessesApp '0'), (& $f1 $s.Processes '0')
    [void]$sb.AppendLine($row)
}
[void]$sb.AppendLine()
[void]$sb.AppendLine('*app* = process tree without the shell (pwsh/cmd/...) and its children, so terminals are compared on their own cost; *total* includes the shell. Private = Private Bytes (the honest number); Working Set double-counts pages shared between processes. Details: `bench/README.md`.')

$sel = $summaries | Where-Object Name -eq 'Selachi' | Select-Object -First 1
if ($sel) {
    foreach ($o in @($summaries | Where-Object { $_.Name -ne 'Selachi' -and $_.PrivateMbApp -gt 0 })) {
        $pct = 100.0 * ($sel.PrivateMbApp / $o.PrivateMbApp)
        [void]$sb.AppendLine()
        [void]$sb.AppendLine(("Selachi private (app) memory = {0:N0}% of {1}." -f $pct, $o.Name))
    }
}
if ($notes.Count -gt 0) {
    [void]$sb.AppendLine()
    [void]$sb.AppendLine('## Notes')
    [void]$sb.AppendLine()
    foreach ($n in $notes) { [void]$sb.AppendLine("- $n") }
}
[void]$sb.AppendLine()
[void]$sb.AppendLine('## Machine')
[void]$sb.AppendLine()
[void]$sb.AppendLine("- CPU: $($mi.Cpu) ($($mi.Cores))")
[void]$sb.AppendLine("- RAM: $($mi.RamGb) GB")
[void]$sb.AppendLine("- GPU: $($mi.Gpu)")
[void]$sb.AppendLine("- Windows: $($mi.Windows)")
[void]$sb.AppendLine("- Date: $($mi.Date)")
[void]$sb.AppendLine("- Raw data: ``$(Split-Path -Leaf $OutCsv)``")

$dir = Split-Path -Parent $OutMd
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
Set-Content -LiteralPath $OutMd -Value $sb.ToString() -Encoding utf8
Write-Host "`nReport written to $OutMd" -ForegroundColor Green
