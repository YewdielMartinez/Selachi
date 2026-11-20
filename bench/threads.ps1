<#
.SYNOPSIS
    Per-thread CPU breakdown of a running process (idle-CPU attribution).

.DESCRIPTION
    Samples the CPU time of every thread of a process (or of -ExePath launched by this script) over
    -Seconds and prints one row per thread: thread id, name (GetThreadDescription, set by Rust's
    std::thread::Builder::name and by gpui/alacritty), CPU ms consumed, % of one core, and the
    wake-up rate (context switches / s, from Thread.ContextSwitches is not available from .NET,
    so wake-ups are estimated through CPU-time ticks only).
    Used for the "which thread burns CPU at idle?" step of the perf investigation.

.EXAMPLE
    pwsh -File bench/threads.ps1 -ExePath target/release/selachi.exe -Seconds 15
    pwsh -File bench/threads.ps1 -Pid 1234 -Seconds 10
#>
param(
    [int]$ProcessId = 0,
    [string]$ExePath = '',
    [string[]]$ExeArgs = @(),
    [int]$Seconds = 10,
    [int]$SettleSeconds = 6,
    [string]$StderrFile = ''
)

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class ThrNative {
  [DllImport("kernel32.dll")] public static extern IntPtr OpenThread(uint access, bool inherit, uint tid);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] public static extern int GetThreadDescription(IntPtr h, out IntPtr desc);
  [DllImport("kernel32.dll")] public static extern bool GetThreadTimes(IntPtr h, out long c, out long e, out long k, out long u);
  [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr p);
  public static string Name(uint tid) {
    IntPtr h = OpenThread(0x0040 /*QUERY_LIMITED_INFORMATION*/, false, tid);
    if (h == IntPtr.Zero) return "";
    try { IntPtr d; if (GetThreadDescription(h, out d) >= 0 && d != IntPtr.Zero) { string s = Marshal.PtrToStringUni(d); LocalFree(d); return s; } }
    finally { CloseHandle(h); }
    return "";
  }
  public static long Cpu100ns(uint tid) {
    IntPtr h = OpenThread(0x0040, false, tid);
    if (h == IntPtr.Zero) return -1;
    try { long c, e, k, u; if (GetThreadTimes(h, out c, out e, out k, out u)) return k + u; }
    finally { CloseHandle(h); }
    return -1;
  }
}
'@

$started = $null
if ($ExePath) {
    $sp = @{ FilePath = $ExePath; PassThru = $true }
    if ($ExeArgs.Count) { $sp.ArgumentList = $ExeArgs }
    if ($StderrFile) { $sp.RedirectStandardError = $StderrFile }
    $started = Start-Process @sp
    $ProcessId = $started.Id
    Start-Sleep -Seconds $SettleSeconds
}
if ($ProcessId -eq 0) { throw 'give -ProcessId or -ExePath' }

try {
    $p = Get-Process -Id $ProcessId
    $t0 = @{}
    foreach ($t in $p.Threads) { $t0[[uint32]$t.Id] = [ThrNative]::Cpu100ns([uint32]$t.Id) }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $Seconds
    $el = $sw.Elapsed.TotalSeconds
    $p.Refresh()
    $rows = foreach ($t in $p.Threads) {
        $id = [uint32]$t.Id
        $now = [ThrNative]::Cpu100ns($id)
        $before = if ($t0.ContainsKey($id)) { $t0[$id] } else { 0 }
        if ($now -lt 0) { continue }
        [pscustomobject]@{
            Tid  = $id
            Name = [ThrNative]::Name($id)
            CpuMs = [math]::Round(($now - $before) / 10000.0, 1)
            PctOfCore = [math]::Round(($now - $before) / 1e7 / $el * 100, 2)
            State = "$($t.ThreadState)/$($t.WaitReason)"
        }
    }
    $rows | Sort-Object CpuMs -Descending | Format-Table -AutoSize
    $tot = ($rows | Measure-Object CpuMs -Sum).Sum
    "total {0:N1} ms over {1:N1} s = {2:N2} % of one core (threads: {3})" -f $tot, $el, ($tot / 1000 / $el * 100), $rows.Count
} finally {
    if ($started -and -not $started.HasExited) { Stop-Process -Id $started.Id }
}
