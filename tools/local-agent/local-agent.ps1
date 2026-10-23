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

# %APPDATA% and %LOCALAPPDATA% on Windows; the XDG folders elsewhere, as the
# Bash runner uses them.
function Get-BaseDir([string]$windowsVar, [string]$xdgVar, [string]$fallback) {
    $dir = [Environment]::GetEnvironmentVariable($windowsVar)
    if (-not $dir) { $dir = [Environment]::GetEnvironmentVariable($xdgVar) }
    if (-not $dir) { $dir = Join-Path $HOME $fallback }
    return $dir
}
$configFile = Join-Path (Join-Path (Get-BaseDir 'APPDATA' 'XDG_CONFIG_HOME' '.config') 'local-agent') 'env'
$stateDir = Join-Path (Get-BaseDir 'LOCALAPPDATA' 'XDG_STATE_HOME' '.local/state') 'local-agent'
$runsDir = Join-Path $stateDir 'runs'
$queueDir = Join-Path $stateDir 'queue'

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
# Room for one turn past the compaction point (a large file read) before the
# runner warns that the worker isn't compacting.
$compactOverrun = 20000
# Seconds between looks at the Proxy during a run, for gaming mode
# (LOCAL_AGENT_PROBE_INTERVAL, mainly for the tests).
$probeInterval = 60
if ($env:LOCAL_AGENT_PROBE_INTERVAL) { $probeInterval = [int]$env:LOCAL_AGENT_PROBE_INTERVAL }
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
function Add-Utf8File([string]$path, [string]$text) { [IO.File]::AppendAllText($path, $text, $utf8) }
# Output, and what git prints, in UTF-8, as Git Bash and the session read it;
# 5.1 uses the console's code page otherwise.
try { [Console]::OutputEncoding = $utf8 } catch { $null = $_ }

# Runs git quietly; returns its output lines and sets gitStatus. Its errors are
# dropped: 5.1 would turn them into terminating errors.
function Invoke-Git {
    $ErrorActionPreference = 'Continue'
    $output = & git @args 2>$null
    $script:gitStatus = $LASTEXITCODE
    return $output
}

# Prints a duration like 90, 90s, 30m or 2h in seconds, or nothing.
function ConvertTo-Seconds([string]$duration) {
    if ($duration -cnotmatch '^([0-9]+)([smh]?)$') { return }
    $n = [long]$Matches[1]
    switch -CaseSensitive ($Matches[2]) {
        'm' { return $n * 60 }
        'h' { return $n * 3600 }
        default { return $n }
    }
}

function Get-Now { return [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }

# Run metadata: one key=value per line in <run>/meta.
function Set-Meta([string]$name, [string]$value) {
    $file = Join-Path $runDir 'meta'
    $lines = @()
    if (Test-Path -LiteralPath $file) { $lines = @([IO.File]::ReadAllLines($file) | Where-Object { -not $_.StartsWith("$name=") }) }
    $lines += "$name=$value"
    Write-Utf8File "$file.tmp" (($lines -join "`n") + "`n")
    Move-Item -LiteralPath "$file.tmp" -Destination $file -Force
}
function Get-Meta([string]$dir, [string]$name) {
    $file = Join-Path $dir 'meta'
    if (-not (Test-Path -LiteralPath $file)) { return '' }
    $value = ''
    foreach ($line in [IO.File]::ReadAllLines($file)) {
        if ($line.StartsWith("$name=")) { $value = $line.Substring($name.Length + 1) }
    }
    return $value
}

function Test-Alive([string]$id) {
    if ($id -notmatch '^[0-9]+$') { return $false }
    return [bool](Get-Process -Id ([int]$id) -ErrorAction SilentlyContinue)
}

# A run's state; a queued or running run whose runner is gone is "stopped".
function Get-RunState([string]$dir) {
    $state = Get-Meta $dir 'state'
    if (($state -eq 'queued' -or $state -eq 'running') -and -not (Test-Alive (Get-Meta $dir 'pid'))) { $state = 'stopped' }
    return $state
}

# Sets runDir for a run id, or for the latest run of the current repo (of any
# repo outside one) when no id is given.
function Select-Run([string]$id) {
    if ($id) {
        $script:runDir = Join-Path $runsDir $id
        if (-not (Test-Path -LiteralPath (Join-Path $runDir 'meta'))) { Exit-Usage "unknown run: $id" }
        return
    }
    $top = Invoke-Git rev-parse --show-toplevel
    if ($gitStatus -ne 0) { $top = '' }
    $latest = ''
    if (Test-Path -LiteralPath $runsDir) {
        foreach ($dir in Get-ChildItem -LiteralPath $runsDir -Directory | Sort-Object Name) {
            if (-not (Test-Path -LiteralPath (Join-Path $dir.FullName 'meta'))) { continue }
            if (-not $top -or (Get-Meta $dir.FullName 'repo') -eq $top) { $latest = $dir.FullName }
        }
    }
    if (-not $latest) { Exit-Usage 'no runs yet' }
    $script:runDir = $latest
}

# The queue lets one run at a time talk to Strata, across all repos. Each
# waiting or active run holds a file named by a growing number, holding the
# runner's pid; the lowest number of a live runner goes first.
function Add-Ticket([string]$id) {
    [void](New-Item -ItemType Directory -Force -Path $queueDir)
    # A file held open without sharing as mutex for the counter; Windows lets
    # go of it when its runner dies.
    $lock = $null
    while (-not $lock) {
        try {
            $lock = [IO.File]::Open((Join-Path $stateDir 'counter.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
        } catch {
            Start-Sleep -Milliseconds 100
        }
    }
    try {
        $counter = Join-Path $stateDir 'counter'
        $n = 0
        if (Test-Path -LiteralPath $counter) { $n = [long]([IO.File]::ReadAllText($counter).Trim()) }
        $n++
        Write-Utf8File $counter "$n`n"
    } finally {
        $lock.Close()
    }
    $script:ticket = Join-Path $queueDir ('{0:d10}-{1}' -f $n, $id)
    # Written whole and then moved, so a reader never sees it without the pid.
    $new = Join-Path $queueDir ".new-$id"
    Write-Utf8File $new "$PID`n"
    Move-Item -LiteralPath $new -Destination $ticket
}

# Waits until this run's ticket is the first one of a live runner.
function Wait-Turn {
    $said = $false
    while ($true) {
        $first = $null
        foreach ($file in Get-ChildItem -LiteralPath $queueDir -File | Where-Object { -not $_.Name.StartsWith('.') } | Sort-Object Name) {
            $owner = ''
            try { $owner = [IO.File]::ReadAllText($file.FullName).Trim() } catch { $owner = '' }
            if (-not (Test-Alive $owner)) {
                Remove-Item -LiteralPath $file.FullName -Force -ErrorAction SilentlyContinue
                continue
            }
            $first = $file
            break
        }
        if ($first -and $first.FullName -eq $ticket) { return }
        if (-not $said -and $first) {
            Write-Out "local-agent: waiting for run $($first.Name.Substring(11)) to finish"
            $said = $true
        }
        Start-Sleep -Seconds 1
    }
}

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
    # cmd.exe ends a command line at a line break.
    if ($claude.Source -match '\.(cmd|bat)$') { $arguments = @($arguments | ForEach-Object { $_ -replace "`r?`n", ' ' }) }
    $line = ($arguments | ForEach-Object { ConvertTo-Argument $_ }) -join ' '
    $process = Start-Process -FilePath $claude.Source -ArgumentList $line -WorkingDirectory $workdir -NoNewWindow -PassThru `
        -RedirectStandardInput $inputFile -RedirectStandardOutput $logFile -RedirectStandardError $errFile
    # Keeps the handle, so that 5.1 can still read the exit code after the
    # process has ended.
    [void]$process.Handle
    return $process
}

# Stops the worker and every command it started: on Windows its process tree,
# first asking, then by force.
function Stop-Worker {
    if (-not $worker -or $worker.HasExited) { return }
    if ($onWindows) {
        # taskkill writes to stderr when a process ignores the polite request;
        # with Stop, Windows PowerShell would throw on that line.
        $ErrorActionPreference = 'Continue'
        & taskkill.exe /T /PID $worker.Id 2>&1 | Out-Null
        for ($i = 0; $i -lt 4 -and -not $worker.HasExited; $i++) { Start-Sleep -Milliseconds 500 }
        if (-not $worker.HasExited) { & taskkill.exe /T /F /PID $worker.Id 2>&1 | Out-Null }
    } else {
        $worker.Kill($true)
    }
    [void]$worker.WaitForExit(5000)
}

# The lines of a log the worker may still be writing, which Windows lets only
# a reader that shares write access open. -Whole leaves out a last line that
# has no line break yet.
function Read-LogLines([string]$logFile, [switch]$Whole) {
    if (-not (Test-Path -LiteralPath $logFile)) { return @() }
    $stream = New-Object IO.FileStream($logFile, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    try {
        $text = (New-Object IO.StreamReader($stream, $utf8)).ReadToEnd()
    } finally {
        $stream.Dispose()
    }
    $lines = $text.Split("`n")
    if ($Whole -or $lines[-1] -eq '') {
        if ($lines.Count -lt 2) { return @() }
        $lines = $lines[0..($lines.Count - 2)]
    }
    return $lines
}

# The objects of stream-json lines; lines that aren't JSON are skipped.
function ConvertFrom-EventLines([string[]]$lines) {
    foreach ($line in $lines) {
        if (-not $line.Trim()) { continue }
        try { $line | ConvertFrom-Json } catch { continue }
    }
}
function Read-Events([string]$logFile) { ConvertFrom-EventLines (Read-LogLines $logFile) }

# Tools the worker may use without asking, before the repo's own allow rules.
$readTools = @('Read', 'Glob', 'Grep', 'TodoWrite', 'Bash(git status:*)', 'Bash(git diff:*)', 'Bash(git log:*)',
    'Bash(git show:*)', 'Bash(ls:*)', 'Bash(grep:*)')
$editTools = @('Edit', 'MultiEdit', 'Write', 'NotebookEdit')
$writeTools = $editTools + @('Bash(git add:*)', 'Bash(git commit:*)')
# Never allowed, whatever the repo or --allow say: pushing, and the usual
# network tools. A deny list can't catch every program that talks to the
# network; it keeps the common ways closed. On Windows the worker also has a
# PowerShell tool, which these Bash rules don't cover, so it gets none.
$deniedTools = @('WebFetch', 'WebSearch', 'Bash(git push:*)', 'Bash(curl:*)', 'Bash(wget:*)', 'Bash(ssh:*)',
    'Bash(scp:*)', 'Bash(rsync:*)', 'Bash(nc:*)', 'PowerShell')

# True when a rule is one of deniedTools or narrows one of them:
# "Bash(git push --force)" narrows "Bash(git push:*)", "WebFetch(domain:x)"
# narrows "WebFetch"; "Bash(ncdu:*)" doesn't narrow "Bash(nc:*)".
function Test-Denied([string]$rule) {
    foreach ($denied in $deniedTools) {
        if ($rule -ceq $denied) { return $true }
        if ($denied.EndsWith(':*)')) {
            $command = $denied.Substring(0, $denied.Length - 3)
            if ($rule -ceq "$command)" -or $rule.StartsWith("${command}:", 'Ordinal') -or $rule.StartsWith("$command ", 'Ordinal')) { return $true }
        } elseif ($rule.StartsWith("$denied(", 'Ordinal')) {
            return $true
        }
    }
    return $false
}

# Sets allowed and disallowed for the worker: the base tools, the repo's allow
# rules (.claude/settings.json; not for read-only runs) and extraAllow, minus
# denied ones.
function Set-Permissions([string]$repo, [bool]$readOnly) {
    $candidates = @($readTools)
    if (-not $readOnly) { $candidates += $writeTools }
    # A read-only run works in the user's checkout, where the repo's rules
    # could allow writing commands; it gets only the read tools and --allow.
    $settings = Join-Path $repo '.claude/settings.json'
    if (-not $readOnly -and (Test-Path -LiteralPath $settings)) {
        try {
            $parsed = [IO.File]::ReadAllText($settings) | ConvertFrom-Json
            if ($parsed.permissions -and $parsed.permissions.allow) { $candidates += @($parsed.permissions.allow | Where-Object { $_ }) }
        } catch {
            $null = $_
        }
    }
    $candidates += $extraAllow
    $script:allowed = @()
    foreach ($rule in $candidates) {
        if (Test-Denied $rule) { continue }
        if ($readOnly -and $editTools -ccontains ($rule -replace '\(.*$', '')) { continue }
        $script:allowed += $rule
    }
    $script:disallowed = @($deniedTools)
    if ($readOnly) { $script:disallowed += $editTools + @('Bash(git add:*)', 'Bash(git commit:*)') }
}

# Ends a run that Strata can't take: exit 3 tells the skill to hand the task to
# a normal agent. $reason, and $hint on what to fix.
function Exit-Fallback([string]$reason, [string]$hint) {
    Set-Meta 'state' 'fallback'
    Set-Meta 'reason' $reason
    if ($hint) { Set-Meta 'hint' $hint }
    Set-Meta 'ended' (Get-Now)
    Write-Err "local-agent: run $(Get-Meta $runDir 'id') falls back to a normal agent: $reason"
    if ($hint) { Write-Err "local-agent: to fix: $hint" }
    exit 3
}

# A field of the worker's final result event, e.g. result or is_error, as
# text ('true', 'false'); empty without one.
function Get-ResultField([string]$name) {
    $value = ''
    foreach ($e in Read-Events (Join-Path $runDir 'log.jsonl')) {
        if ($e.type -eq 'result') {
            $value = $e.$name
            if ($value -is [bool]) { $value = $value.ToString().ToLower() }
        }
    }
    return [string]$value
}

# The worker's log as one line per step.
function Get-Steps([object[]]$events) {
    foreach ($e in $events) {
        if ($e.type -eq 'assistant') {
            foreach ($part in @($e.message.content)) {
                if (-not $part -or $part.type -ne 'tool_use') { continue }
                $what = ''
                foreach ($field in 'file_path', 'path', 'command', 'pattern') {
                    if ($part.input -and $null -ne $part.input.$field) {
                        $what = [string]$part.input.$field
                        break
                    }
                }
                if ($what.Length -gt 100) { $what = $what.Substring(0, 100) }
                "$($part.name) $what"
            }
        } elseif ($e.type -eq 'system' -and $e.subtype -eq 'init') {
            'started'
        } elseif ($e.type -eq 'system' -and $e.subtype -eq 'compact_boundary') {
            'context compacted'
        } elseif ($e.type -eq 'result') {
            if ($e.is_error) { 'finished with an error' } else { 'finished' }
        }
    }
}
function Get-LastSteps([int]$count) { return @(Get-Steps @(Read-Events (Join-Path $runDir 'log.jsonl')) | Select-Object -Last $count) }

# For --watch: appends the steps of new whole log lines to the progress file
# and, once per run, a warning when the worker is past its compaction point.
# Updates seen and warned.
function Update-Progress {
    $lines = @(Read-LogLines (Join-Path $runDir 'log.jsonl') -Whole)
    if ($lines.Count -le $seen) { return }
    $steps = @(Get-Steps @(ConvertFrom-EventLines $lines[$seen..($lines.Count - 1)]))
    $progress = Join-Path $runDir 'progress'
    if ($steps.Count -gt 0) { Add-Utf8File $progress (($steps -join "`n") + "`n") }
    if (-not $warned) {
        Get-ContextStats
        $warning = @(Get-ContextReport) | Where-Object { $_.StartsWith('warning:') }
        if ($warning) {
            Add-Utf8File $progress "$warning`n"
            $script:warned = $true
        }
    }
    $script:seen = $lines.Count
}

# How full the worker's context is, from the usage in its log: sets
# contextNow and contextPeak (tokens) and compactions.
function Get-ContextStats {
    $script:contextNow = 0
    $script:contextPeak = 0
    $script:compactions = 0
    foreach ($e in Read-Events (Join-Path $runDir 'log.jsonl')) {
        if ($e.type -eq 'assistant' -and $e.message -and $e.message.usage) {
            $u = $e.message.usage
            $script:contextNow = [long]$u.input_tokens + [long]$u.cache_read_input_tokens + [long]$u.cache_creation_input_tokens
            if ($contextNow -gt $contextPeak) { $script:contextPeak = $contextNow }
        } elseif ($e.type -eq 'system' -and $e.subtype -eq 'compact_boundary') {
            $script:compactions++
        }
    }
}

# Lines on the worker's context, and a warning when it has grown past the
# compaction point by more than a turn's room without compacting (Claude Code
# then runs on until Strata answers 400). Needs Get-ContextStats and a run that
# recorded compact_at.
function Get-ContextReport {
    $at = Get-Meta $runDir 'compact_at'
    if (-not $at -or $contextPeak -le 0) { return }
    "context: $(Get-Quotient ($contextNow + 500) 1000)K now (peak $(Get-Quotient ($contextPeak + 500) 1000)K, " +
    "compacts near $(Get-Quotient ([long]$at + 500) 1000)K), compactions: $compactions"
    if ($contextNow -gt [long]$at + $compactOverrun) {
        'warning: context past the compaction point without compacting; Strata may answer 400 soon'
    }
}

# Commits what the worker left uncommitted, so the branch holds all its work.
function Save-Leftovers([string]$worktree, [string]$id) {
    $changes = Invoke-Git -C $worktree status --porcelain
    if (-not $changes) { return }
    [void](Invoke-Git -C $worktree add -A)
    [void](Invoke-Git -c core.longpaths=true -C $worktree commit -qm "local-agent: uncommitted changes of run $id")
}

# After the worker died partway: keeps its work, writes what a fallback agent
# needs to continue, and falls back.
function Exit-MidRun([int]$exitCode) {
    $errorText = Get-ResultField 'result'
    if (-not $errorText) { $errorText = Get-Content -LiteralPath (Join-Path $runDir 'stderr') -Tail 1 -Encoding UTF8 -ErrorAction SilentlyContinue }
    if (-not $errorText) { $errorText = "the worker exited with code $exitCode" }
    $handoff = @("Error: $errorText")
    $branch = Get-Meta $runDir 'branch'
    if ($branch) {
        $repo = Get-Meta $runDir 'repo'
        Save-Leftovers (Get-Meta $runDir 'worktree') (Get-Meta $runDir 'id')
        $handoff += "Changed files, committed on ${branch}:"
        $handoff += @(Invoke-Git -C $repo diff --name-only (Get-Meta $runDir 'base') $branch | ForEach-Object { "  $_" })
    }
    Get-ContextStats
    $handoff += @(Get-ContextReport)
    $handoff += 'Last steps:'
    $handoff += @(Get-LastSteps 10 | ForEach-Object { "  $_" })
    Write-Utf8File (Join-Path $runDir 'handoff') (($handoff -join "`n") + "`n")
    # A quick look whether the PC went into gaming mode meanwhile.
    Invoke-Probe 15
    if ($probeTag -eq 'gaming') { Exit-Fallback 'Strata failed mid-run: the PC switched to gaming mode' }
    Exit-Fallback "Strata failed mid-run: $errorText"
}

$instructions = @"
You are a local-agent worker: a Claude Code session running unattended for another session.
Nobody can answer questions or permission prompts. If a tool or command is denied, work around it or say so in your summary.
Work only in the current directory.
"@
$writeInstructions = @"
When you are done, commit your changes with a short message.
End with a short summary: what you changed, how you checked it, and what is left open.
"@
$readOnlyInstructions = @"
This run is read-only: you cannot change files. Answer from what you read.
End with your answer and the files it rests on.
"@

function Invoke-Run([string[]]$arguments) {
    $task = ''
    $readOnly = $false
    $watch = $false
    $timeout = '2h'
    $script:extraAllow = @()
    $i = 0
    while ($i -lt $arguments.Count) {
        $arg = $arguments[$i]
        $i++
        if ($arg -ceq '--read-only') {
            $readOnly = $true
        } elseif ($arg -ceq '--watch') {
            $watch = $true
        } elseif ($arg -ceq '--timeout') {
            if ($i -ge $arguments.Count) { Exit-Usage '--timeout needs a time, e.g. --timeout 30m' }
            $timeout = $arguments[$i]
            $i++
        } elseif ($arg -ceq '--allow') {
            if ($i -ge $arguments.Count) { Exit-Usage "--allow needs a rule, e.g. --allow 'Bash(cargo test:*)'" }
            $script:extraAllow += $arguments[$i]
            $i++
        } elseif ($arg -ceq '--') {
            if ($i -lt $arguments.Count) { $task = $arguments[$i..($arguments.Count - 1)] -join ' ' }
            break
        } elseif ($arg.StartsWith('-')) {
            Exit-Usage "unknown option: $arg"
        } else {
            $task = $arguments[($i - 1)..($arguments.Count - 1)] -join ' '
            break
        }
    }
    $limit = ConvertTo-Seconds $timeout
    if ($null -eq $limit) { Exit-Usage "--timeout takes seconds, or a number with s, m or h: $timeout" }
    Read-Config
    if (-not $task) {
        if ([Console]::IsInputRedirected) { [Console]::InputEncoding = $utf8 }
        $task = [Console]::In.ReadToEnd().TrimEnd("`r", "`n")
    }
    if (-not $task) { Exit-Usage 'no task' }
    $repo = Invoke-Git rev-parse --show-toplevel
    if ($gitStatus -ne 0 -or -not $repo) { Exit-Usage 'not inside a git repository' }

    $id = '{0}-{1:x4}' -f (Get-Date -Format 'yyyyMMdd-HHmmss'), (Get-Random -Maximum 65536)
    $script:runDir = Join-Path $runsDir $id
    [void](New-Item -ItemType Directory -Force -Path $runDir)
    if (-not $onWindows) { & chmod 700 $stateDir }
    Write-Utf8File (Join-Path $runDir 'task') "$task`n"
    Set-Meta 'id' $id
    Set-Meta 'repo' $repo
    Set-Meta 'state' 'queued'
    Set-Meta 'pid' $PID
    Set-Meta 'started' (Get-Now)
    Write-Out "local-agent: run $id ($runDir)"
    if ($watch) {
        Write-Utf8File (Join-Path $runDir 'progress') ''
        Write-Out "local-agent: progress in $(Join-Path $runDir 'progress')"
    }

    Add-Ticket $id
    try {
        Wait-Turn
        Assert-Strata 'Exit-Fallback'
        Set-Compaction
        Set-Meta 'compact_at' $compactAt

        # A writing run gets its own branch and worktree; a read-only one
        # reads the repo as it is.
        $workdir = $repo
        $mode = 'acceptEdits'
        if ($readOnly) {
            $mode = 'default'
            Set-Meta 'mode' 'read-only'
        } else {
            $base = Invoke-Git -C $repo rev-parse HEAD
            $branch = "local-agent/$id"
            $workdir = Join-Path (Join-Path $stateDir (Split-Path -Leaf $repo)) $id
            # Long paths for git, without touching the repo's config: deep
            # build folders in a worktree go past Windows' 260 characters.
            [void](Invoke-Git -c core.longpaths=true -C $repo worktree add -q -b $branch $workdir $base)
            if ($gitStatus -ne 0) { Exit-Die "git could not create the worktree $workdir" }
            Set-Meta 'mode' 'write'
            Set-Meta 'base' $base
            Set-Meta 'branch' $branch
            Set-Meta 'worktree' $workdir
        }
        Set-Permissions $repo $readOnly
        $prompt = $instructions + "`n" + $writeInstructions
        if ($readOnly) { $prompt = $instructions + "`n" + $readOnlyInstructions }
        Set-Meta 'state' 'running'
        Set-Meta 'started' (Get-Now)

        Set-WorkerEnvironment
        # git commands of the worker get long paths too, after any entries the
        # environment already passes this way.
        $n = 0
        if ($env:GIT_CONFIG_COUNT -match '^[0-9]+$') { $n = [int]$env:GIT_CONFIG_COUNT }
        [Environment]::SetEnvironmentVariable("GIT_CONFIG_KEY_$n", 'core.longpaths')
        [Environment]::SetEnvironmentVariable("GIT_CONFIG_VALUE_$n", 'true')
        $env:GIT_CONFIG_COUNT = [string]($n + 1)
        $workerArgs = @('-p', '--output-format', 'stream-json', '--verbose', '--append-system-prompt', $prompt,
            '--permission-mode', $mode, '--allowedTools') + $allowed + @('--disallowedTools') + $disallowed
        $script:worker = Start-Worker $workdir $workerArgs (Join-Path $runDir 'task') (Join-Path $runDir 'log.jsonl') (Join-Path $runDir 'stderr')
        if (-not $worker) { Exit-Fallback 'claude not found' 'install Claude Code, so that claude is on PATH' }
        $deadline = (Get-Now) + $limit
        $nextProbe = (Get-Now) + $probeInterval
        $timedOut = $false
        $gaming = $false
        $finished = $false
        $script:seen = 0
        $script:warned = $false
        try {
            while (-not $worker.WaitForExit(1000)) {
                if ($watch) { Update-Progress }
                if ((Get-Now) -ge $deadline) {
                    $timedOut = $true
                    Stop-Worker
                    break
                }
                # Claude Code retries a 503 for a long time; gaming mode ends
                # the run sooner.
                if ((Get-Now) -ge $nextProbe) {
                    $nextProbe = (Get-Now) + $probeInterval
                    Invoke-Probe 10
                    if ($probeTag -eq 'gaming') {
                        $gaming = $true
                        Stop-Worker
                        break
                    }
                }
            }
            $finished = $true
        } finally {
            # Interrupted (Ctrl+C): the worker and what it started stop too.
            if (-not $finished) {
                Stop-Worker
                Set-Meta 'state' 'stopped'
                Set-Meta 'ended' (Get-Now)
            }
        }
        if ($watch) { Update-Progress }
        if ($timedOut) {
            if (-not $readOnly) { Save-Leftovers $workdir $id }
            Set-Meta 'state' 'timeout'
            Set-Meta 'reason' "stopped after the time limit of $timeout"
            Set-Meta 'ended' (Get-Now)
            Write-Err "local-agent: run $id stopped after $timeout; see local-agent result $id"
            exit 4
        }

        $exitCode = -1
        if ($worker.HasExited) { $exitCode = $worker.ExitCode }
        # A worker that exits badly, ends on an error or never ends counts as
        # failed.
        if ($gaming -or $exitCode -ne 0 -or (Get-ResultField 'is_error') -ne 'false') { Exit-MidRun $exitCode }
        Write-Utf8File (Join-Path $runDir 'summary') ((Get-ResultField 'result') + "`n")
        if (-not $readOnly) { Save-Leftovers $workdir $id }
        Set-Meta 'ended' (Get-Now)
        Set-Meta 'state' 'done'
        Write-Out "local-agent: run $id done; review it with: local-agent result $id"
    } finally {
        Remove-Item -LiteralPath $ticket -Force -ErrorAction SilentlyContinue
    }
}

function Show-Status([string]$id) {
    Select-Run $id
    $state = Get-RunState $runDir
    $started = [long](Get-Meta $runDir 'started')
    $ended = Get-Meta $runDir 'ended'
    if (-not $ended) { $ended = Get-Now }
    $elapsed = [long]$ended - $started
    Write-Out "run $(Get-Meta $runDir 'id'): $state ($(Get-Quotient $elapsed 60)m$($elapsed % 60)s)"
    Write-Out "repo: $(Get-Meta $runDir 'repo')"
    Write-Out "task: $([IO.File]::ReadAllLines((Join-Path $runDir 'task'))[0])"
    $reason = Get-Meta $runDir 'reason'
    if ($reason) { Write-Out "reason: $reason" }
    $log = Join-Path $runDir 'log.jsonl'
    if ((Test-Path -LiteralPath $log) -and (Get-Item -LiteralPath $log).Length -gt 0) {
        Get-ContextStats
        Get-ContextReport | ForEach-Object { Write-Out $_ }
        Write-Out 'last steps:'
        Get-LastSteps 8 | ForEach-Object { Write-Out "  $_" }
    }
}

function Show-Result([string]$id) {
    Select-Run $id
    $state = Get-RunState $runDir
    Write-Out "run $(Get-Meta $runDir 'id'): $state"
    if ($state -eq 'fallback') { Write-Out 'next: fall back to a normal agent' }
    if ($state -eq 'timeout') { Write-Out 'next: report the timeout; no fallback' }
    $reason = Get-Meta $runDir 'reason'
    $hint = Get-Meta $runDir 'hint'
    if ($reason) { Write-Out "reason: $reason" }
    if ($hint) { Write-Out "to fix: $hint" }
    $worktree = Get-Meta $runDir 'worktree'
    if ($worktree) { Write-Out "worktree: $worktree (branch $(Get-Meta $runDir 'branch'))" }
    $summary = Join-Path $runDir 'summary'
    if (Test-Path -LiteralPath $summary) {
        Write-Out ''
        Write-Out '## Summary'
        Write-Out ([IO.File]::ReadAllText($summary, $utf8).TrimEnd("`n"))
    }
    if ($state -eq 'timeout') {
        Write-Out ''
        Write-Out '## Last steps'
        Get-LastSteps 10 | ForEach-Object { Write-Out $_ }
    }
    $handoff = Join-Path $runDir 'handoff'
    if (Test-Path -LiteralPath $handoff) {
        Write-Out ''
        Write-Out '## Log summary'
        Write-Out ([IO.File]::ReadAllText($handoff, $utf8).TrimEnd("`n"))
    }
    $branch = Get-Meta $runDir 'branch'
    if ($branch) {
        $repo = Get-Meta $runDir 'repo'
        $base = Get-Meta $runDir 'base'
        Write-Out ''
        Write-Out "## Changes on $branch"
        Invoke-Git -C $repo --no-pager diff --stat $base $branch | ForEach-Object { Write-Out $_ }
        Invoke-Git -C $repo --no-pager diff $base $branch | ForEach-Object { Write-Out $_ }
    }
}

# Removes a run's worktree and branch, if it has them.
function Remove-Worktree([string]$dir) {
    $repo = Get-Meta $dir 'repo'
    $worktree = Get-Meta $dir 'worktree'
    $branch = Get-Meta $dir 'branch'
    if (-not $branch -or -not (Test-Path -LiteralPath $repo -PathType Container)) { return }
    if (Test-Path -LiteralPath $worktree -PathType Container) {
        [void](Invoke-Git -c core.longpaths=true -C $repo worktree remove --force $worktree)
    }
    [void](Invoke-Git -C $repo worktree prune)
    [void](Invoke-Git -C $repo rev-parse -q --verify "refs/heads/$branch")
    if ($gitStatus -eq 0) { [void](Invoke-Git -C $repo branch -qD $branch) }
}

# True when a run's branch is merged: it has commits, and another local branch
# contains its tip.
function Test-Merged([string]$dir) {
    $repo = Get-Meta $dir 'repo'
    $branch = Get-Meta $dir 'branch'
    if (-not $branch -or -not (Test-Path -LiteralPath $repo -PathType Container)) { return $false }
    $tip = Invoke-Git -C $repo rev-parse -q --verify "refs/heads/$branch"
    if ($gitStatus -ne 0) { return $false }
    # A branch without commits of its own is contained everywhere; keep it.
    if ($tip -eq (Get-Meta $dir 'base')) { return $false }
    $containing = @(Invoke-Git -C $repo for-each-ref --contains "refs/heads/$branch" '--format=%(refname:short)' refs/heads/)
    return [bool]($containing | Where-Object { $_ -and -not $_.StartsWith('local-agent/') })
}

function Invoke-Discard([string]$id) {
    if (-not $id) { Exit-Usage 'discard needs a run id' }
    Select-Run $id
    if ((Get-RunState $runDir) -in 'queued', 'running') { Exit-Usage "run $id is still running" }
    Remove-Worktree $runDir
    Set-Meta 'state' 'discarded'
    Write-Out "local-agent: discarded run $id"
}

# Removes the worktrees of merged runs, and run folders older than 14 days
# whose worktree is gone. Unmerged work is never removed.
function Invoke-Prune {
    if (-not (Test-Path -LiteralPath $runsDir)) { return }
    foreach ($dir in Get-ChildItem -LiteralPath $runsDir -Directory | Sort-Object Name) {
        $meta = Join-Path $dir.FullName 'meta'
        if (-not (Test-Path -LiteralPath $meta)) { continue }
        if ((Get-RunState $dir.FullName) -in 'queued', 'running') { continue }
        if (Test-Merged $dir.FullName) { Remove-Worktree $dir.FullName }
        # As find -mtime +14: 15 whole days or more.
        $old = ((Get-Date) - (Get-Item -LiteralPath $meta).LastWriteTime).TotalDays -ge 15
        $worktree = Get-Meta $dir.FullName 'worktree'
        if ($old -and -not ($worktree -and (Test-Path -LiteralPath $worktree -PathType Container))) {
            Remove-Item -LiteralPath $dir.FullName -Recurse -Force
            Write-Out "local-agent: pruned run $($dir.Name)"
        }
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
            $last = Get-Content -LiteralPath (Join-Path $dir 'stderr') -Tail 1 -Encoding UTF8 -ErrorAction SilentlyContinue
            Exit-CheckFailed "the worker made no working tool call ($last)" "look at Strata's log on the PC; Claude Code needs Strata 0.1.17 or newer"
        }
        Write-Out 'ok   tool call through Strata'
    } finally {
        Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$command = ''
$rest = @()
if ($args.Count -gt 0) {
    $command = $args[0]
    $rest = @($args | Select-Object -Skip 1)
}
switch ($command) {
    'run' { Invoke-Run $rest }
    'status' { Show-Status ($rest | Select-Object -First 1) }
    'result' { Show-Result ($rest | Select-Object -First 1) }
    'check' { Invoke-Check }
    'discard' { Invoke-Discard ($rest | Select-Object -First 1) }
    'prune' { Invoke-Prune }
    { $_ -in '-h', '--help', 'help' } { Write-Out $usage }
    default {
        Write-Err $usage
        exit 2
    }
}
exit 0
