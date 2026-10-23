# local-agent.ps1: the Windows port of local-agent, which hands a Claude Code
# task to Strata. Starts a headless `claude -p` whose API is the Proxy on the
# NAS, so Strata runs the whole agent loop. It behaves like the Bash runner next
# to it (same commands, exit codes and output); a change to one goes into both.
#
# Runs on Windows PowerShell 5.1 and PowerShell 7; needs only git and claude.
# The tests run it on Linux under pwsh, so what differs by OS has a branch for
# both.
$ErrorActionPreference = 'Stop'

$usage = @'
Usage: local-agent run [--read-only] [--watch] [--allow RULE]... [--timeout TIME] [TASK]
       local-agent status [RUN]
       local-agent result [RUN]
       local-agent discard RUN
       local-agent check
       local-agent prune

run reads TASK from stdin when it isn't given. Exit codes of run:
  0  done: review the result (local-agent result RUN)
  3  fall back to a normal agent; the reason is in local-agent result RUN
  4  timed out; the worktree and log are kept
  1  error of local-agent itself, 2 wrong usage or configuration

Configuration: %APPDATA%\local-agent\env (readable only by you) with
  LOCAL_AGENT_URL=http://<NAS>:<port>   the Proxy
  LOCAL_AGENT_KEY=<key from Access in the Proxy's web UI>
'@

# $IsWindows only exists from PowerShell 6 on; 5.1 runs only on Windows.
$onWindows = -not (Test-Path variable:IsWindows) -or $IsWindows

# %APPDATA% on Windows; the XDG folder elsewhere, as the Bash runner uses it.
function Get-BaseDir([string]$windowsVar, [string]$xdgVar, [string]$fallback) {
    $dir = [Environment]::GetEnvironmentVariable($windowsVar)
    if (-not $dir) { $dir = [Environment]::GetEnvironmentVariable($xdgVar) }
    if (-not $dir) { $dir = Join-Path $HOME $fallback }
    return $dir
}
$configFile = Join-Path (Join-Path (Get-BaseDir 'APPDATA' 'XDG_CONFIG_HOME' '.config') 'local-agent') 'env'

# Compaction starts at this share of Strata's window, in percent
# (LOCAL_AGENT_COMPACT_SHARE in the configuration).
$compactShare = 75
$outputTokens = 8192
# Claude Code compacts this many tokens below its window, on top of the output
# cap: measured on 2026-10-19 with Claude Code 2.1.218 in -p mode against Strata
# (docs/research/fit-max-tokens-and-autocompact.md, "Hardware test").
$compactReserve = 13000
# Bounds of the compaction window: Claude Code is documented to accept no less
# than 100K, and nothing above 180K is tested (200K is the model's own window).
$minWindow = 100000
$maxWindow = 180000
# Below 128K, compaction thrashes on read-heavy work (measured at 64K): after a
# compaction the context is already 20-29K, and one file read adds up to 15K.
# 128000 rather than 131072, so that both count as 128K.
$minContext = 128000

function Write-Out([string]$text) { [Console]::Out.WriteLine($text) }
function Write-Err([string]$text) { [Console]::Error.WriteLine($text) }
function Exit-Die([string]$message) {
    Write-Err "local-agent: $message"
    exit 1
}
function Exit-Usage([string]$message) {
    Write-Err "local-agent: $message"
    exit 2
}

# Integer division of non-negative numbers, as Bash's $((a / b)).
function Get-Quotient([long]$a, [long]$b) { return [long][math]::Floor($a / $b) }

# Writes text as UTF-8 without a byte order mark, which 5.1's Set-Content and
# Out-File can't.
$utf8 = New-Object System.Text.UTF8Encoding($false)
function Write-Utf8File([string]$path, [string]$text) { [IO.File]::WriteAllText($path, $text, $utf8) }

# True when the configuration file can be read by anyone but the user (and, on
# Windows, SYSTEM and the administrators, who can read every file anyway).
function Test-ReadableByOthers([string]$path) {
    if (-not $onWindows) {
        return (Get-Item -LiteralPath $path).UnixMode.Substring(4) -ne '------'
    }
    $own = @([Security.Principal.WindowsIdentity]::GetCurrent().User.Value, 'S-1-5-18', 'S-1-5-32-544')
    # As SIDs, so that an entry of a deleted account needs no name lookup.
    $rules = (Get-Acl -LiteralPath $path).GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])
    foreach ($rule in $rules) {
        if ($rule.AccessControlType -ne 'Allow') { continue }
        if ($own -notcontains $rule.IdentityReference.Value) { return $true }
    }
    return $false
}

# The configuration: KEY=VALUE lines, which the Bash runner sources; a value
# may be quoted or followed by a comment.
function Read-Config {
    if (-not (Test-Path -LiteralPath $configFile -PathType Leaf)) {
        Exit-Usage "no configuration in $configFile (LOCAL_AGENT_URL and LOCAL_AGENT_KEY, readable only by you)"
    }
    # The file holds a key: nobody else may read it.
    if (Test-ReadableByOthers $configFile) {
        if ($onWindows) {
            Exit-Usage "$configFile can be read by others; run: icacls `"$configFile`" /inheritance:r /grant:r `"$($env:USERNAME):F`""
        }
        Exit-Usage "$configFile can be read by others; run: chmod 600 $configFile"
    }
    $values = @{}
    foreach ($line in [IO.File]::ReadAllLines($configFile)) {
        if ($line -match '^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
            $name = $Matches[1]
            $value = $Matches[2].Trim()
            if ($value -match '^"([^"]*)"' -or $value -match "^'([^']*)'") {
                $value = $Matches[1]
            } else {
                $value = ($value -replace '\s+#.*$', '')
            }
            $values[$name] = $value
        }
    }
    if (-not $values['LOCAL_AGENT_URL'] -or -not $values['LOCAL_AGENT_KEY']) {
        Exit-Usage "$configFile needs LOCAL_AGENT_URL and LOCAL_AGENT_KEY"
    }
    $script:url = $values['LOCAL_AGENT_URL'] -replace '/$', ''
    $script:key = $values['LOCAL_AGENT_KEY']
    # Strata ignores the model name, but Claude Code only accepts names it knows.
    $script:model = 'claude-sonnet-4-5'
    if ($values['LOCAL_AGENT_MODEL']) { $script:model = $values['LOCAL_AGENT_MODEL'] }
    $share = $values['LOCAL_AGENT_COMPACT_SHARE']
    if ($share) {
        if ($share -notmatch '^[0-9]+$') { Exit-Usage "LOCAL_AGENT_COMPACT_SHARE in $configFile takes a percentage, not: $share" }
        $script:compactShare = [long]$share
    }
}

# Asks the Proxy for Strata's /health; the Proxy wakes the PC for it. Sets
# probeStatus ('000' when the Proxy didn't answer, with probeError),
# probeTag (its x-strata-proxy-status) and probeContext (Strata's max_context).
function Invoke-Probe([int]$timeout = 330) {
    $script:probeStatus = '000'
    $script:probeTag = ''
    $script:probeContext = ''
    $script:probeError = ''
    $request = [Net.HttpWebRequest]::Create("$url/health")
    # The NAS is in the LAN: no proxy server in between.
    $request.Proxy = $null
    $request.Timeout = $timeout * 1000
    $request.ReadWriteTimeout = $timeout * 1000
    $request.Headers.Add('x-api-key', $key)
    $response = $null
    try {
        $response = $request.GetResponse()
    } catch {
        # An answer with an error status comes as an exception that holds it.
        $e = $_.Exception
        while ($e -and -not ($e -is [Net.WebException])) { $e = $e.InnerException }
        if ($e -and $e.Response) {
            $response = $e.Response
        } elseif ($e) {
            $script:probeError = $e.Message
        } else {
            $script:probeError = $_.Exception.Message
        }
    }
    if (-not $response) { return }
    try {
        $script:probeStatus = [string][int]$response.StatusCode
        $tag = $response.Headers['x-strata-proxy-status']
        if ($tag) { $script:probeTag = $tag.Trim() }
        $reader = New-Object IO.StreamReader($response.GetResponseStream(), $utf8)
        $body = $reader.ReadToEnd()
        $reader.Close()
        try {
            $context = ($body | ConvertFrom-Json).max_context
            if ($null -ne $context) { $script:probeContext = [string]$context }
        } catch {
            $script:probeContext = ''
        }
    } finally {
        $response.Close()
    }
}

# Checks that Strata can take a run, waking the PC; calls $onFailure (a
# function taking a reason and a hint) otherwise. A wake timeout gets one more
# try. Sets probeContext.
function Assert-Strata([string]$onFailure) {
    Invoke-Probe
    if ($probeStatus -eq '504' -and $probeTag -eq 'wake_timeout') { Invoke-Probe }
    # The first matching case only: 401/strata_unauthorized also matches 401/*.
    switch -Wildcard ("$probeStatus/$probeTag") {
        '200/*' { break }
        '503/gaming' {
            & $onFailure 'Strata is off: the PC is in gaming mode'
            break
        }
        '504/wake_timeout' {
            & $onFailure 'the PC did not wake up in time (tried twice)'
            break
        }
        '401/strata_unauthorized' {
            & $onFailure "Strata refused the Proxy's own key" "set the Strata key in the Proxy's web UI (Strata section)"
            break
        }
        '401/*' {
            & $onFailure 'the Proxy did not accept the key' "check LOCAL_AGENT_KEY in $configFile against Access in the Proxy's web UI"
            break
        }
        '503/not_configured' {
            & $onFailure 'the Proxy is not set up: it has no address for Strata' "set Strata's address in the Proxy's web UI (Strata section)"
            break
        }
        '503/no_mac' {
            & $onFailure 'the Proxy cannot wake the PC: it knows no MAC address' 'start the PC once so the PC service reports its MAC, or set PC_MAC on the Proxy'
            break
        }
        '000/*' {
            & $onFailure "the Proxy is unreachable at $url ($probeError)" "check LOCAL_AGENT_URL in $configFile and that this machine reaches the NAS"
            break
        }
        default {
            $tagNote = ''
            if ($probeTag) { $tagNote = " ($probeTag)" }
            & $onFailure "the Proxy answered HTTP $probeStatus$tagNote"
        }
    }
    if ($probeContext -notmatch '^[0-9]+$') { & $onFailure "Strata's /health named no max_context" }
    if ([long]$probeContext -lt $minContext) {
        & $onFailure "Strata's context is $probeContext tokens, less than the $minContext a run needs" `
            "raise Strata's context to at least 128K ($minContext tokens), e.g. in the Strata section of the Proxy's web UI"
    }
}

# Sets compactWindow, compactPct (empty when not needed) and compactAt, all in
# tokens, so that Claude Code compacts at compactShare of Strata's context and
# the compaction call itself still fits. Claude Code compacts at about
# window - output cap - compactReserve; below minWindow, the percentage
# override (of the minWindow window) brings compaction down to the target.
# Needs probeContext.
function Set-Compaction {
    $script:compactAt = Get-Quotient ($compactShare * [long]$probeContext) 100
    $script:compactWindow = $compactAt + $outputTokens + $compactReserve
    $script:compactPct = ''
    if ($compactWindow -lt $minWindow) {
        $script:compactWindow = $minWindow
        $script:compactPct = [string](Get-Quotient ($compactAt * 100) $minWindow)
    } elseif ($compactWindow -gt $maxWindow) {
        $script:compactWindow = $maxWindow
        $script:compactAt = $compactWindow - $outputTokens - $compactReserve
    }
}

# Points Claude Code at the Proxy and sets its compaction window explicitly
# from Strata's context (see Set-Compaction): without it, Claude Code 2.1.218
# did not compact in -p mode at all. Output stays small enough for the
# compaction call itself to fit. The key goes in through the environment, never
# on a command line. Needs probeContext.
function Set-WorkerEnvironment {
    Set-Compaction
    Remove-Item Env:ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
    $env:ANTHROPIC_BASE_URL = $url
    $env:ANTHROPIC_AUTH_TOKEN = $key
    $env:ANTHROPIC_MODEL = $model
    $env:ANTHROPIC_DEFAULT_HAIKU_MODEL = $model
    $env:CLAUDE_CODE_DISABLE_1M_CONTEXT = '1'
    $env:CLAUDE_CODE_AUTO_COMPACT_WINDOW = [string]$compactWindow
    if ($compactPct) {
        $env:CLAUDE_AUTOCOMPACT_PCT_OVERRIDE = $compactPct
    } else {
        Remove-Item Env:CLAUDE_AUTOCOMPACT_PCT_OVERRIDE -ErrorAction SilentlyContinue
    }
    $env:CLAUDE_CODE_MAX_OUTPUT_TOKENS = [string]$outputTokens
    $env:CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'
    $env:API_TIMEOUT_MS = '900000'
}

# Quotes one argument by the rules Windows programs split their command line
# with. 5.1 passes arguments with spaces or quotes to native programs wrongly,
# so the worker's command line is built here. Anything beyond plain characters
# is quoted. When claude is npm's claude.cmd, cmd.exe reads the line first and
# still expands %NAME% inside quotes; the arguments the runner passes hold no %.
function ConvertTo-Argument([string]$argument) {
    if ($argument -match '^[A-Za-z0-9_./:=-]+$') { return $argument }
    $quoted = New-Object Text.StringBuilder
    [void]$quoted.Append('"')
    $backslashes = 0
    foreach ($c in $argument.ToCharArray()) {
        if ($c -eq '\') {
            $backslashes++
            continue
        }
        if ($c -eq '"') {
            [void]$quoted.Append('\', 2 * $backslashes + 1)
        } else {
            [void]$quoted.Append('\', $backslashes)
        }
        [void]$quoted.Append($c)
        $backslashes = 0
    }
    [void]$quoted.Append('\', 2 * $backslashes)
    [void]$quoted.Append('"')
    return $quoted.ToString()
}

# Starts claude in $workdir with $arguments, stdin from $inputFile and its
# output in $logFile and $errFile; returns the process, or nothing when there
# is no claude.
function Start-Worker([string]$workdir, [string[]]$arguments, [string]$inputFile, [string]$logFile, [string]$errFile) {
    $claude = Get-Command claude -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $claude) { return }
    $line = ($arguments | ForEach-Object { ConvertTo-Argument $_ }) -join ' '
    return Start-Process -FilePath $claude.Source -ArgumentList $line -WorkingDirectory $workdir -NoNewWindow -PassThru `
        -RedirectStandardInput $inputFile -RedirectStandardOutput $logFile -RedirectStandardError $errFile
}

# The objects of a stream-json log, one per line; lines that aren't JSON are
# skipped.
function Read-Events([string]$logFile) {
    if (-not (Test-Path -LiteralPath $logFile)) { return }
    foreach ($line in [IO.File]::ReadAllLines($logFile, $utf8)) {
        if (-not $line.Trim()) { continue }
        try { $line | ConvertFrom-Json } catch { continue }
    }
}

function Exit-CheckFailed([string]$reason, [string]$hint) {
    Write-Out "FAIL ${checkStep}: $reason"
    if ($hint) { Write-Out "     to fix: $hint" }
    exit 2
}

# Windows limits paths to 260 characters unless long paths are switched on; a
# Cargo target folder deep in a worktree can go past that.
function Test-LongPaths {
    if (-not $onWindows) { return $true }
    $value = Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -ErrorAction SilentlyContinue
    return $value -and $value.LongPathsEnabled -eq 1
}

# Checks the setup step by step, as far as it gets.
function Invoke-Check {
    Read-Config
    Write-Out "ok   config $configFile"
    $script:checkStep = 'proxy'
    Assert-Strata 'Exit-CheckFailed'
    Write-Out "ok   proxy: Strata answers with $probeContext tokens of context"
    Set-Compaction
    $override = ''
    if ($compactPct) { $override = ", override $compactPct %" }
    Write-Out "ok   compaction: window $compactWindow$override, compacts near $(Get-Quotient ($compactAt + 500) 1000)K"
    if (-not (Test-LongPaths)) {
        Write-Out 'warn long paths: Windows limits paths to 260 characters; deep build folders in a worktree can fail'
        Write-Out '     to fix: set LongPathsEnabled to 1 under HKLM\SYSTEM\CurrentControlSet\Control\FileSystem (as administrator)'
    }

    $script:checkStep = 'tool call'
    $marker = 'local-agent-check-{0:x4}' -f (Get-Random -Maximum 65536)
    $dir = Join-Path ([IO.Path]::GetTempPath()) ('local-agent-check-' + [Guid]::NewGuid().ToString('N'))
    $work = Join-Path $dir 'work'
    [void](New-Item -ItemType Directory -Path $work)
    try {
        Write-Utf8File (Join-Path $dir 'task') "Run this command with the Bash tool, then reply with its output: echo $marker`n"
        Set-WorkerEnvironment
        $worker = Start-Worker $work @('-p', '--output-format', 'stream-json', '--verbose', '--allowedTools', 'Bash(echo:*)',
            '--disallowedTools', 'PowerShell') (Join-Path $dir 'task') (Join-Path $dir 'log.jsonl') (Join-Path $dir 'stderr')
        if (-not $worker) { Exit-CheckFailed 'claude not found' 'install Claude Code, so that claude is on PATH' }
        $worker.WaitForExit()
        $found = $false
        foreach ($e in Read-Events (Join-Path $dir 'log.jsonl')) {
            if ($e.type -ne 'user' -or -not $e.message -or -not $e.message.content) { continue }
            foreach ($part in @($e.message.content)) {
                if ($part.type -ne 'tool_result') { continue }
                $text = $part.content
                if ($text -is [array]) { $text = ($text | ForEach-Object { $_.text }) -join '' }
                if ([string]$text -like "*$marker*") { $found = $true }
            }
        }
        if (-not $found) {
            $last = Get-Content -LiteralPath (Join-Path $dir 'stderr') -Tail 1 -ErrorAction SilentlyContinue
            Exit-CheckFailed "the worker made no working tool call ($last)" "look at Strata's log on the PC; Claude Code needs Strata 0.1.17 or newer"
        }
        Write-Out 'ok   tool call through Strata'
    } finally {
        Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$command = ''
if ($args.Count -gt 0) { $command = $args[0] }
switch ($command) {
    'check' { Invoke-Check }
    { $_ -in 'run', 'status', 'result', 'discard', 'prune' } { Exit-Die "$command is not ported to Windows yet; see issue #312" }
    { $_ -in '-h', '--help', 'help' } { Write-Out $usage }
    default {
        Write-Err $usage
        exit 2
    }
}
exit 0
