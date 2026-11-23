<#
.SYNOPSIS
    Time-resolved per-thread CPU: prints every sampling interval in which a thread burned CPU.

.DESCRIPTION
    threads.ps1 gives one total per thread over the window, which hides *when* the CPU was spent.
    This script launches -ExePath (or attaches to -ProcessId), waits -SettleSeconds and then samples
    all threads every -IntervalMs for -Seconds. Each interval where a thread used more than
    -MinMs of CPU is printed as `t=<s> <tid> <name> <ms>`; the end line gives the total. Use it to
    tell periodic work (timers, scans) from one-off bursts (startup, shell output).
    Nothing is sent to any window.

.EXAMPLE
    pwsh -File bench/bursts.ps1 -ExePath target/release/selachi.exe -Seconds 60
#>
param(
    [int]$ProcessId = 0,
    [string]$ExePath = '',
    [int]$Seconds = 30,
    [int]$SettleSeconds = 5,
    [int]$IntervalMs = 500,
    [double]$MinMs = 15
)

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class BurstNative {
  [DllImport("kernel32.dll")] public static extern IntPtr OpenThread(uint access, bool inherit, uint tid);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] public static extern int GetThreadDescription(IntPtr h, out IntPtr desc);
  [DllImport("kernel32.dll")] public static extern bool GetThreadTimes(IntPtr h, out long c, out long e, out long k, out long u);
  [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr p);
  public static string Name(uint tid) {
    IntPtr h = OpenThread(0x0040, false, tid);
    if (h == IntPtr.Zero) return "";
    try { IntPtr d; if (GetThreadDescription(h, out d) >= 0 && d != IntPtr.Zero) { string s = Marshal.PtrToStringUni(d); LocalFree(d); return s; } }
    finally { CloseHandle(h); }
    return "";
  }
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    public static uint ForegroundPid() { uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid; }
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
    $started = Start-Process -FilePath $ExePath -PassThru
    $ProcessId = $started.Id
    Start-Sleep -Seconds $SettleSeconds
}
if ($ProcessId -eq 0) { throw 'give -ProcessId or -ExePath' }

try {
    $p = Get-Process -Id $ProcessId
    $prev = @{}
    foreach ($t in $p.Threads) { $prev[[uint32]$t.Id] = [BurstNative]::Cpu100ns([uint32]$t.Id) }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $total = 0.0
    while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
        Start-Sleep -Milliseconds $IntervalMs
        $p.Refresh()
        $now = $sw.Elapsed.TotalSeconds
        foreach ($t in $p.Threads) {
            $id = [uint32]$t.Id
            $c = [BurstNative]::Cpu100ns($id)
            if ($c -lt 0) { continue }
            $b = if ($prev.ContainsKey($id)) { $prev[$id] } else { 0 }
            $prev[$id] = $c
            $ms = ($c - $b) / 10000.0
            $total += $ms
            if ($ms -ge $MinMs) { '{0,6:N1}s tid={1,-6} {2,-24} {3,8:N1} ms  focused={4}' -f $now, $id, [BurstNative]::Name($id), $ms, ([BurstNative]::ForegroundPid() -eq $ProcessId) }
        }
    }
    'total {0:N1} ms over {1:N1} s = {2:N2} % of one core' -f $total, $sw.Elapsed.TotalSeconds, ($total / 1000 / $sw.Elapsed.TotalSeconds * 100)
} finally {
    if ($started -and -not $started.HasExited) { Stop-Process -Id $started.Id }
}
