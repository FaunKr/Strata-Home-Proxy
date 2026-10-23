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
    Remove-Item Env:FAKE_CLAUDE_MODE, Env:PREFIX -ErrorAction SilentlyContinue
    Write-Config
    Set-Health '{"status": 200, "max_context": 262144}'
    New-Repo
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
    la check
    if ($code -ne 2) { Fail "exit ${code}: $out" }
    Assert-Contains $out $config
    if (Test-Path (Join-Path $record 'args')) { Fail 'worker started' }
    # A config others can read is refused.
    Write-Config
    & chmod 644 $config
    la check
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
