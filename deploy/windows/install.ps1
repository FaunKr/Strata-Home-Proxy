# Installs, updates or removes the Strata PC service for the signed-in user.
#
# From the NAS (PC area of the web UI), in a PowerShell:
#   irm http://<NAS>:<Port>/admin/agent/install.ps1?code=<Code> | iex
#   irm http://<NAS>:<Port>/admin/agent/install.ps1?uninstall | iex
# The NAS fills in NasUrl, Code and Uninstall below.
#
# Without the NAS, in the folder with a self-built strata-pc-agent.exe:
#   powershell -ExecutionPolicy Bypass -File install.ps1 -NasIp 192.168.1.10
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall
param(
    [string]$NasIp = "",
    [string]$NasUrl = "",
    [string]$Code = "",
    [switch]$Uninstall,
    [string]$Target = "$env:LOCALAPPDATA\StrataHome",
    [int]$Port = 8091
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
# Name of the scheduled task and firewall rule; existing installs have it.
$TaskName = "Strata PC-Dienst"
# The firewall rule for Strata's port: a new Strata program after an update
# needs no Windows prompt then.
$StrataRule = "Strata from the NAS"
# Strata's port; the NAS names the one in its Strata address.
$StrataPort = 8080
$exe = Join-Path $Target "strata-pc-agent.exe"
$config = Join-Path $Target "strata-pc-agent.toml"
# Existing installs have this shortcut name.
$shortcut = Join-Path ([Environment]::GetFolderPath("Desktop")) "Gaming an-aus.lnk"

function Test-Admin {
    $id = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    $id.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# The firewall rule and the scheduled task need administrator rights. Without
# them the script restarts itself after a UAC prompt; the code is still valid
# then, because only the registration with the NAS uses it up.
if (-not (Test-Admin)) {
    Write-Host "Administrator rights needed, Windows will ask in a moment."
    if ($NasUrl) {
        $query = if ($Uninstall) { "uninstall" } else { "code=$Code" }
        $command = "irm '$NasUrl/admin/agent/install.ps1?$query' | iex"
        $arguments = "-NoProfile -NoExit -ExecutionPolicy Bypass -Command `"$command`""
    } else {
        $arguments = "-NoProfile -NoExit -ExecutionPolicy Bypass -File `"$PSCommandPath`""
        foreach ($p in $PSBoundParameters.GetEnumerator()) {
            $arguments += if ($p.Value -is [switch]) { " -$($p.Key)" } else { " -$($p.Key) `"$($p.Value)`"" }
        }
    }
    Start-Process powershell -Verb RunAs -ArgumentList $arguments
    return
}

function Stop-Agent {
    Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue | Stop-ScheduledTask
    Stop-Process -Name strata-pc-agent -ErrorAction SilentlyContinue
    # Windows releases the .exe only shortly after it ends.
    Start-Sleep -Seconds 1
}

if ($Uninstall) {
    Stop-Agent
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
    Get-NetFirewallRule -DisplayName $TaskName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Get-NetFirewallRule -DisplayName $StrataRule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Remove-Item $shortcut -ErrorAction SilentlyContinue
    Remove-Item $Target -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "PC service removed. On the NAS, reset PC_AGENT_URL and PC_AGENT_TOKEN in the settings."
    return
}

if (-not $NasUrl -and -not $NasIp) {
    throw "Without the NAS, please give -NasIp, e.g. -NasIp 192.168.1.10"
}

New-Item -ItemType Directory -Force -Path $Target | Out-Null
$update = Test-Path $config

if ($NasUrl) {
    # Before the registration, so an error here does not use up the code.
    $nasUri = [Uri]$NasUrl
    $nasHost = $nasUri.Host.Trim("[]")
    $NasIp = [System.Net.Dns]::GetHostAddresses($nasHost) |
        Where-Object AddressFamily -eq "InterNetwork" |
        Select-Object -First 1 -ExpandProperty IPAddressToString
    if (-not $NasIp) {
        throw "No IPv4 address found for $nasHost. Open the web UI through the NAS's IP and create a new install command."
    }
    # Through the NAS's IPv4, so it sees the PC from the address it later uses
    # to reach it (the firewall and allowed_clients apply to IPv4).
    $NasBase = "http://${NasIp}:$($nasUri.Port)"
}

if ($NasUrl) {
    # Download first, then stop the running service: if the download fails, the
    # old one keeps running. A broken-off download does not use up the code either.
    $download = "$exe.neu"
    Invoke-WebRequest -UseBasicParsing "$NasBase/admin/agent/strata-pc-agent.exe" -OutFile $download
    Stop-Agent
    Move-Item $download $exe -Force
} else {
    $here = Split-Path -Parent $PSCommandPath
    Stop-Agent
    Copy-Item "$here\strata-pc-agent.exe" $exe -Force
}

if ($NasUrl) {
    # Also on a PC already set up: so a new install command registers it again,
    # e.g. after the token or address was reset on the NAS.
    $body = @{ code = $Code; port = $Port } | ConvertTo-Json
    try {
        $reg = Invoke-RestMethod -Method Post "$NasBase/admin/agent/register" -ContentType "application/json" -Body $body
    } catch {
        throw "Registration with the NAS failed. The one-time code is valid only once and only briefly: create a new install command in the PC area of the web UI. ($_)"
    }
    # A NAS before the Strata port does not name it.
    if ($reg.strata_port) { $StrataPort = [int]$reg.strata_port }
    # What the registration sets is replaced; everything else in an existing
    # config stays. A strata_api_key of an older install goes: the service
    # makes the Strata key itself.
    $own = "^\s*(listen|token|allowed_clients|nas_url|strata_api_key)\s*="
    if ($update) {
        $lines = @(Get-Content $config | Where-Object { $_ -notmatch $own })
    } else {
        $lines = @(
            "# PC service config, written by the install command from the NAS."
            "# The start script and Strata programs are set in the Strata area of the web UI."
            "strata_url = `"http://127.0.0.1:8080`""
            "fresh_strata_after_standby = true"
        )
    }
    $lines += "listen = `"0.0.0.0:$Port`""
    $lines += "token = `"$($reg.token)`""
    $lines += "allowed_clients = [`"$NasIp`"]"
    $lines += "nas_url = `"$NasBase`""
    # Without a BOM, otherwise the service does not read the first line.
    [IO.File]::WriteAllLines($config, $lines)
    Write-Host "Registered with the NAS, config: $config"
} elseif ($update) {
    # Strata's port from the address the service reaches it at.
    $url = Get-Content $config | Where-Object { $_ -match '^\s*strata_url\s*=' } | Select-Object -First 1
    if ($url -match '"http://[^"/]+:(\d+)') { $StrataPort = [int]$Matches[1] }
    # Only the NAS address can have changed.
    $lines = Get-Content $config | ForEach-Object {
        if ($_ -match "^\s*allowed_clients\s*=") { "allowed_clients = [`"$NasIp`"]" } else { $_ }
    }
    [IO.File]::WriteAllLines($config, [string[]]$lines)
    Write-Host "PC service updated, the config stays except for the NAS address: $config"
} else {
    $token = -join ((48..57) + (97..122) | Get-Random -Count 40 | ForEach-Object { [char]$_ })
    (Get-Content "$here\strata-pc-agent.example.toml" -Raw) `
        -replace 'put-a-long-random-secret-here', $token `
        -replace '192\.168\.1\.10', $NasIp |
        Set-Content -Encoding UTF8 $config
    Write-Host "Config created: $config"
    Write-Host "Token for the NAS (PC_AGENT_TOKEN): $token"
    Write-Host "Please check strata_start and strata_processes in the config."
}

# Task, firewall rule and shortcut anew on every run, so a new NAS address or
# a new port arrives there.
# Start at sign-in, in the user's context (for user activity and the Strata window).
$action = New-ScheduledTaskAction -Execute $exe -WorkingDirectory $Target
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Settings $settings -Force | Out-Null

# Open the port only for the NAS (and locally for the shortcut).
Get-NetFirewallRule -DisplayName $TaskName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName $TaskName -Direction Inbound -Protocol TCP `
    -LocalPort $Port -RemoteAddress $NasIp -Action Allow | Out-Null
Get-NetFirewallRule -DisplayName $StrataRule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName $StrataRule -Direction Inbound -Protocol TCP `
    -LocalPort $StrataPort -RemoteAddress $NasIp -Action Allow | Out-Null

# Desktop shortcut that switches gaming mode on and off.
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($shortcut)
$link.TargetPath = $exe
$link.Arguments = "gaming toggle"
$link.WorkingDirectory = $Target
$link.IconLocation = "$env:SystemRoot\System32\imageres.dll,-1023"
$link.Save()

Start-ScheduledTask -TaskName $TaskName
Write-Host "Done. Log: $(Join-Path $Target 'strata-pc-agent.log')"
