<#
.SYNOPSIS
    Launches an exe, waits, saves a screenshot of its main window (screen copy) to a PNG, closes it.
.EXAMPLE
    pwsh -File bench/shot.ps1 -Exe target/release/selachi.exe -Out shot.png
#>
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Out, [int]$WaitSeconds = 4, [string[]]$ExeArgs = @(), [string]$Keys = '')
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class ShotNative {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
}
'@
$sp = @{ FilePath = $Exe; PassThru = $true }
if ($ExeArgs.Count) { $sp.ArgumentList = $ExeArgs }
$p = Start-Process @sp
try {
    Start-Sleep -Seconds $WaitSeconds
    $p.Refresh()
    $h = $p.MainWindowHandle
    if ($h -eq 0) { throw 'no main window' }
    if ($Keys) {
        Add-Type -AssemblyName System.Windows.Forms
        [void][ShotNative]::SetForegroundWindow($h)
        Start-Sleep -Milliseconds 500
        [System.Windows.Forms.SendKeys]::SendWait($Keys)
        Start-Sleep -Seconds 2
    }
    $r = New-Object ShotNative+RECT
    [void][ShotNative]::DwmGetWindowAttribute($h, 9, [ref]$r, 16)   # DWMWA_EXTENDED_FRAME_BOUNDS
    $w = $r.R - $r.L; $ht = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    "saved $Out ($w x $ht)"
} finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id } }
