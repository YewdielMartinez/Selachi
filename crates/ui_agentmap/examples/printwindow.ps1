<#
.SYNOPSIS
    Launches an exe, waits, saves a screenshot of its main window with PrintWindow and closes it.
    No synthetic input at all: nothing is typed, clicked or moved (it cannot write into the user's apps).
    The window is captured from its own surface, so other windows on top of it do not matter.
.EXAMPLE
    pwsh -File crates/ui_agentmap/examples/printwindow.ps1 -Exe target/debug/examples/agentmap_demo.exe `
        -Out docs/screenshots/w7-agentmap-tree-dark.png -ExeArgs "--mode tree"
#>
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Out,
    [string]$ExeArgs = "",
    [int]$WaitSeconds = 5
)
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class PwNative {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
}
'@
$sp = @{ FilePath = $Exe; PassThru = $true }
if ($ExeArgs) { $sp.ArgumentList = $ExeArgs }
$p = Start-Process @sp
try {
    Start-Sleep -Seconds $WaitSeconds
    $p.Refresh()
    $h = $p.MainWindowHandle
    if ($h -eq 0) { throw 'no main window' }
    $r = New-Object PwNative+RECT
    [void][PwNative]::DwmGetWindowAttribute($h, 9, [ref]$r, 16)   # DWMWA_EXTENDED_FRAME_BOUNDS
    $w = $r.R - $r.L; $ht = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    # PW_RENDERFULLCONTENT (2): also captures DirectX / composition content
    $ok = [PwNative]::PrintWindow($h, $hdc, 2)
    $g.ReleaseHdc($hdc)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    "saved $Out ($w x $ht) PrintWindow=$ok"
} finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id } }
