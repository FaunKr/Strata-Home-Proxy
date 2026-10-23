# Installs local-agent on Windows to %LOCALAPPDATA%\Programs\local-agent (or
# $env:PREFIX), adds that folder to the user PATH, and creates the
# configuration file, readable only by you, if there is none yet. Needs no
# administrator rights. Run it with:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\local-agent\install.ps1
$ErrorActionPreference = 'Stop'
$onWindows = -not (Test-Path variable:IsWindows) -or $IsWindows

function Get-BaseDir([string]$windowsVar, [string]$xdgVar, [string]$fallback) {
    $dir = [Environment]::GetEnvironmentVariable($windowsVar)
    if (-not $dir) { $dir = [Environment]::GetEnvironmentVariable($xdgVar) }
    if (-not $dir) { $dir = Join-Path $HOME $fallback }
    return $dir
}

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$prefix = $env:PREFIX
if (-not $prefix) { $prefix = Join-Path (Join-Path (Get-BaseDir 'LOCALAPPDATA' 'XDG_DATA_HOME' '.local/share') 'Programs') 'local-agent' }
$config = Join-Path (Join-Path (Get-BaseDir 'APPDATA' 'XDG_CONFIG_HOME' '.config') 'local-agent') 'env'

foreach ($tool in 'git', 'claude') {
    if (-not (Get-Command $tool -CommandType Application -ErrorAction SilentlyContinue)) {
        [Console]::Error.WriteLine("Warning: $tool not found; local-agent needs it.")
    }
}

# The shims go on the PATH; the runner sits below them, so PowerShell never
# runs it directly, which the execution policy may forbid.
$lib = Join-Path $prefix 'lib'
[void](New-Item -ItemType Directory -Force -Path $lib)
Copy-Item -LiteralPath (Join-Path $here 'local-agent.ps1') -Destination $lib -Force
Copy-Item -LiteralPath (Join-Path $here 'shims/local-agent.cmd') -Destination $prefix -Force
Copy-Item -LiteralPath (Join-Path $here 'shims/local-agent') -Destination $prefix -Force
if (-not $onWindows) { & chmod 755 (Join-Path $prefix 'local-agent') }
[Console]::Out.WriteLine("Installed $(Join-Path $prefix 'local-agent')")

$separator = [IO.Path]::PathSeparator
if ($onWindows) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    if (($userPath.Split($separator) | Where-Object { $_.TrimEnd('\') -eq $prefix.TrimEnd('\') }).Count -eq 0) {
        [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd($separator) + $separator + $prefix).TrimStart($separator), 'User')
        [Console]::Out.WriteLine("Added $prefix to your user PATH; open a new terminal to use it.")
    }
} elseif (($env:PATH.Split($separator) | Where-Object { $_ -eq $prefix }).Count -eq 0) {
    [Console]::Out.WriteLine("Add $prefix to your PATH.")
}

if (-not (Test-Path -LiteralPath $config)) {
    [void](New-Item -ItemType Directory -Force -Path (Split-Path -Parent $config))
    [IO.File]::WriteAllText($config, "# local-agent: the Proxy on the NAS and a client key from Access in its web UI.`nLOCAL_AGENT_URL=`nLOCAL_AGENT_KEY=`n", (New-Object Text.UTF8Encoding($false)))
    # Only you may read it: it will hold your key.
    if ($onWindows) {
        $acl = Get-Acl -LiteralPath $config
        $acl.SetAccessRuleProtection($true, $false)
        foreach ($rule in @($acl.Access)) { [void]$acl.RemoveAccessRule($rule) }
        $user = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($user, 'FullControl', 'Allow')))
        Set-Acl -LiteralPath $config -AclObject $acl
    } else {
        & chmod 600 $config
    }
    [Console]::Out.WriteLine("Fill in $config, then run: local-agent check")
}
