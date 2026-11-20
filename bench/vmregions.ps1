<#
.SYNOPSIS
    Breaks the committed memory of a process down by region type (VirtualQueryEx walk), VMMap-style.

.DESCRIPTION
    Private Bytes = committed private memory. This script lists, for a running process or for
    -ExePath launched here (then waited -SettleSeconds), the committed bytes by kind (private /
    image / mapped), the biggest private committed regions, and how much of the private commit is
    actually resident (working set) via QueryWorkingSetEx sampling is NOT done: use Process WS for that.
    Used to attribute "where do the 100 MB of an empty GPUI window come from".

.EXAMPLE
    pwsh -File bench/vmregions.ps1 -ExePath target/release/examples/empty_window.exe
#>
param([int]$ProcessId = 0, [string]$ExePath = '', [string[]]$ExeArgs = @(), [int]$SettleSeconds = 8, [int]$Top = 12)

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class Vm {
  [StructLayout(LayoutKind.Sequential)] public struct MBI { public IntPtr BaseAddress, AllocationBase; public uint AllocationProtect; public ushort PartitionId; public UIntPtr RegionSize; public uint State, Protect, Type; }
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] static extern UIntPtr VirtualQueryEx(IntPtr h, IntPtr addr, out MBI mbi, UIntPtr len);
  public static List<object[]> Walk(int pid) {
    var res = new List<object[]>();
    IntPtr h = OpenProcess(0x0400 | 0x0010, false, pid);
    if (h == IntPtr.Zero) return res;
    long addr = 0; MBI m;
    while (VirtualQueryEx(h, (IntPtr)addr, out m, (UIntPtr)Marshal.SizeOf(typeof(MBI))) != UIntPtr.Zero) {
      long size = (long)m.RegionSize;
      if (m.State == 0x1000) res.Add(new object[] { (long)m.BaseAddress, size, m.Type, m.Protect });
      addr = (long)m.BaseAddress + size;
      if (addr >= 0x7FFFFFFF0000L) break;
    }
    CloseHandle(h);
    return res;
  }
}
'@

$started = $null
if ($ExePath) {
    $sp = @{ FilePath = $ExePath; PassThru = $true }
    if ($ExeArgs.Count) { $sp.ArgumentList = $ExeArgs }
    $started = Start-Process @sp
    $ProcessId = $started.Id
    Start-Sleep -Seconds $SettleSeconds
}
try {
    $regs = [Vm]::Walk($ProcessId)
    $kind = @{ 0x20000 = 'private'; 0x1000000 = 'image'; 0x40000 = 'mapped' }
    $groups = $regs | Group-Object { $kind[[int]$_[2]] } | ForEach-Object {
        [pscustomobject]@{ Kind = $_.Name; Regions = $_.Count; CommittedMB = [math]::Round((($_.Group | ForEach-Object { $_[1] }) | Measure-Object -Sum).Sum / 1MB, 1) }
    }
    $groups | Format-Table -AutoSize
    "Top $Top private committed regions:"
    $regs | Where-Object { $_[2] -eq 0x20000 } | Sort-Object { $_[1] } -Descending | Select-Object -First $Top | ForEach-Object {
        [pscustomobject]@{ Base = '0x{0:X}' -f $_[0]; SizeMB = [math]::Round($_[1] / 1MB, 2); Protect = '0x{0:X}' -f $_[3] }
    } | Format-Table -AutoSize
    $p = Get-Process -Id $ProcessId
    "WS {0:N1} MB, PrivateMemorySize64 {1:N1} MB" -f ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB)
} finally {
    if ($started -and -not $started.HasExited) { Stop-Process -Id $started.Id }
}
