# Selachi shell integration for PowerShell (pwsh 7 and Windows PowerShell 5.1).
#
# Selachi runs this after your profile when it starts the shell; it never edits your profile.
# It chains whatever `prompt` function you already have and only adds invisible sequences:
#   OSC 7    file://host/path  the current directory (percent-encoded)
#   OSC 133  A prompt start, B command start, C command executed, D;<exit code> finished
#   OSC 633  E;<command>  the exact command line, sent just before C (VS Code's format:
#            `\` is written `\\`; control chars, `;` and every non-ASCII byte are written `\xNN`,
#            one escape per UTF-8 byte). Selachi uses it for the command blocks because reading
#            the screen between B and C is unreliable with PSReadLine.
# ASCII only on purpose: Windows PowerShell 5.1 reads BOM-less files as ANSI.
#
# PSReadLine settings (only when PSReadLine is loaded; every call is guarded, so a failure never
# breaks the prompt). Selachi passes them as environment variables; a variable that is absent
# means "default":
#   SELACHI_TAB_MODE         smart (default) | complete | menu | native
#       smart     Tab accepts the gray inline suggestion if there is one, else opens MenuComplete
#       complete  Tab = TabCompleteNext (cycles)          menu    Tab = MenuComplete
#       native    leave the Tab binding alone
#       Shift+Tab stays TabCompletePrevious (it is only set explicitly in `complete` mode).
#   SELACHI_PREDICTION       history (default) | history_and_plugin | off | native
#       -> Set-PSReadLineOption -PredictionSource (skipped when the version has no predictions)
#   SELACHI_PREDICTION_VIEW  inline (default) | list | native   -> -PredictionViewStyle
#   SELACHI_ACCEPT_WORD_KEY  chord, default Ctrl+RightArrow; accepts the next word of the
#                            suggestion (falls back to ForwardWord when there is none).
#                            Empty or `native` leaves the key alone.
# Bindings you set in your own profile are respected when the variable is absent (implicit
# default): Tab / the accept-word key are only replaced if they still have the stock PSReadLine
# binding. When the variable is set explicitly (anything but `native`) Selachi's choice wins.

if ($global:__selachi_integration) { return }

$global:__selachi_integration = @{
    Prompt   = $function:prompt   # the prompt in effect right now (profile, oh-my-posh, ...)
    ReadLine = $null
    LastId   = -1
    Escape   = {
        param([string]$text)
        if ($text.Length -gt 4096) { $text = $text.Substring(0, 4096) }
        $sb = [System.Text.StringBuilder]::new()
        foreach ($b in [System.Text.Encoding]::UTF8.GetBytes($text)) {
            if ($b -eq 92) { [void]$sb.Append('\\') }
            elseif ($b -lt 32 -or $b -gt 126 -or $b -eq 59) { [void]$sb.Append(('\x{0:x2}' -f $b)) }
            else { [void]$sb.Append([char]$b) }
        }
        $sb.ToString()
    }
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

# E + C: emitted when the command line is submitted. PSReadLine is the only place that sees Enter.
# The original function's scriptblock is kept (calling the CommandInfo would resolve the name
# again and recurse into the wrapper).
$rl = Get-Command PSConsoleHostReadLine -CommandType Function -ErrorAction SilentlyContinue
if ($rl) {
    $global:__selachi_integration.ReadLine = $rl.ScriptBlock
    function global:PSConsoleHostReadLine {
        $line = & $global:__selachi_integration.ReadLine
        if ($line -and $line.Trim()) {
            $cmd = & $global:__selachi_integration.Escape $line
            [Console]::Write("$([char]27)]633;E;$cmd$([char]7)$([char]27)]133;C$([char]7)")
        }
        $line
    }
}

# PSReadLine settings from the environment (see the header). Runs in its own scope.
if ($rl) {
    & {
        $mode = "$env:SELACHI_TAB_MODE".Trim().ToLowerInvariant()
        $pred = "$env:SELACHI_PREDICTION".Trim().ToLowerInvariant()
        $view = "$env:SELACHI_PREDICTION_VIEW".Trim().ToLowerInvariant()
        $wordKeyVar = $env:SELACHI_ACCEPT_WORD_KEY
        $explicitTab = [bool]$mode
        if (-not $mode) { $mode = 'smart' }
        if (-not $pred) { $pred = 'history' }
        if (-not $view) { $view = 'inline' }
        $explicitWord = [bool]("$wordKeyVar".Trim())
        $wordKey = if ($explicitWord) { "$wordKeyVar".Trim() } else { 'Ctrl+RightArrow' }

        $psrl = Get-Module PSReadLine
        if (-not $psrl) { return }
        $ver = $psrl.Version
        $type = [Microsoft.PowerShell.PSConsoleReadLine]
        $setOpt = Get-Command Set-PSReadLineOption -ErrorAction SilentlyContinue
        if (-not $setOpt -or $ver -lt [version]'2.0.0') { return }

        # What is bound to a key right now (Function is the handler name or its description).
        $current = {
            param($key)
            try { (Get-PSReadLineKeyHandler -Bound | Where-Object { $_.Key -eq $key } | Select-Object -First 1).Function } catch { $null }
        }
        $hasWord = $null -ne $type.GetMethod('AcceptNextSuggestionWord')

        # --- Prediction source / view ---
        if ($setOpt.Parameters.ContainsKey('PredictionSource')) {
            try {
                switch ($pred) {
                    'history' { Set-PSReadLineOption -PredictionSource History }
                    'history_and_plugin' {
                        try { Set-PSReadLineOption -PredictionSource HistoryAndPlugin }
                        catch { Set-PSReadLineOption -PredictionSource History }
                    }
                    'off' { Set-PSReadLineOption -PredictionSource None }
                }
            } catch { }
        }
        if ($setOpt.Parameters.ContainsKey('PredictionViewStyle')) {
            try {
                switch ($view) {
                    'inline' { Set-PSReadLineOption -PredictionViewStyle InlineView }
                    'list' { Set-PSReadLineOption -PredictionViewStyle ListView }
                }
            } catch { }
        }

        # --- Tab ---
        $tabFn = & $current 'Tab'
        $stockTab = (-not $tabFn) -or ($tabFn -in 'TabCompleteNext', 'Complete', 'SelachiSmartTab')
        if ($mode -ne 'native' -and ($explicitTab -or $stockTab)) {
            try {
                switch ($mode) {
                    'complete' {
                        Set-PSReadLineKeyHandler -Key Tab -Function TabCompleteNext
                        Set-PSReadLineKeyHandler -Key Shift+Tab -Function TabCompletePrevious
                    }
                    'menu' { Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete }
                    default {
                        # smart: accept the inline suggestion when one is visible, else the menu.
                        Set-PSReadLineKeyHandler -Key Tab -BriefDescription SelachiSmartTab `
                            -Description 'Accept the inline suggestion, or open the completion menu' `
                            -ScriptBlock {
                                param($key, $arg)
                                $psrl = [Microsoft.PowerShell.PSConsoleReadLine]
                                $before = $null; $c1 = 0
                                $psrl::GetBufferState([ref]$before, [ref]$c1)
                                try { $psrl::AcceptSuggestion($key, $arg) } catch { }
                                $after = $null; $c2 = 0
                                $psrl::GetBufferState([ref]$after, [ref]$c2)
                                if ($after -ceq $before) { $psrl::MenuComplete($key, $arg) }
                            }
                    }
                }
            } catch { }
        }

        # --- Accept the next word of the suggestion ---
        if ($hasWord -and $wordKey -and $wordKey -ne 'native') {
            $wordFn = & $current $wordKey
            $stockWord = (-not $wordFn) -or ($wordFn -in 'NextWord', 'ForwardWord', 'SelachiAcceptWord')
            if ($explicitWord -or $stockWord) {
                try {
                    Set-PSReadLineKeyHandler -Chord $wordKey -BriefDescription SelachiAcceptWord `
                        -Description 'Accept the next word of the suggestion, else move forward one word' `
                        -ScriptBlock {
                            param($key, $arg)
                            $psrl = [Microsoft.PowerShell.PSConsoleReadLine]
                            $before = $null; $c1 = 0
                            $psrl::GetBufferState([ref]$before, [ref]$c1)
                            try { $psrl::AcceptNextSuggestionWord($key, $arg) } catch { }
                            $after = $null; $c2 = 0
                            $psrl::GetBufferState([ref]$after, [ref]$c2)
                            if ($after -ceq $before) { $psrl::ForwardWord($key, $arg) }
                        }
                } catch { }
            }
        }
    }
}
Remove-Variable rl -ErrorAction SilentlyContinue
