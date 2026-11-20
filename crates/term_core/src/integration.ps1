# Selachi shell integration for PowerShell (pwsh 7 and Windows PowerShell 5.1).
#
# Selachi runs this after your profile when it starts the shell; it never edits your profile.
# It chains whatever `prompt` function you already have and only adds invisible sequences:
#   OSC 7    file://host/path  the current directory (percent-encoded)
#   OSC 133  A prompt start, B command start, C command executed, D;<exit code> finished
# ASCII only on purpose: Windows PowerShell 5.1 reads BOM-less files as ANSI.

if ($global:__selachi_integration) { return }

$global:__selachi_integration = @{
    Prompt   = $function:prompt   # the prompt in effect right now (profile, oh-my-posh, ...)
    ReadLine = $null
    LastId   = -1
}

function global:prompt {
    $ok = $?
    $exit = $global:LASTEXITCODE
    $s = $global:__selachi_integration
    $esc = [char]27
    $bel = [char]7
    $marks = ''

    # D: only when a command actually ran since the previous prompt (empty Enter adds nothing).
    $h = Get-History -Count 1 -ErrorAction SilentlyContinue
    $id = if ($h) { $h.Id } else { 0 }
    if ($s.LastId -ne -1 -and $id -ne $s.LastId) {
        $code = 0
        if (-not $ok) { $code = if (($exit -is [int]) -and $exit -ne 0) { $exit } else { 1 } }
        $marks += "$esc]133;D;$code$bel"
    }
    $s.LastId = $id

    # 7: current directory as a file URI.
    $loc = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($loc.Provider.Name -eq 'FileSystem') {
        $p = $loc.ProviderPath -replace '\\', '/'
        $enc = (($p -split '/' | ForEach-Object { [uri]::EscapeDataString($_) }) -join '/') -replace '^([A-Za-z])%3A', '$1:'
        $uri = if ($enc.StartsWith('//')) { 'file:' + $enc } else { 'file://' + $env:COMPUTERNAME + '/' + $enc }
        $marks += "$esc]7;$uri$bel"
    }

    # A goes straight to the console so it stays in order with anything the original prompt
    # writes with Write-Host; B ends the returned prompt string.
    [Console]::Write("$marks$esc]133;A$bel")

    # Restore the failed state so prompts that read $? / $LASTEXITCODE (git, posh, starship) see it.
    if (-not $ok) { Write-Error 'selachi' -ErrorAction Ignore }
    $out = $null
    try { $out = & $s.Prompt } catch { $out = "PS $($loc.Path)> " }
    $global:LASTEXITCODE = $exit
    if ($out -is [array]) { $out = $out -join '' }
    "$out$esc]133;B$bel"
}

# C: emitted when the command line is submitted. PSReadLine is the only place that sees Enter.
$rl = Get-Command PSConsoleHostReadLine -CommandType Function -ErrorAction SilentlyContinue
if ($rl) {
    $global:__selachi_integration.ReadLine = $rl
    function global:PSConsoleHostReadLine {
        $line = & $global:__selachi_integration.ReadLine
        if ($line -and $line.Trim()) { [Console]::Write("$([char]27)]133;C$([char]7)") }
        $line
    }
}
Remove-Variable rl -ErrorAction SilentlyContinue
