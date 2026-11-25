<#
.SYNOPSIS
    Memory regression check for long chat sessions (docs/olas/ola-9.md, 9C).

.DESCRIPTION
    Runs the release `chat_demo` example (fake agent, no model calls) over synthetic threads and
    measures the private bytes of the process at idle through bench/measure.ps1:

      empty        --long 0                                  (the idle baseline, a short scripted session)
      long         --long <Rows> --scroll-through            (every row painted once, back at the end)
      long-open    --long <Rows> --scroll-through --expand-all   (every command / thought expanded too)
      images       --images <Images>                         (4K screenshots attached to the composer)

    Each case fails when its private MB exceeds -MaxMb (absolute) or exceeds the empty baseline by
    more than -MaxGrowthMb. Exit code 1 on any failure, so it can gate a CI job.

    Build first:  cargo build --release -p ui_chat --example chat_demo

.EXAMPLE
    pwsh -File bench/chat-ram.ps1
    pwsh -File bench/chat-ram.ps1 -Rows 400 -MaxMb 150 -MaxGrowthMb 60 -Cases long,images
#>
#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$Exe,
    [ValidateRange(1, 5000)][int]$Rows = 400,
    [ValidateRange(0, 50)][int]$Images = 4,
    [ValidateRange(0, 10000000)][int]$OutBytes = 50000,
    [double]$MaxMb = 150,
    [double]$MaxGrowthMb = 60,
    [ValidateSet('empty', 'long', 'long-open', 'images')][string[]]$Cases = @('empty', 'long', 'long-open', 'images'),
    [ValidateRange(1, 5)][int]$Runs = 1,
    [ValidateRange(1, 300)][int]$IdleSeconds = 6,
    [ValidateRange(0, 120)][int]$SettleSeconds = 14
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $root 'target/release/examples/chat_demo.exe' }
if (-not (Test-Path -LiteralPath $Exe)) { throw "chat_demo not found: $Exe (cargo build --release -p ui_chat --example chat_demo)" }
$csv = Join-Path $PSScriptRoot 'results/chat-ram.csv'

$base = @('--scene', 'thread', '--no-sidebar')
$defs = [ordered]@{
    'empty'     = $base + @('--long', '0')
    'long'      = $base + @('--long', "$Rows", '--out-bytes', "$OutBytes", '--scroll-through')
    'long-open' = $base + @('--long', "$Rows", '--out-bytes', "$OutBytes", '--scroll-through', '--expand-all')
    'images'    = $base + @('--long', '0', '--images', "$Images")
}

$measured = [ordered]@{}
$order = @('empty') + @($Cases | Where-Object { $_ -ne 'empty' })
foreach ($name in $order) {
    $s = & (Join-Path $PSScriptRoot 'measure.ps1') -Exe $Exe -Name "chat-$name" -ArgumentList $defs[$name] `
        -IdleSeconds $IdleSeconds -SettleSeconds $SettleSeconds -Runs $Runs -OutCsv $csv -PassThru 6>$null
    $s = @($s) | Select-Object -Last 1
    if (-not $s) { throw "no measurement for case '$name'" }
    $measured[$name] = [double]$s.PrivateMbApp
}

$baseline = $measured['empty']
$fail = $false
Write-Host ''
Write-Host ('{0,-10} {1,10} {2,12}  {3}' -f 'case', 'private_MB', 'vs_empty_MB', 'result')
foreach ($name in $measured.Keys) {
    $mb = $measured[$name]
    $growth = $mb - $baseline
    $ok = ($name -eq 'empty') -or (($mb -le $MaxMb) -and ($growth -le $MaxGrowthMb))
    if (-not $ok) { $fail = $true }
    Write-Host ('{0,-10} {1,10:N1} {2,12:N1}  {3}' -f $name, $mb, $growth, $(if ($ok) { 'ok' } else { "FAIL (max $MaxMb MB, growth max $MaxGrowthMb MB)" }))
}
if ($fail) { exit 1 }
