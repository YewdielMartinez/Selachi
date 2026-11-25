<#
.SYNOPSIS
    Launches an exe, waits, saves its main window to a PNG through PrintWindow (works while the window is
    covered, sends no input), and closes it. Falls back to a screen copy when PrintWindow returns a blank frame.
.EXAMPLE
    pwsh -File bench/shot-pw.ps1 -Exe target/debug/examples/chat_demo.exe -Out shot.png -ExeArgs dark,--select,menu
#>
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Out, [int]$WaitSeconds = 5, [string[]]$ExeArgs = @())
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class PwNative {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
}
'@
[void][PwNative]::SetProcessDPIAware()
$ExeArgs = @($ExeArgs | ForEach-Object { $_ -split ',' } | Where-Object { $_ })   # -File passes a,b,c as one string
$sp = @{ FilePath = $Exe; PassThru = $true }
if ($ExeArgs.Count) { $sp.ArgumentList = $ExeArgs }
$p = Start-Process @sp
try {
    Start-Sleep -Seconds $WaitSeconds
    $p.Refresh()
    $h = $p.MainWindowHandle
    if ($h -eq 0) { throw 'no main window' }
    $r = New-Object PwNative+RECT
    [void][PwNative]::GetWindowRect($h, [ref]$r)
    $w = $r.R - $r.L; $ht = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc()
    [void][PwNative]::PrintWindow($h, $dc, 2)   # PW_RENDERFULLCONTENT
    $g.ReleaseHdc($dc)
    # Blank check: sample a grid of pixels.
    $distinct = @{}
    foreach ($x in 0..9) { foreach ($y in 0..9) { $distinct[$bmp.GetPixel([int]($x * ($w - 1) / 9), [int]($y * ($ht - 1) / 9)).ToArgb()] = 1 } }
    if ($distinct.Count -lt 3) {
        $e = New-Object PwNative+RECT
        [void][PwNative]::DwmGetWindowAttribute($h, 9, [ref]$e, 16)
        $w = $e.R - $e.L; $ht = $e.B - $e.T
        $bmp = New-Object System.Drawing.Bitmap $w, $ht
        $g2 = [System.Drawing.Graphics]::FromImage($bmp)
        $g2.CopyFromScreen($e.L, $e.T, 0, 0, $bmp.Size)
        'PrintWindow was blank: used a screen copy'
    }
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    "saved $Out ($w x $ht)"
} finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id } }
