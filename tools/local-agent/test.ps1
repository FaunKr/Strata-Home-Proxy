# Tests tools/local-agent/local-agent.ps1 as a black box, as test.sh tests the
# Bash runner: each test runs the real script under pwsh in a throwaway git
# repo, against the same fake Proxy (testdata/fake-proxy.py) and fake `claude`
# on PATH (testdata/fake-claude), and checks only what a user sees: exit code,
# output, worktrees, branches and the original checkout. A test that test.sh
# has too carries the same name.
#
# Usage: pwsh -NoProfile -File tools/local-agent/test.ps1 [test_name...]
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$runner = Join-Path $here 'local-agent.ps1'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('local-agent-test-' + [Guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path (Join-Path $tmp 'bin'))

$env:APPDATA = Join-Path $tmp 'appdata'
$env:LOCALAPPDATA = Join-Path $tmp 'localappdata'
$env:GIT_AUTHOR_NAME = 'test'
$env:GIT_AUTHOR_EMAIL = 'test@example.com'
$env:GIT_COMMITTER_NAME = 'test'
$env:GIT_COMMITTER_EMAIL = 'test@example.com'
[void](New-Item -ItemType SymbolicLink -Path (Join-Path $tmp 'bin/claude') -Target (Join-Path $here 'testdata/fake-claude'))
$env:PATH = (Join-Path $tmp 'bin') + [IO.Path]::PathSeparator + $env:PATH
$env:FAKE_CLAUDE_RECORD = Join-Path $tmp 'claude'
$env:FAKE_PROXY_SCENARIO = Join-Path $tmp 'scenario.json'
$record = $env:FAKE_CLAUDE_RECORD
$config = Join-Path $env:APPDATA 'local-agent/env'
$stateDir = Join-Path $env:LOCALAPPDATA 'local-agent'

Set-Content -LiteralPath $env:FAKE_PROXY_SCENARIO -Value '{}'
$portFile = Join-Path $tmp 'port'
$proxy = Start-Process -FilePath python3 -ArgumentList @((Join-Path $here 'testdata/fake-proxy.py'), $env:FAKE_PROXY_SCENARIO, $portFile) -PassThru -NoNewWindow
for ($i = 0; $i -lt 50 -and -not (Test-Path $portFile -PathType Leaf); $i++) { Start-Sleep -Milliseconds 100 }
Start-Sleep -Milliseconds 100
$url = 'http://127.0.0.1:' + (Get-Content -LiteralPath $portFile -Raw).Trim()

$script:current = ''
$script:failures = 0
function Fail([string]$message) {
    $script:failures++
    [Console]::Error.WriteLine("FAIL ${script:current}: $message")
}
# Asserts that a file holds a line, or that a text holds a substring.
function Assert-HasLine([string]$file, [string]$line) {
    if (-not (Test-Path -LiteralPath $file)) {
        Fail "no $file"
    } elseif ((Get-Content -LiteralPath $file) -cnotcontains $line) {
        Fail "$file has no line '$line' in: $((Get-Content -LiteralPath $file) -join "`n")"
    }
}
function Assert-Contains([string]$text, [string]$part) {
    if (-not $text.Contains($part)) { Fail "'$part' missing in: $text" }
}

function Write-Config {
    [void](New-Item -ItemType Directory -Force -Path (Split-Path -Parent $config))
    Set-Content -LiteralPath $config -Value "LOCAL_AGENT_URL=$url`nLOCAL_AGENT_KEY=client-key"
    & chmod 600 $config
}
# Sets the fake Proxy's answers to /health, one JSON object per argument.
function Set-Health {
    Set-Content -LiteralPath $env:FAKE_PROXY_SCENARIO -Value ('{"health": [' + ($args -join ',') + ']}')
}

# A fresh repo with one commit, as the current directory.
function New-Repo([string]$suffix = '') {
    $script:repo = Join-Path $tmp "repos/$script:current$suffix"
    [void](New-Item -ItemType Directory -Force -Path $script:repo)
    Set-Location -LiteralPath $script:repo
    git init -q -b main
    Set-Content -LiteralPath README -Value 'hello'
    git add README
    git commit -qm init
}

# Runs a command; sets $out (stdout and stderr) and $code.
function Invoke-Captured([string]$program, [string[]]$arguments) {
    $ErrorActionPreference = 'Continue'
    $lines = & $program @arguments 2>&1 | ForEach-Object { "$_" }
    $script:code = $LASTEXITCODE
    $script:out = ($lines -join "`n")
}
# Runs local-agent.
$pwsh = (Get-Command pwsh -CommandType Application | Select-Object -First 1).Source
function la { Invoke-Captured $pwsh (@('-NoProfile', '-File', $runner) + $args) }

function Reset-Test {
    foreach ($path in $record, $stateDir, "$($env:FAKE_PROXY_SCENARIO).requests") {
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Recurse -Force }
    }
    Remove-Item Env:FAKE_CLAUDE_MODE, Env:FAKE_CLAUDE_SLEEP, Env:PREFIX, Env:LOCAL_AGENT_PROBE_INTERVAL -ErrorAction SilentlyContinue
    Write-Config
    Set-Health '{"status": 200, "max_context": 262144}'
    New-Repo
}

# Starts local-agent in the background with its output in $outFile; returns
# the process.
function Start-Runner([string]$outFile, [string[]]$arguments) {
    $info = New-Object Diagnostics.ProcessStartInfo 'sh'
    foreach ($a in @('-c', 'exec "$@" >"$0" 2>&1', $outFile, $pwsh, '-NoProfile', '-File', $runner) + $arguments) { $info.ArgumentList.Add($a) }
    $info.UseShellExecute = $false
    $info.WorkingDirectory = (Get-Location).Path
    return [Diagnostics.Process]::Start($info)
}
# A process counts as alive unless it is gone or a zombie (which a container's
# init may never reap).
function Test-Alive([string]$id) {
    $stat = (& /bin/ps -o stat= -p $id 2>$null | Out-String).Trim()
    return $stat -and -not $stat.StartsWith('Z')
}
function Get-RunId { if ($out -match '(?m)^local-agent: run (\S+)') { return $Matches[1] } }
function Get-Recorded([string]$name) { return (Get-Content -LiteralPath (Join-Path $record $name) -Raw).Trim() }
function Test-Branch([string]$name) {
    git rev-parse -q --verify $name 2>$null | Out-Null
    return $LASTEXITCODE -eq 0
}

# The values the fake claude got for an option, one per line.
function Get-OptionValues([string]$option) {
    $on = $false
    foreach ($line in Get-Content -LiteralPath (Join-Path $record 'args')) {
        if ($line -ceq $option) { $on = $true; continue }
        if ($line.StartsWith('--')) { $on = $false }
        if ($on) { $line }
    }
}

function test_config_required {
    Remove-Item -LiteralPath $config
    la run task
    if ($code -ne 2) { Fail "exit ${code}: $out" }
    Assert-Contains $out $config
    if (Test-Path (Join-Path $record 'args')) { Fail 'worker started' }
    # A config others can read is refused.
    Write-Config
    & chmod 644 $config
    la run task
    if ($code -ne 2) { Fail "exit $code for a readable file: $out" }
    Assert-Contains $out 'chmod 600'
}

function test_install {
    Remove-Item -LiteralPath $config
    $inst = Join-Path $tmp 'inst'
    $env:PREFIX = $inst
    Invoke-Captured 'pwsh' @('-NoProfile', '-File', (Join-Path $here 'install.ps1'))
    if ($code -ne 0) { Fail "install exit ${code}: $out" }
    # The runner, and a shim for PowerShell and cmd and one for Git Bash.
    foreach ($file in 'lib/local-agent.ps1', 'local-agent.cmd', 'local-agent') {
        if (-not (Test-Path -LiteralPath (Join-Path $inst $file))) { Fail "$file not installed" }
    }
    # A config template, readable only by the user, that the runner still refuses.
    if (-not (Test-Path -LiteralPath $config)) { Fail "no template for $config" }
    if ((Get-Item -LiteralPath $config).UnixMode -ne '-rw-------') { Fail 'template readable by others' }
    Assert-Contains $out $config
    $shim = Join-Path $inst 'local-agent'
    Invoke-Captured $shim @('check')
    if ($code -ne 2) { Fail "check with an empty template: exit ${code}: $out" }
    Assert-Contains $out 'needs LOCAL_AGENT_URL'
    # A second install keeps the existing config.
    Write-Config
    Invoke-Captured 'pwsh' @('-NoProfile', '-File', (Join-Path $here 'install.ps1'))
    if (-not (Get-Content -LiteralPath $config -Raw).Contains('client-key')) { Fail 'config overwritten' }
    # The Git Bash shim passes the arguments and the exit code on.
    $env:FAKE_CLAUDE_MODE = 'check'
    Invoke-Captured $shim @('check')
    if ($code -ne 0) { Fail "check through the shim: exit ${code}: $out" }
    Assert-Contains $out 'ok   tool call'
}

function test_check {
    $env:FAKE_CLAUDE_MODE = 'check'
    la check
    if ($code -ne 0) { Fail "check exit ${code}: $out" }
    Assert-Contains $out 'ok   tool call'
    Assert-Contains $out 'ok   compaction: window 180000, compacts near 159K'
    # The worker check runs with the same settings as a run.
    $envFile = Join-Path $record 'env'
    Assert-HasLine $envFile 'CLAUDE_CODE_AUTO_COMPACT_WINDOW=180000'
    Assert-HasLine $envFile 'CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>'
    Assert-HasLine $envFile "ANTHROPIC_BASE_URL=$url"
    Assert-HasLine $envFile 'ANTHROPIC_AUTH_TOKEN=client-key'
    # Each failure names its fix.
    $env:FAKE_CLAUDE_MODE = 'no-tool'
    la check
    if ($code -ne 2) { Fail "without a tool call: exit ${code}: $out" }
    Assert-Contains $out 'FAIL tool call'
    Set-Health '{"status": 200, "max_context": 32768}'
    la check
    if ($code -ne 2) { Fail "32K: exit ${code}: $out" }
    Assert-Contains $out '128K'
    Set-Health '{"status": 401, "tag": "unauthorized"}'
    la check
    Assert-Contains $out 'Access'
    Set-Content -LiteralPath $config -Value "LOCAL_AGENT_URL=http://127.0.0.1:9`nLOCAL_AGENT_KEY=client-key"
    la check
    if ($code -ne 2) { Fail "unreachable: exit ${code}: $out" }
    Assert-Contains $out 'LOCAL_AGENT_URL'
    Remove-Item -LiteralPath $config
    la check
    if ($code -ne 2) { Fail "without config: exit ${code}: $out" }
    Assert-Contains $out $config
}

function test_check_disallows_powershell_tool {
    # On Windows the worker would have a PowerShell tool, which the deny list
    # of Bash rules doesn't cover.
    $env:FAKE_CLAUDE_MODE = 'check'
    la check
    if (@(Get-OptionValues '--disallowedTools') -cnotcontains 'PowerShell') { Fail "disallowed: $(Get-OptionValues '--disallowedTools')" }
    if (@(Get-OptionValues '--allowedTools') -cnotcontains 'Bash(echo:*)') { Fail "allowed: $(Get-OptionValues '--allowedTools')" }
}

function test_check_task_on_stdin {
    $env:FAKE_CLAUDE_MODE = 'check'
    la check
    if (-not (Get-Content -LiteralPath (Join-Path $record 'stdin') -Raw).Contains('echo local-agent-check-')) { Fail 'no task on stdin' }
    if ((Get-Content -LiteralPath (Join-Path $record 'args')) -match 'local-agent-check-') { Fail 'task on the command line' }
}

function test_check_without_claude {
    $saved = $env:PATH
    $env:PATH = '/usr/bin:/bin'
    try {
        la check
    } finally {
        $env:PATH = $saved
    }
    if ($code -ne 2) { Fail "exit ${code}: $out" }
    Assert-Contains $out 'FAIL tool call: claude not found'
}

function test_config_formats {
    # As the Bash runner sources it: quotes, comments and a trailing slash.
    $env:FAKE_CLAUDE_MODE = 'check'
    Set-Content -LiteralPath $config -Value "# the Proxy`nLOCAL_AGENT_URL=`"$url/`"`nLOCAL_AGENT_KEY=client-key  # from Access`nLOCAL_AGENT_COMPACT_SHARE='70'"
    la check
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    Assert-HasLine (Join-Path $record 'env') "ANTHROPIC_BASE_URL=$url"
    Assert-HasLine (Join-Path $record 'env') 'ANTHROPIC_AUTH_TOKEN=client-key'
    Assert-Contains $out 'compacts near 159K'
    Add-Content -LiteralPath $config -Value 'LOCAL_AGENT_COMPACT_SHARE=most'
    la check
    if ($code -ne 2) { Fail "a share that is no number: exit ${code}: $out" }
    Assert-Contains $out 'LOCAL_AGENT_COMPACT_SHARE'
}

function test_happy_path {
    la run 'Add a line to README'
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    $id = Get-RunId
    if (-not $id) { Fail "no run id in: $out" }
    # The worker ran in a worktree outside the repo, on a new branch.
    $cwd = Get-Recorded 'cwd'
    if (-not $cwd.StartsWith("$stateDir/")) { Fail "worker ran in $cwd" }
    if ((git -C $cwd branch --show-current) -ne "local-agent/$id") { Fail "wrong branch in $cwd" }
    Assert-HasLine (Join-Path $record 'stdin') 'Add a line to README'
    # The edit is committed on the branch; the checkout is untouched.
    git diff --quiet main "local-agent/$id"
    if ($LASTEXITCODE -eq 0) { Fail 'branch without a change' }
    if ((Get-Content README -Raw).Trim() -ne 'hello') { Fail 'checkout changed' }
    if (git status --porcelain) { Fail 'checkout not clean' }
    # How the worker was set up.
    $envFile = Join-Path $record 'env'
    Assert-HasLine $envFile "ANTHROPIC_BASE_URL=$url"
    Assert-HasLine $envFile 'ANTHROPIC_AUTH_TOKEN=client-key'
    Assert-HasLine $envFile 'ANTHROPIC_API_KEY=<unset>'
    if (-not ((Get-Content $envFile) -match '^ANTHROPIC_MODEL=claude-')) { Fail 'no Claude model name' }
    Assert-HasLine $envFile 'CLAUDE_CODE_DISABLE_1M_CONTEXT=1'
    Assert-HasLine $envFile 'CLAUDE_CODE_MAX_OUTPUT_TOKENS=8192'
    # At 256K the window is capped at 180K, without a percentage override.
    Assert-HasLine $envFile 'CLAUDE_CODE_AUTO_COMPACT_WINDOW=180000'
    Assert-HasLine $envFile 'CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>'
    # The result shows the summary and the diff.
    la result $id
    if ($code -ne 0) { Fail "result exit ${code}: $out" }
    Assert-Contains $out 'Added a line to README.'
    Assert-Contains $out '+edited by the worker'
}

function test_long_paths {
    # Git in the worktree, the runner's and the worker's, handles paths past
    # Windows' 260 characters, without a change to the repo's config.
    la run task
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    $envFile = Join-Path $record 'env'
    Assert-HasLine $envFile 'GIT_CONFIG_COUNT=1'
    Assert-HasLine $envFile 'GIT_CONFIG_KEY_0=core.longpaths'
    Assert-HasLine $envFile 'GIT_CONFIG_VALUE_0=true'
    if (git config core.longpaths) { Fail "the repo's config changed" }
}

function test_files_without_byte_order_mark {
    $task = 'Unicode ' + [char]0x00DC + [char]0x00EF + ' task'
    la run $task
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    $runDir = Join-Path $stateDir "runs/$(Get-RunId)"
    foreach ($name in 'task', 'meta', 'summary') {
        $bytes = [IO.File]::ReadAllBytes((Join-Path $runDir $name))
        if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) { Fail "$name starts with a byte order mark" }
    }
    Assert-HasLine (Join-Path $runDir 'task') $task
}

function test_autocompact_follows_context {
    # 75 % of 128000, plus the output cap and Claude Code's reserve.
    Set-Health '{"status": 200, "max_context": 128000}'
    la run task
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    Assert-HasLine (Join-Path $record 'env') 'CLAUDE_CODE_AUTO_COMPACT_WINDOW=117192'
    Assert-HasLine (Join-Path $record 'env') 'CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>'
}

function test_compact_share_changes_window {
    Add-Content -LiteralPath $config -Value 'LOCAL_AGENT_COMPACT_SHARE=70'
    Set-Health '{"status": 200, "max_context": 128000}'
    la run task
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    Assert-HasLine (Join-Path $record 'env') 'CLAUDE_CODE_AUTO_COMPACT_WINDOW=110792'
    Assert-HasLine (Join-Path $record 'env') 'CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>'
    # Below Claude Code's 100K minimum the window stays at 100K, and the
    # percentage override brings compaction down to the share.
    Add-Content -LiteralPath $config -Value 'LOCAL_AGENT_COMPACT_SHARE=50'
    la run task
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    Assert-HasLine (Join-Path $record 'env') 'CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000'
    Assert-HasLine (Join-Path $record 'env') 'CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=64'
}

function test_worker_permissions {
    [void](New-Item -ItemType Directory -Path .claude)
    Set-Content -LiteralPath .claude/settings.json -Value '{"permissions": {"allow": ["Bash(cargo test:*)", "Bash(git push:*)"]}}'
    git add .claude
    git commit -qm settings
    la run --allow 'Bash(make check:*)' task
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    if (@(Get-OptionValues '--permission-mode') -cnotcontains 'acceptEdits') { Fail 'not acceptEdits' }
    $allowed = @(Get-OptionValues '--allowedTools')
    $denied = @(Get-OptionValues '--disallowedTools')
    # A rule with a space and parentheses arrives as one argument.
    foreach ($rule in 'Edit', 'Bash(git commit:*)', 'Bash(cargo test:*)', 'Bash(make check:*)') {
        if ($allowed -cnotcontains $rule) { Fail "$rule not allowed: $allowed" }
    }
    if ($allowed -ccontains 'Bash(git push:*)') { Fail 'git push allowed' }
    foreach ($rule in 'Bash(git push:*)', 'WebFetch', 'Bash(curl:*)', 'PowerShell') {
        if ($denied -cnotcontains $rule) { Fail "$rule not denied: $denied" }
    }
    # --allow counts for its run only.
    la run task
    if (@(Get-OptionValues '--allowedTools') -ccontains 'Bash(make check:*)') { Fail '--allow kept' }
    # A denied tool doesn't block tools that merely share its first letters.
    la run --allow 'Bash(ncdu:*)' task
    if (@(Get-OptionValues '--allowedTools') -cnotcontains 'Bash(ncdu:*)') { Fail 'ncdu not allowed' }
    # The PowerShell tool stays off whatever --allow says.
    la run --allow PowerShell --allow 'PowerShell(Get-ChildItem:*)' task
    if (@(Get-OptionValues '--allowedTools') -match '^PowerShell') { Fail 'PowerShell allowed' }
    # Read-only runs work in the checkout: the repo's rules don't apply there.
    $env:FAKE_CLAUDE_MODE = 'no-tool'
    la run --read-only task
    if (@(Get-OptionValues '--allowedTools') -ccontains 'Bash(cargo test:*)') { Fail "repo's rules in a read-only run" }
    if (@(Get-OptionValues '--disallowedTools') -cnotcontains 'PowerShell') { Fail 'PowerShell allowed in a read-only run' }
}

function test_read_only {
    $env:FAKE_CLAUDE_MODE = 'no-tool'
    la run --read-only 'Explain README'
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    if ((Get-Recorded 'cwd') -ne $repo) { Fail 'worker not in the repo' }
    if ((git branch --format='%(refname:short)') -ne 'main') { Fail 'branch created' }
    if (@(git worktree list).Count -ne 1) { Fail 'worktree created' }
    if (@(Get-OptionValues '--allowedTools') -ccontains 'Edit') { Fail 'Edit allowed' }
    foreach ($rule in 'Edit', 'Write', 'Bash(git commit:*)') {
        if (@(Get-OptionValues '--disallowedTools') -cnotcontains $rule) { Fail "$rule not denied" }
    }
    la result (Get-RunId)
    Assert-Contains $out 'I would rather not.'
    if ($out.Contains('## Changes')) { Fail 'changes with --read-only' }
}

# Runs a task that must fall back before the worker starts; $reason is the
# expected reason, $hint a piece of the hint.
function Assert-Fallback([string]$reason, [string]$hint) {
    la run task
    if ($code -ne 3) { Fail "exit $code instead of 3: $out" }
    if (Test-Path (Join-Path $record 'args')) { Fail 'worker started' }
    if ((git branch --format='%(refname:short)') -ne 'main') { Fail 'branch created' }
    la result (Get-RunId)
    Assert-Contains $out 'fallback'
    Assert-Contains $out $reason
    Assert-Contains $out $hint
}
function Get-RequestCount { return @(Get-Content -LiteralPath "$($env:FAKE_PROXY_SCENARIO).requests").Count }

function test_fallback_gaming {
    Set-Health '{"status": 503, "tag": "gaming"}'
    Assert-Fallback 'gaming mode' 'fall back'
    if ((Get-RequestCount) -ne 1) { Fail 'gaming mode retried' }
}

function test_fallback_wake_timeout_retries_once {
    Set-Health '{"status": 504, "tag": "wake_timeout"}' '{"status": 200, "max_context": 262144}'
    la run task
    if ($code -ne 0) { Fail "exit $code after a successful retry: $out" }
    Set-Health '{"status": 504, "tag": "wake_timeout"}'
    Remove-Item -LiteralPath "$($env:FAKE_PROXY_SCENARIO).requests", $record -Recurse -Force
    New-Repo '-2'
    Assert-Fallback 'did not wake' 'fall back'
    if ((Get-RequestCount) -ne 2) { Fail 'not retried exactly once' }
}

function test_fallback_setup_errors {
    Set-Health '{"status": 401, "tag": "unauthorized"}'
    Assert-Fallback 'key' 'Access'
    Set-Health '{"status": 401, "tag": "strata_unauthorized"}'
    Assert-Fallback 'Strata refused' 'Strata key'
    Set-Health '{"status": 503, "tag": "not_configured"}'
    Assert-Fallback 'not set up' 'Strata'
    Set-Health '{"status": 503, "tag": "no_mac"}'
    Assert-Fallback 'MAC' 'PC service'
    Set-Health '{"status": 200, "max_context": 32768}'
    Assert-Fallback '32768' '128K'
    Set-Health '{"status": 200, "max_context": 127999}'
    Assert-Fallback '127999' '128K'
}

function test_fallback_proxy_unreachable {
    Set-Content -LiteralPath $config -Value "LOCAL_AGENT_URL=http://127.0.0.1:9`nLOCAL_AGENT_KEY=client-key"
    Assert-Fallback 'unreachable' 'LOCAL_AGENT_URL'
}

function test_fallback_mid_run_keeps_work {
    $env:FAKE_CLAUDE_MODE = 'crash'
    la run task
    if ($code -ne 3) { Fail "exit $code instead of 3: $out" }
    $id = Get-RunId
    if (@(git show "local-agent/${id}:README") -cnotcontains 'half done') { Fail "the worker's work is missing on the branch" }
    if (-not (Test-Path -LiteralPath (Get-Recorded 'cwd'))) { Fail 'worktree removed' }
    la result $id
    foreach ($part in 'fallback', 'mid-run', 'API Error: 503', '## Log summary', 'Edit README', '+half done') { Assert-Contains $out $part }
}

function test_warns_without_compaction {
    $env:FAKE_CLAUDE_MODE = 'no-compact'
    la run task
    if ($code -ne 3) { Fail "exit $code instead of 3: $out" }
    $id = Get-RunId
    la status $id
    Assert-Contains $out 'context: 190K now (peak 190K, compacts near 159K), compactions: 0'
    Assert-Contains $out 'warning: context past the compaction point'
    # The fallback's log summary names the cause.
    la result $id
    Assert-Contains $out 'API Error: 400'
    Assert-Contains $out 'compactions: 0'
}

function test_fallback_mid_run_gaming {
    $env:FAKE_CLAUDE_MODE = 'crash-gaming'
    la run task
    if ($code -ne 3) { Fail "exit $code instead of 3: $out" }
    la result (Get-RunId)
    Assert-Contains $out 'mid-run'
    Assert-Contains $out 'the PC switched to gaming mode'
}

function test_time_limit {
    $env:FAKE_CLAUDE_MODE = 'hang'
    $started = Get-Date
    la run --timeout 2s task
    if ($code -ne 4) { Fail "exit $code instead of 4: $out" }
    if (((Get-Date) - $started).TotalSeconds -ge 15) { Fail 'time limit not kept' }
    Start-Sleep -Milliseconds 500
    foreach ($id in (Get-Recorded 'pid'), (Get-Recorded 'child')) {
        if (Test-Alive $id) { Fail "process $id still running" }
    }
    $run = Get-RunId
    if (-not (Test-Branch "local-agent/$run")) { Fail 'branch removed' }
    if (-not (Test-Path -LiteralPath (Get-Recorded 'cwd'))) { Fail 'worktree removed' }
    la result $run
    Assert-Contains $out 'timeout'
    Assert-Contains $out 'Bash sleep 1000'
    # Durations: seconds, minutes, hours; anything else is refused.
    la run --timeout soon task
    if ($code -ne 2) { Fail "--timeout soon: exit $code" }
}

function test_status {
    # While running.
    $env:FAKE_CLAUDE_MODE = 'slow'
    $env:FAKE_CLAUDE_SLEEP = '3'
    $runOut = Join-Path $tmp 'run.out'
    $background = Start-Runner $runOut @('run', "First line of the task`nsecond line")
    for ($i = 0; $i -lt 50; $i++) {
        la status
        if ($out.Contains('running')) { break }
        Start-Sleep -Milliseconds 100
    }
    Assert-Contains $out 'running'
    Assert-Contains $out 'First line of the task'
    $background.WaitForExit()
    if ($background.ExitCode -ne 0) { Fail "run failed: $(Get-Content -Raw $runOut)" }
    # Done; without a run id it shows the latest run of this repo.
    la status
    if ($code -ne 0) { Fail "status exit $code" }
    foreach ($part in 'done', 'Edit README', 'context compacted', 'context: 20K now (peak 50K, compacts near 159K), compactions: 1') { Assert-Contains $out $part }
    if ($out.Contains('warning:')) { Fail "warning despite compaction: $out" }
    # Fallen back.
    Set-Health '{"status": 503, "tag": "gaming"}'
    la run task
    la status (Get-RunId)
    Assert-Contains $out 'fallback'
    Assert-Contains $out 'the PC is in gaming mode'
}

function test_one_run_at_a_time {
    $env:FAKE_CLAUDE_MODE = 'slow'
    $env:FAKE_CLAUDE_SLEEP = '2'
    $runs = @()
    foreach ($task in 'A', 'B', 'C') {
        $runs += Start-Runner (Join-Path $tmp "run-$task.out") @('run', $task)
        Start-Sleep -Milliseconds 500
    }
    la status
    Assert-Contains $out 'queued'
    foreach ($r in $runs) {
        $r.WaitForExit()
        if ($r.ExitCode -ne 0) { Fail "a run failed: exit $($r.ExitCode)" }
    }
    # The workers ran one after another, in the order the runs started.
    $spans = @(Get-Content -LiteralPath (Join-Path $record 'spans') | ForEach-Object { , ($_ -split ' ') } | Sort-Object { [double]$_[0] })
    if (($spans | ForEach-Object { $_[2] }) -join '' -ne 'ABC') { Fail "order: $(Get-Recorded 'spans')" }
    for ($i = 1; $i -lt $spans.Count; $i++) {
        if ([double]$spans[$i][0] -lt [double]$spans[$i - 1][1]) { Fail "workers ran at the same time: $(Get-Recorded 'spans')" }
    }
}

function test_stale_queue_entry {
    # A runner killed while waiting leaves its entry behind; it must not block.
    [void](New-Item -ItemType Directory -Force -Path (Join-Path $stateDir 'queue'))
    Set-Content -LiteralPath (Join-Path $stateDir 'queue/0000000000-dead') -Value '999999'
    $started = Get-Date
    la run task
    if ($code -ne 0) { Fail "exit ${code}: $out" }
    if (((Get-Date) - $started).TotalSeconds -ge 10) { Fail 'a dead entry held up the run' }
}

function test_interrupted_run {
    $env:FAKE_CLAUDE_MODE = 'hang'
    $runOut = Join-Path $tmp 'run.out'
    $background = Start-Runner $runOut @('run', 'task')
    for ($i = 0; $i -lt 100 -and -not (Test-Path (Join-Path $record 'child')); $i++) { Start-Sleep -Milliseconds 100 }
    # Ctrl+C.
    & /bin/kill -INT $background.Id
    $background.WaitForExit()
    Start-Sleep -Milliseconds 500
    foreach ($id in (Get-Recorded 'pid'), (Get-Recorded 'child')) {
        if (Test-Alive $id) { Fail "process $id still running after cancelling" }
    }
    la status
    Assert-Contains $out 'stopped'
    # A runner that died without cleaning up counts as stopped, too.
    if ((Get-Content -Raw $runOut) -notmatch '(?m)^local-agent: run (\S+)') { Fail 'no run id' }
    $meta = Join-Path $stateDir "runs/$($Matches[1])/meta"
    (Get-Content $meta) -replace '^state=.*', 'state=running' -replace '^pid=.*', 'pid=999999' | Set-Content $meta
    la status $Matches[1]
    Assert-Contains $out 'stopped'
}

function test_fallback_gaming_during_run {
    $env:FAKE_CLAUDE_MODE = 'hang-gaming'
    $env:LOCAL_AGENT_PROBE_INTERVAL = '1'
    $started = Get-Date
    la run task
    if ($code -ne 3) { Fail "exit $code instead of 3: $out" }
    if (((Get-Date) - $started).TotalSeconds -ge 15) { Fail 'gaming mode noticed too late' }
    if (Test-Alive (Get-Recorded 'child')) { Fail 'worker still running' }
    la result (Get-RunId)
    Assert-Contains $out 'switched to gaming mode'
    Assert-Contains $out '## Log summary'
}

$tests = @($args)
if ($tests.Count -eq 0) {
    $tests = @(Get-ChildItem function:test_* | Sort-Object Name | ForEach-Object { $_.Name })
}
try {
    foreach ($test in $tests) {
        $script:current = $test
        try {
            Reset-Test
            & $test
        } catch {
            Fail "aborted: $_"
        }
    }
} finally {
    Set-Location -LiteralPath $here
    Stop-Process -Id $proxy.Id -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
if ($script:failures -gt 0) {
    [Console]::Error.WriteLine("local-agent.ps1: $($script:failures) failures")
    exit 1
}
[Console]::Out.WriteLine("local-agent.ps1: ok ($($tests.Count) tests)")
