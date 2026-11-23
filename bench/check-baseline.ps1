<#
.SYNOPSIS
    Performance regression gate: runs measure.ps1 on a release build and compares startup and
    private (app) memory against a committed baseline (bench/baseline.json).

.DESCRIPTION
    Part of Wave 6c.1 (CI and quality). Used by .github/workflows/ci.yml (job "perf") and usable locally.

      1. Creates a SEEDED data directory (SELACHI_DATA_DIR) with the legacy `onboarding.done` marker,
         which the app migrates into state.db's `meta` table (store::db::ONBOARDING_DONE) on first start.
         So the first-run wizard (animated, much heavier) never shows and does not skew the numbers.
      2. Runs measure.ps1 (-Runs x -IdleSeconds) and takes the medians.
      3. Compares `startup_ms` and `private_mb_app` with the baseline profile `-Profile`.
         Fails (exit 1) if either is more than -Threshold (default 15 %) worse.
      4. Always writes bench/results/measured.json (same shape as a baseline profile) so a baseline can
         be created from any run; -Update writes it into bench/baseline.json directly.

    Baseline profiles. Absolute numbers depend on the hardware (CPU for startup; GPU driver / WARP for
    memory), so baseline.json holds one profile per machine class: `local` (the developer machine, with
    its description inside) and `ci` (GitHub-hosted windows-latest). A profile that does not exist yet
    is NOT a failure: the script reports "no baseline" and exits 0 so the first run can seed it.

    Idle CPU is reported but not gated (too noisy on shared runners; see bench/README.md).

.PARAMETER Exe
    Release executable (target/release/selachi.exe).

.PARAMETER Profile
    Baseline profile to compare against / update (`local` or `ci`).

.PARAMETER Update
    Write the measured numbers into bench/baseline.json under -Profile (with machine info) instead of comparing.

.PARAMETER Threshold
    Allowed regression as a fraction (0.15 = 15 %).

.PARAMETER SummaryFile
    Append a markdown summary here (CI passes $env:GITHUB_STEP_SUMMARY).

.EXAMPLE
    pwsh -File bench/check-baseline.ps1 -Exe target/release/selachi.exe -Profile local
    pwsh -File bench/check-baseline.ps1 -Exe target/release/selachi.exe -Profile local -Update
#>
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Profile = 'local',
    [string]$Baseline,
    [ValidateRange(1, 20)][int]$Runs = 3,
    [ValidateRange(1, 600)][int]$IdleSeconds = 10,
    [ValidateRange(0, 600)][int]$SettleSeconds = 5,
    [ValidateRange(1, 600)][int]$StartupTimeoutSeconds = 30,
    [ValidateRange(0.01, 5)][double]$Threshold = 0.15,
    [switch]$Update,
    [string]$SummaryFile
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$inv = [System.Globalization.CultureInfo]::InvariantCulture
if (-not $Baseline) { $Baseline = Join-Path $PSScriptRoot 'baseline.json' }
$resultsDir = Join-Path $PSScriptRoot 'results'
New-Item -ItemType Directory -Force $resultsDir | Out-Null
$exePath = (Resolve-Path -LiteralPath $Exe).Path

# --- seeded, isolated data dir -------------------------------------------------------------
$dataDir = Join-Path ([System.IO.Path]::GetTempPath()) ("selachi-bench-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force $dataDir | Out-Null
# Legacy first-run marker: ui_onboarding::needs_onboarding() treats it as "done" and migrates it
# into state.db (meta key onboarding.done). Same effect as finishing the wizard once.
Set-Content -LiteralPath (Join-Path $dataDir 'onboarding.done') -Value 'selachi onboarding v1 done (bench seed)'
$env:SELACHI_DATA_DIR = $dataDir
Write-Host "==> seeded data dir: $dataDir"

# --- measure -------------------------------------------------------------------------------
$csv = Join-Path $resultsDir 'check-baseline.csv'
$measure = Join-Path $PSScriptRoot 'measure.ps1'
try {
    $out = & $measure -Exe $exePath -Name 'selachi' -Runs $Runs -IdleSeconds $IdleSeconds `
        -SettleSeconds $SettleSeconds -StartupTimeoutSeconds $StartupTimeoutSeconds -OutCsv $csv -PassThru
}
finally {
    Remove-Item -Recurse -Force -LiteralPath $dataDir -ErrorAction SilentlyContinue
}
$s = @($out | Where-Object { $_ -and $_.PSObject.TypeNames -contains 'Selachi.Bench.Summary' }) | Select-Object -Last 1
if (-not $s) { throw 'measure.ps1 produced no successful run (the exe did not start or never showed a window).' }

function Num($v) { if ($null -eq $v) { $null } else { [math]::Round([double]$v, 1) } }
$measured = [ordered]@{
    startup_ms     = Num $s.StartupMs
    private_mb_app = Num $s.PrivateMbApp
    cpu_idle_pct   = if ($null -ne $s.CpuIdlePct) { [math]::Round([double]$s.CpuIdlePct, 2) } else { $null }
    runs           = $s.Runs
    idle_seconds   = $IdleSeconds
}

function Get-MachineInfo {
    $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1)
    $os = Get-CimInstance Win32_OperatingSystem
    $gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name })
    $commit = try { (git -C $PSScriptRoot rev-parse --short HEAD 2>$null) } catch { $null }
    [ordered]@{
        cpu       = $cpu.Name.Trim()
        logical   = [Environment]::ProcessorCount
        ram_gb    = [math]::Round($os.TotalVisibleMemorySize / 1MB, 1)
        gpu       = $gpus
        os        = "$($os.Caption) $($os.Version)"
        exe_mb    = [math]::Round((Get-Item $exePath).Length / 1MB, 1)
        commit    = $commit
        date      = (Get-Date).ToString('yyyy-MM-dd')
    }
}
$machine = Get-MachineInfo
$profileObj = [ordered]@{ machine = $machine } + $measured
$measuredFile = Join-Path $resultsDir 'measured.json'
([ordered]@{ profile = $Profile; data = $profileObj } | ConvertTo-Json -Depth 6) | Set-Content -LiteralPath $measuredFile -Encoding utf8
Write-Host "==> measured: $($measured | ConvertTo-Json -Compress)  (saved to $measuredFile)"

# --- load / update baseline ----------------------------------------------------------------
$base = if (Test-Path $Baseline) { Get-Content -Raw -LiteralPath $Baseline | ConvertFrom-Json -AsHashtable } else { $null }
if (-not $base) { $base = [ordered]@{ schema = 1; threshold_pct = [int]($Threshold * 100); profiles = [ordered]@{} } }
if (-not $base.Contains('profiles') -or $null -eq $base['profiles']) { $base['profiles'] = [ordered]@{} }

if ($Update) {
    $base['profiles'][$Profile] = $profileObj
    ($base | ConvertTo-Json -Depth 8) | Set-Content -LiteralPath $Baseline -Encoding utf8
    Write-Host "==> baseline profile '$Profile' written to $Baseline" -ForegroundColor Green
    exit 0
}

$ref = if ($base['profiles'].Contains($Profile)) { $base['profiles'][$Profile] } else { $null }
$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add("### Selachi performance gate (profile '$Profile', threshold $([int]($Threshold * 100)) %)")
$lines.Add('')
if (-not $ref -or $null -eq $ref['startup_ms']) {
    $lines.Add("No baseline for profile **$Profile** in bench/baseline.json: nothing to compare (not a failure).")
    $lines.Add('')
    $lines.Add("Measured: startup $($measured.startup_ms) ms, private app $($measured.private_mb_app) MB, idle CPU $($measured.cpu_idle_pct) %.")
    $lines.Add("To seed it, copy the 'data' object of the uploaded measured.json into profiles.$Profile of bench/baseline.json.")
    $lines | ForEach-Object { Write-Host $_ }
    if ($SummaryFile) { $lines | Add-Content -LiteralPath $SummaryFile }
    exit 0
}

$failed = $false
$lines.Add('| metric | baseline | measured | delta | limit | result |')
$lines.Add('|---|---:|---:|---:|---:|---|')
foreach ($m in 'startup_ms', 'private_mb_app') {
    $b = [double]$ref[$m]
    $v = $measured[$m]
    if ($null -eq $v) {
        $lines.Add("| $m | $b | n/a | | | FAIL (no value: window never appeared?) |")
        $failed = $true
        continue
    }
    $limit = $b * (1 + $Threshold)
    $delta = if ($b -gt 0) { 100.0 * ($v - $b) / $b } else { 0.0 }
    $ok = $v -le $limit
    if (-not $ok) { $failed = $true }
    $lines.Add(("| {0} | {1} | {2} | {3:+0.0;-0.0} % | {4:N1} | {5} |" -f $m, $b, $v, $delta, $limit, $(if ($ok) { 'ok' } else { 'FAIL' })))
}
$lines.Add('')
$lines.Add("Idle CPU (informational, not gated): $($measured.cpu_idle_pct) % of one core.")
$lines.Add("Baseline machine: $($ref['machine']['cpu']) ($($ref['machine']['date']), commit $($ref['machine']['commit'])).")
$lines | ForEach-Object { Write-Host $_ }
if ($SummaryFile) { $lines | Add-Content -LiteralPath $SummaryFile }
if ($failed) {
    Write-Error "Performance regression beyond $([int]($Threshold * 100)) % (see table above)." -ErrorAction Continue
    exit 1
}
Write-Host '==> performance gate passed' -ForegroundColor Green
exit 0
