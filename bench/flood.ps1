<#
.SYNOPSIS
    Output-throughput test. Run it INSIDE the terminal you want to measure.

.DESCRIPTION
    Generates and prints N MB (default 100) of mixed ANSI-colored text to stdout as fast as the
    terminal will accept it, then reports elapsed time and MB/s. This measures the terminal's
    OUTPUT THROUGHPUT (ConPTY pipe -> VT parser -> grid -> render coalescing), the "cat of a 100 MB
    file" scenario from docs/PLAN.md section 9. It does not measure the speed of this script:
    the data is pre-generated in a few large blocks and written as raw bytes.

    Content: lines of 40-110 visible columns with a mix of SGR sequences (16 colors, bold/dim/
    underline, 256-color, truecolor, resets). -Plain emits the same amount of text without any
    escape sequence, to separate parser cost from plain text cost.

    Timing: the clock stops when the last byte has been ACCEPTED by the terminal, i.e. the write
    returned. ConPTY gives only a small buffer (pipe + conhost), so a slow terminal back-pressures the
    writer and the elapsed time reflects its real consumption speed. A terminal can still need a few
    hundred ms to finish rendering the tail (its own queue); for the strictest comparison also watch
    that the last line appears at the moment the result is printed, or use a screen recorder.
    (A DSR/ESC[6n handshake was evaluated and rejected: under ConPTY conhost answers it itself,
    so it says nothing about the terminal's backlog.)

    Tips: maximise the window or use the same window size in every terminal you compare, disable
    background transparency/blur, run 3 times and take the median, and run once in the plain variant.

.PARAMETER SizeMB
    Total amount of data to print. Default 100.

.PARAMETER Plain
    No ANSI escape sequences, only plain text.

.PARAMETER BlockKB
    Size of each pre-generated block. Default 256.

.PARAMETER ResultFile
    Also append the result line to this file (handy when the terminal window closes on exit).

.PARAMETER Seed
    RNG seed so every terminal gets identical bytes. Default 42.

.EXAMPLE
    pwsh -File bench/flood.ps1            # 100 MB colored

.EXAMPLE
    pwsh -File bench/flood.ps1 -Plain -SizeMB 200
#>
#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateRange(1, 10240)][int]$SizeMB = 100,
    [switch]$Plain,
    [ValidateRange(4, 4096)][int]$BlockKB = 256,
    [int]$Seed = 42,
    [string]$ResultFile
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$rng = [System.Random]::new($Seed)
$ESC = [char]27
$words = ('selachi terminal gpui alacritty conpty render throughput frame scrollback glyph atlas shader ' +
    'cursor parser buffer damage coalesce flood stream vt100 xterm utf8 color truecolor palette ' +
    'lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor').Split(' ')

function New-Line {
    $target = $rng.Next(40, 111)
    $sb = [System.Text.StringBuilder]::new(200)
    $visible = 0
    while ($visible -lt $target) {
        $w = $words[$rng.Next($words.Length)]
        if (-not $Plain -and $rng.Next(3) -eq 0) {
            switch ($rng.Next(6)) {
                0 { [void]$sb.Append("$ESC[$(30 + $rng.Next(8))m") }
                1 { [void]$sb.Append("$ESC[$(90 + $rng.Next(8))m") }
                2 { [void]$sb.Append("$ESC[1;$(31 + $rng.Next(6))m") }
                3 { [void]$sb.Append("$ESC[38;5;$($rng.Next(256))m") }
                4 { [void]$sb.Append("$ESC[38;2;$($rng.Next(256));$($rng.Next(256));$($rng.Next(256))m") }
                5 { [void]$sb.Append("$ESC[$(@(2, 4, 7)[$rng.Next(3)])m") }
            }
        }
        [void]$sb.Append($w).Append(' ')
        $visible += $w.Length + 1
        if (-not $Plain -and $rng.Next(5) -eq 0) { [void]$sb.Append("$ESC[0m") }
    }
    if (-not $Plain) { [void]$sb.Append("$ESC[0m") }
    [void]$sb.Append("`r`n")
    $sb.ToString()
}

# Pre-generate 4 distinct blocks (bytes) and cycle through them.
$blocks = foreach ($i in 1..4) {
    $sb = [System.Text.StringBuilder]::new($BlockKB * 1024 + 512)
    while ($sb.Length -lt $BlockKB * 1024) { [void]$sb.Append((New-Line)) }
    , [System.Text.Encoding]::UTF8.GetBytes($sb.ToString())
}

$total = [int64]$SizeMB * 1MB
$out = [Console]::OpenStandardOutput()

# Start on a fresh line and make sure nothing earlier is pending.
[Console]::Out.Flush()
$label = if ($Plain) { 'plain' } else { 'ansi' }

$written = [int64]0
$i = 0
$sw = [System.Diagnostics.Stopwatch]::StartNew()
while ($written -lt $total) {
    $b = $blocks[$i % $blocks.Count]; $i++
    $n = [int][math]::Min($b.Length, $total - $written)
    $out.Write($b, 0, $n)
    $written += $n
}
$out.Flush()
$tWrite = $sw.Elapsed.TotalSeconds

$sw.Stop()

[Console]::Out.WriteLine('')
$mbps = ($written / 1MB) / $tWrite
$line = "flood[{0}]: {1:N0} MB in {2:N2} s = {3:N1} MB/s (output throughput, write-accepted time)" -f
$label, ($written / 1MB), $tWrite, $mbps
[Console]::Out.WriteLine($line)
if ($ResultFile) { Add-Content -LiteralPath $ResultFile -Value $line }
