<#
.SYNOPSIS
  Builds Selachi's grammar plugins (one cdylib per tree-sitter grammar).

.DESCRIPTION
  1. cargo build --release of every crate in this (separate) workspace.
  2. For each plugin writes dist/plugins/<id>/ with
       grammar.dll      the compiled grammar (exports the grammar's C entry point)
       highlights.scm   queries copied from the upstream grammar crate(s)
       injections.scm / locals.scm (when the plugin has them)
       LICENSE.txt      the upstream grammar's license (attribution)
       manifest.toml    name, version, extensions, tree-sitter ABI, symbol, sha256 of every file
  3. Zips each plugin folder (files at the zip root) into dist/<id>-<version>.zip.
  4. Writes dist/grammars.toml, the index the app downloads: lang, version, url, sha256 and
     size of each zip.

  The plugin table (symbols, extensions, which upstream query files to concatenate) is
  plugins.json. PowerShell's highlights.scm is the one upstream ships inside the
  tree-sitter-powershell crate (airbus-cert, MIT); its LICENSE is included.

.PARAMETER BaseUrl
  URL prefix of the published zips (placeholder by default; CI overrides it with the
  GitHub release asset base).

.PARAMETER Only
  Build just these plugin ids (e.g. -Only csharp,rust).

.EXAMPLE
  ./build.ps1
  ./build.ps1 -BaseUrl https://github.com/ORG/selachi/releases/download/grammars-v1 -Only csharp
#>
[CmdletBinding()]
param(
    [string]$BaseUrl = 'https://github.com/YewdielMartinez/Selachi/releases/download/grammars-v1',
    [string[]]$Only = @()
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$utf8 = New-Object System.Text.UTF8Encoding $false
function Write-Text([string]$Path, [string]$Text) {
    [System.IO.File]::WriteAllText($Path, $Text.Replace("`r`n", "`n"), $utf8)
}
function Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

$plugins = Get-Content -Raw plugins.json | ConvertFrom-Json
if ($Only.Count -gt 0) { $plugins = @($plugins | Where-Object { $Only -contains $_.id }) }
if ($plugins.Count -eq 0) { throw 'no plugins selected' }

# --- 1. build ------------------------------------------------------------------------
$pkgArgs = @()
foreach ($p in $plugins) { $pkgArgs += @('-p', "selachi-grammar-$($p.id)") }
Write-Host '==> cargo build --release' -ForegroundColor Cyan
cargo build --release @pkgArgs
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

# --- upstream crate locations (versions, queries, licenses) ----------------------------
$meta = cargo metadata --format-version 1 | ConvertFrom-Json
$crates = @{}
foreach ($pkg in $meta.packages) { $crates[$pkg.name] = $pkg }
function Crate-Dir([string]$Name) {
    if (-not $crates.ContainsKey($Name)) { throw "crate $Name not found in cargo metadata" }
    Split-Path -Parent $crates[$Name].manifest_path
}

function Combine-Queries($Items) {
    $parts = foreach ($i in $Items) {
        $f = Join-Path (Crate-Dir $i.crate) $i.file
        if (-not (Test-Path $f)) { throw "missing query $f" }
        Get-Content -Raw -LiteralPath $f
    }
    ($parts -join "`n")
}

function Grammar-Abi([string]$CrateDir, [string]$Id) {
    foreach ($c in @("$CrateDir/src/parser.c", "$CrateDir/$Id/src/parser.c")) {
        if (Test-Path $c) {
            $m = Select-String -Path $c -Pattern '#define\s+LANGUAGE_VERSION\s+(\d+)' | Select-Object -First 1
            if ($m) { return [int]$m.Matches[0].Groups[1].Value }
        }
    }
    throw "could not read LANGUAGE_VERSION for $Id"
}

# --- 2/3. plugin folders and zips ------------------------------------------------------
$dist = Join-Path $PSScriptRoot 'dist'
$pluginsDir = Join-Path $dist 'plugins'
if (Test-Path $pluginsDir) { Remove-Item -Recurse -Force $pluginsDir }
New-Item -ItemType Directory -Force $pluginsDir | Out-Null
Get-ChildItem $dist -Filter '*.zip' -ErrorAction SilentlyContinue | Remove-Item -Force

$index = New-Object System.Collections.Generic.List[string]
$report = @()
foreach ($p in $plugins) {
    $id = $p.id
    $crateDir = Crate-Dir $p.crate
    $version = $crates[$p.crate].version
    $out = Join-Path $pluginsDir $id
    New-Item -ItemType Directory -Force $out | Out-Null

    $dll = Join-Path 'target/release' ("selachi_grammar_$id.dll")
    if (-not (Test-Path $dll)) { throw "missing $dll" }
    Copy-Item $dll (Join-Path $out 'grammar.dll')
    Write-Text (Join-Path $out 'highlights.scm') (Combine-Queries $p.highlights)
    if (@($p.injections).Count -gt 0) { Write-Text (Join-Path $out 'injections.scm') (Combine-Queries $p.injections) }
    if (@($p.locals).Count -gt 0) { Write-Text (Join-Path $out 'locals.scm') (Combine-Queries $p.locals) }
    $lic = Join-Path (Crate-Dir $p.license_crate) 'LICENSE'
    if (Test-Path $lic) { Copy-Item $lic (Join-Path $out 'LICENSE.txt') }

    $files = Get-ChildItem $out -File | Sort-Object Name
    $size = ($files | Measure-Object Length -Sum).Sum
    $abi = Grammar-Abi $crateDir $id
    $exts = ($p.extensions | ForEach-Object { '"' + $_ + '"' }) -join ', '
    $names = ($p.names | ForEach-Object { '"' + $_ + '"' }) -join ', '
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.AppendLine("name = `"$id`"")
    [void]$sb.AppendLine("version = `"$version`"")
    [void]$sb.AppendLine("display_name = `"$($p.display_name)`"")
    [void]$sb.AppendLine("file_extensions = [$exts]")
    [void]$sb.AppendLine("names = [$names]")
    [void]$sb.AppendLine("tree_sitter_abi = $abi")
    [void]$sb.AppendLine("symbol = `"$($p.symbol)`"")
    [void]$sb.AppendLine("size_bytes = $size")
    [void]$sb.AppendLine('')
    [void]$sb.AppendLine('[files]')
    foreach ($f in $files) { [void]$sb.AppendLine("`"$($f.Name)`" = `"$(Sha256 $f.FullName)`"") }
    Write-Text (Join-Path $out 'manifest.toml') $sb.ToString()

    $zip = Join-Path $dist "$id-$version.zip"
    Compress-Archive -Path (Join-Path $out '*') -DestinationPath $zip -CompressionLevel Optimal
    $zipSize = (Get-Item $zip).Length
    $zipSha = Sha256 $zip

    [void]$index.Add('[[grammar]]')
    [void]$index.Add("lang = `"$id`"")
    [void]$index.Add("version = `"$version`"")
    [void]$index.Add("url = `"$($BaseUrl.TrimEnd('/'))/$id-$version.zip`"")
    [void]$index.Add("sha256 = `"$zipSha`"")
    [void]$index.Add("size = $zipSize")
    [void]$index.Add("display_name = `"$($p.display_name)`"")
    [void]$index.Add("file_extensions = [$exts]")
    [void]$index.Add('')

    $report += [pscustomobject]@{
        Plugin  = $id
        Version = $version
        ABI     = $abi
        'DLL MB' = [math]::Round((Get-Item (Join-Path $out 'grammar.dll')).Length / 1MB, 2)
        'Zip MB' = [math]::Round($zipSize / 1MB, 2)
    }
}

# --- 4. index ----------------------------------------------------------------------------
Write-Text (Join-Path $dist 'grammars.toml') (($index -join "`n"))
Write-Host "==> dist/ ready" -ForegroundColor Green
$report | Format-Table -AutoSize
