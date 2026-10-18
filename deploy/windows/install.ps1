# Installiert den Strata PC-Dienst für den angemeldeten Benutzer.
# In PowerShell als Administrator, im Ordner mit strata-pc-agent.exe:
#   powershell -ExecutionPolicy Bypass -File install.ps1 -NasIp 192.168.1.10
param(
    [Parameter(Mandatory = $true)][string]$NasIp,
    [string]$Target = "$env:LOCALAPPDATA\StrataHome",
    [int]$Port = 8091
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

New-Item -ItemType Directory -Force -Path $Target | Out-Null
Get-ScheduledTask -TaskName "Strata PC-Dienst" -ErrorAction SilentlyContinue | Stop-ScheduledTask
Stop-Process -Name strata-pc-agent -ErrorAction SilentlyContinue
Copy-Item "$here\strata-pc-agent.exe" $Target -Force
$exe = Join-Path $Target "strata-pc-agent.exe"
$config = Join-Path $Target "strata-pc-agent.toml"

if (-not (Test-Path $config)) {
    $token = -join ((48..57) + (97..122) | Get-Random -Count 40 | ForEach-Object { [char]$_ })
    (Get-Content "$here\strata-pc-agent.example.toml" -Raw) `
        -replace 'hier-ein-langes-zufaelliges-geheimnis', $token `
        -replace '192\.168\.1\.10', $NasIp |
        Set-Content -Encoding UTF8 $config
    Write-Host "Konfiguration angelegt: $config"
    Write-Host "Token fuer das NAS (PC_AGENT_TOKEN): $token"
    Write-Host "Bitte strata_start und strata_processes in der Konfiguration pruefen."
}

# Start bei Anmeldung, im Kontext des Benutzers (für Benutzeraktivität und Strata-Fenster).
$action = New-ScheduledTaskAction -Execute $exe -WorkingDirectory $Target
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName "Strata PC-Dienst" -Action $action -Trigger $trigger `
    -Settings $settings -Force | Out-Null

# Port nur für das NAS (und lokal für die Verknüpfung) öffnen.
Get-NetFirewallRule -DisplayName "Strata PC-Dienst" -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName "Strata PC-Dienst" -Direction Inbound -Protocol TCP `
    -LocalPort $Port -RemoteAddress $NasIp -Action Allow | Out-Null

# Desktop-Verknüpfung "Gaming an/aus".
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath("Desktop")) "Gaming an-aus.lnk"))
$link.TargetPath = $exe
$link.Arguments = "gaming umschalten"
$link.WorkingDirectory = $Target
$link.IconLocation = "$env:SystemRoot\System32\imageres.dll,-1023"
$link.Save()

Start-ScheduledTask -TaskName "Strata PC-Dienst"
Write-Host "Fertig. Log: $(Join-Path $Target 'strata-pc-agent.log')"
