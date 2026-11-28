<#
.SYNOPSIS
  Builds the Selachi installer (Inno Setup, per-user, no admin) into packaging/installer/dist/.

.DESCRIPTION
  1. cargo build --release -p selachi -p agent_harness   (one shared target dir; skip with -SkipBuild)
  2. Stages packaging/installer/stage/ :
       selachi.exe, selachi-agent.exe, selachi.ico,
       assets/onboarding/shark.gif   (the exe looks for <exe dir>\assets\onboarding\shark.gif),
       LICENSE.txt, THIRD-PARTY-NOTICES.txt, THIRD-PARTY-LICENSES.txt (generated from cargo tree),
       licenses/ (OFL fonts, Apache-2.0 gpui and alacritty_terminal)
  3. Runs ISCC on selachi.iss -> dist/Selachi-Setup-<version>.exe, and writes dist/SHA256SUMS.txt.

  Nothing is installed on this machine and nothing is signed unless -SignCommand is given.

.PARAMETER Version
  Release version (default: the `selachi` crate version). A leading "v" is dropped, so CI can pass the tag.

.PARAMETER SkipBuild
  Do not run cargo; stage the existing target\release exes (they must exist).

.PARAMETER IsccPath
  Path to ISCC.exe. Otherwise looked up in: $env:ISCC, PATH, %LOCALAPPDATA%\Programs\Inno Setup 6|7,
  %ProgramFiles(x86)%\Inno Setup 6|7, %ProgramFiles%\Inno Setup 6|7.

.PARAMETER NoContextMenu
  Leave out the opt-in "Abrir en Selachi" Explorer task.

.PARAMETER SignCommand
  Code-signing command WITHOUT the file argument, e.g.
    'signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /f C:\cert.pfx /p ****'
  When given, selachi.exe, selachi-agent.exe, the setup and the uninstaller are signed. Without it nothing is signed.

.EXAMPLE
  ./packaging/installer/build.ps1
  ./packaging/installer/build.ps1 -Version v0.2.0 -SkipBuild
#>
[CmdletBinding()]
param(
    [string]$Version = '',
    [switch]$SkipBuild,
    [string]$IsccPath = '',
    [switch]$NoContextMenu,
    [string]$SignCommand = ''
)

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$repo = (Resolve-Path (Join-Path $here '..\..')).Path
$stage = Join-Path $here 'stage'
$dist = Join-Path $here 'dist'
# Cargo's default target dir: the whole workspace shares ONE (do not set CARGO_TARGET_DIR here).
$targetRel = Join-Path $repo 'target\release'

function Step([string]$m) { Write-Host "==> $m" -ForegroundColor Cyan }

# --- version ------------------------------------------------------------------------------
if (-not $Version) {
    $m = Select-String -Path (Join-Path $repo 'crates\app\Cargo.toml') -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $m) { throw 'could not read the selachi version from crates/app/Cargo.toml' }
    $Version = $m.Matches[0].Groups[1].Value
}
$Version = $Version.TrimStart('v')
if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)') { throw "version '$Version' is not x.y.z[-suffix]" }
$numeric = '{0}.{1}.{2}.0' -f $Matches[1], $Matches[2], $Matches[3]
Write-Host "Selachi $Version (numeric $numeric)"

# --- ISCC ---------------------------------------------------------------------------------
function Find-Iscc {
    if ($IsccPath) { if (Test-Path $IsccPath) { return (Resolve-Path $IsccPath).Path } else { throw "ISCC not found at $IsccPath" } }
    $cands = @()
    if ($env:ISCC) { $cands += $env:ISCC }
    $cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($cmd) { $cands += $cmd.Source }
    foreach ($root in @(($env:LOCALAPPDATA + '\Programs'), ${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if (-not $root) { continue }
        foreach ($v in '7', '6') { $cands += (Join-Path $root "Inno Setup $v\ISCC.exe") }
    }
    foreach ($c in $cands) { if ($c -and (Test-Path $c)) { return (Resolve-Path $c).Path } }
    throw @'
ISCC.exe (Inno Setup) not found. Install it WITHOUT administrator (per user):
  1. Download innosetup-<ver>-x64.exe from https://jrsoftware.org/isdl.php (check its Authenticode signature)
  2. innosetup-<ver>-x64.exe /CURRENTUSER /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-
     (installs to %LOCALAPPDATA%\Programs\Inno Setup 6|7)
or pass -IsccPath <path to ISCC.exe>, or set $env:ISCC.
'@
}
$iscc = Find-Iscc
Write-Host "ISCC: $iscc"

# --- 1. cargo -----------------------------------------------------------------------------
Push-Location $repo
try {
    if (-not $SkipBuild) {
        Step 'cargo build --release -p selachi -p agent_harness'
        cargo build --release -p selachi -p agent_harness
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed (use -SkipBuild to package the existing target\release exes)' }
    }
    foreach ($e in 'selachi.exe', 'selachi-agent.exe') {
        $p = Join-Path $targetRel $e
        if (-not (Test-Path $p)) { throw "missing $p (run without -SkipBuild)" }
        $i = Get-Item $p
        Write-Host ('  {0,-18} {1,10:N0} bytes  built {2:u}' -f $e, $i.Length, $i.LastWriteTimeUtc)
    }

    # --- 2. stage -------------------------------------------------------------------------
    Step 'staging'
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage, "$stage\assets\onboarding", "$stage\licenses" | Out-Null
    Copy-Item "$targetRel\selachi.exe", "$targetRel\selachi-agent.exe" $stage
    Copy-Item (Join-Path $repo 'assets\app-icon\icon.ico') "$stage\selachi.ico"
    Copy-Item (Join-Path $repo 'assets\onboarding\shark.gif') "$stage\assets\onboarding\shark.gif"

    $lic = Join-Path $repo 'LICENSE'
    if (-not (Test-Path $lic)) { $lic = Join-Path $here 'LICENSE.txt' }
    Copy-Item $lic "$stage\LICENSE.txt"
    Copy-Item (Join-Path $here 'THIRD-PARTY-NOTICES.txt') $stage

    # Licenses that must travel with the binary (fonts are embedded in selachi.exe).
    Copy-Item (Join-Path $repo 'assets\fonts\OFL-DMSans.txt') "$stage\licenses\OFL-DMSans.txt"
    Copy-Item (Join-Path $repo 'assets\fonts\OFL-GeistMono.txt') "$stage\licenses\OFL-GeistMono.txt"
    Copy-Item (Join-Path $repo 'vendor\gpui\LICENSE-APACHE') "$stage\licenses\LICENSE-APACHE-gpui.txt"
    $meta = cargo metadata --format-version 1 --offline | ConvertFrom-Json
    $alac = $meta.packages | Where-Object { $_.name -eq 'alacritty_terminal' } | Select-Object -First 1
    if (-not $alac) { throw 'alacritty_terminal not found in cargo metadata' }
    $alacLic = Join-Path (Split-Path -Parent $alac.manifest_path) 'LICENSE-APACHE'
    if (-not (Test-Path $alacLic)) { throw "missing $alacLic" }
    Copy-Item $alacLic "$stage\licenses\LICENSE-APACHE-alacritty_terminal.txt"

    # Index of every shipped dependency and its license (from the resolved graph, Windows target).
    Step 'dependency license index'
    $lines = cargo tree -p selachi -p agent_harness --prefix none --format '{p}|{l}|{r}' -e normal --target x86_64-pc-windows-msvc --offline
    if ($LASTEXITCODE -ne 0) { throw 'cargo tree failed' }
    $rows = foreach ($l in $lines) {
        $l = $l -replace ' \(\*\)$', '' -replace ' \(proc-macro\)', ''
        $f = $l -split '\|', 3
        if ($f.Count -lt 3 -or $f[0] -match '\((?:[A-Za-z]:\\|/)') { continue }  # workspace/path crates
        '{0}  |  {1}  |  {2}' -f $f[0], ($(if ($f[1]) { $f[1] } else { 'see the crate' })), $f[2]
    }
    $rows = $rows | Sort-Object -Unique
    $hdr = "Selachi $Version - third-party Rust crates linked into the executables`r`n" +
           "(name version | SPDX license | repository). Generated by packaging/installer/build.ps1 from cargo tree.`r`n" +
           "Full license texts: see the licenses folder and each crate's repository.`r`n`r`n"
    [System.IO.File]::WriteAllText("$stage\THIRD-PARTY-LICENSES.txt", $hdr + ($rows -join "`r`n") + "`r`n", (New-Object System.Text.UTF8Encoding $false))
    Write-Host ("  {0} crates indexed" -f @($rows).Count)

    # --- optional signing of the binaries ----------------------------------------------------
    if ($SignCommand) {
        Step 'signing binaries'
        foreach ($e in 'selachi.exe', 'selachi-agent.exe') {
            Invoke-Expression "$SignCommand `"$stage\$e`""
            if ($LASTEXITCODE -ne 0) { throw "signing $e failed" }
        }
    }
}
finally { Pop-Location }

# --- 3. installer -------------------------------------------------------------------------
Step 'ISCC'
New-Item -ItemType Directory -Force $dist | Out-Null
Get-ChildItem $dist -Filter 'Selachi-Setup-*.exe' -ErrorAction SilentlyContinue | Remove-Item -Force
$isccArgs = @("/DAppVersion=$Version", "/DAppVersionNumeric=$numeric", "/DStageDir=$stage", "/DOutDir=$dist", '/Q')
if ($NoContextMenu) { $isccArgs += '/DNoContextMenu' }
if ($SignCommand) { $isccArgs += '/DSign'; $isccArgs += "/Sselachi=$SignCommand `$f" }
& $iscc @isccArgs (Join-Path $here 'selachi.iss')
if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }

$setup = Join-Path $dist "Selachi-Setup-$Version.exe"
if (-not (Test-Path $setup)) { throw "expected $setup" }
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $setup).Hash.ToLowerInvariant()
"$hash  Selachi-Setup-$Version.exe" | Set-Content -Encoding ascii (Join-Path $dist 'SHA256SUMS.txt')

Step 'done'
Get-Item $setup | ForEach-Object { '{0}  {1:N0} bytes ({2:N2} MiB)' -f $_.FullName, $_.Length, ($_.Length / 1MB) }
Write-Host "sha256 $hash"
Write-Host 'Stage size (uncompressed):' ('{0:N2} MiB' -f ((Get-ChildItem $stage -Recurse -File | Measure-Object Length -Sum).Sum / 1MB))
if (-not $SignCommand) { Write-Host 'NOTE: not code-signed (no -SignCommand): SmartScreen will warn on first run.' -ForegroundColor Yellow }
