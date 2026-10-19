# Installiert, aktualisiert oder entfernt den Strata PC-Dienst für den angemeldeten Benutzer.
#
# Vom NAS (Seite "PC-Dienst" der Weboberfläche), in einer PowerShell:
#   irm http://<NAS>:<Port>/admin/agent/install.ps1?code=<Code> | iex
#   irm http://<NAS>:<Port>/admin/agent/install.ps1?uninstall | iex
# Der NAS setzt NasUrl, Code und Uninstall unten ein.
#
# Ohne NAS, im Ordner mit einer selbst gebauten strata-pc-agent.exe:
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
$TaskName = "Strata PC-Dienst"
$exe = Join-Path $Target "strata-pc-agent.exe"
$config = Join-Path $Target "strata-pc-agent.toml"
$shortcut = Join-Path ([Environment]::GetFolderPath("Desktop")) "Gaming an-aus.lnk"

function Test-Admin {
    $id = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    $id.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Firewall-Regel und Aufgabenplanung brauchen Administratorrechte. Ohne sie
# startet sich das Skript nach einer UAC-Abfrage neu; der Code gilt dann noch,
# weil erst die Anmeldung beim NAS ihn verbraucht.
if (-not (Test-Admin)) {
    Write-Host "Administratorrechte noetig, Windows fragt gleich nach."
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
    # Windows gibt die .exe erst kurz nach dem Beenden frei.
    Start-Sleep -Seconds 1
}

if ($Uninstall) {
    Stop-Agent
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
    Get-NetFirewallRule -DisplayName $TaskName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Remove-Item $shortcut -ErrorAction SilentlyContinue
    Remove-Item $Target -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "PC-Dienst entfernt. Auf dem NAS unter Einstellungen PC_AGENT_URL und PC_AGENT_TOKEN zuruecksetzen."
    return
}

if (-not $NasUrl -and -not $NasIp) {
    throw "Ohne NAS bitte -NasIp angeben, z. B. -NasIp 192.168.1.10"
}

New-Item -ItemType Directory -Force -Path $Target | Out-Null
$update = Test-Path $config

if ($NasUrl) {
    # Vor der Anmeldung, damit ein Fehler hier den Code nicht verbraucht.
    $nasUri = [Uri]$NasUrl
    $nasHost = $nasUri.Host.Trim("[]")
    $NasIp = [System.Net.Dns]::GetHostAddresses($nasHost) |
        Where-Object AddressFamily -eq "InterNetwork" |
        Select-Object -First 1 -ExpandProperty IPAddressToString
    if (-not $NasIp) {
        throw "Keine IPv4-Adresse fuer $nasHost gefunden. Die Weboberflaeche ueber die IP des NAS oeffnen und einen neuen Befehl erzeugen."
    }
    # Ueber die IPv4 des NAS, damit er den PC unter der Adresse sieht, ueber
    # die er ihn spaeter anspricht (Firewall und allowed_clients gelten fuer IPv4).
    $NasBase = "http://${NasIp}:$($nasUri.Port)"
}

if ($NasUrl) {
    # Erst laden, dann den laufenden Dienst beenden: scheitert der Download,
    # laeuft der alte weiter. Ein abgebrochener Download verbraucht auch den Code nicht.
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
    # Auch bei einem schon eingerichteten PC: so laesst er sich mit einem neuen
    # Befehl neu anmelden, z. B. nachdem auf dem NAS Token oder Adresse
    # zurueckgesetzt wurden.
    $body = @{ code = $Code; port = $Port } | ConvertTo-Json
    try {
        $reg = Invoke-RestMethod -Method Post "$NasBase/admin/agent/register" -ContentType "application/json" -Body $body
    } catch {
        throw "Anmeldung beim NAS fehlgeschlagen. Der Code gilt nur einmal und kurz: im Bereich PC der Weboberflaeche einen neuen Befehl erzeugen. ($_)"
    }
    # Was die Anmeldung festlegt, wird ersetzt; alles andere in einer
    # vorhandenen Konfiguration bleibt.
    $own = "^\s*(listen|token|allowed_clients|nas_url|strata_api_key)\s*="
    if ($update) {
        $lines = @(Get-Content $config | Where-Object { $_ -notmatch $own })
    } else {
        $lines = @(
            "# Konfiguration des PC-Dienstes, angelegt bei der Einrichtung vom NAS."
            "# Startskript und Strata-Programme setzt die Seite Strata der Weboberflaeche."
            "strata_url = `"http://127.0.0.1:8080`""
            "fresh_strata_after_standby = true"
        )
    }
    $lines += "listen = `"0.0.0.0:$Port`""
    $lines += "token = `"$($reg.token)`""
    $lines += "allowed_clients = [`"$NasIp`"]"
    $lines += "nas_url = `"$NasBase`""
    if ($reg.strata_api_key) {
        # TOML-Zeichenkette: \ und " escapen.
        $key = $reg.strata_api_key -replace '\\', '\\' -replace '"', '\"'
        $lines += "strata_api_key = `"$key`""
    }
    # Ohne BOM, sonst liest der Dienst die erste Zeile nicht.
    [IO.File]::WriteAllLines($config, $lines)
    Write-Host "Beim NAS angemeldet, Konfiguration: $config"
} elseif ($update) {
    # Nur die NAS-Adresse kann sich geaendert haben.
    $lines = Get-Content $config | ForEach-Object {
        if ($_ -match "^\s*allowed_clients\s*=") { "allowed_clients = [`"$NasIp`"]" } else { $_ }
    }
    [IO.File]::WriteAllLines($config, [string[]]$lines)
    Write-Host "PC-Dienst aktualisiert, Konfiguration bleibt bis auf die NAS-Adresse: $config"
} else {
    $token = -join ((48..57) + (97..122) | Get-Random -Count 40 | ForEach-Object { [char]$_ })
    (Get-Content "$here\strata-pc-agent.example.toml" -Raw) `
        -replace 'hier-ein-langes-zufaelliges-geheimnis', $token `
        -replace '192\.168\.1\.10', $NasIp |
        Set-Content -Encoding UTF8 $config
    Write-Host "Konfiguration angelegt: $config"
    Write-Host "Token fuer das NAS (PC_AGENT_TOKEN): $token"
    Write-Host "Bitte strata_start und strata_processes in der Konfiguration pruefen."
}

# Aufgabe, Firewall-Regel und Verknüpfung bei jedem Lauf neu, damit eine neue
# NAS-Adresse oder ein neuer Port dort ankommt.
# Start bei Anmeldung, im Kontext des Benutzers (für Benutzeraktivität und Strata-Fenster).
$action = New-ScheduledTaskAction -Execute $exe -WorkingDirectory $Target
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Settings $settings -Force | Out-Null

# Port nur für das NAS (und lokal für die Verknüpfung) öffnen.
Get-NetFirewallRule -DisplayName $TaskName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName $TaskName -Direction Inbound -Protocol TCP `
    -LocalPort $Port -RemoteAddress $NasIp -Action Allow | Out-Null

# Desktop-Verknüpfung "Gaming an/aus".
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($shortcut)
$link.TargetPath = $exe
$link.Arguments = "gaming umschalten"
$link.WorkingDirectory = $Target
$link.IconLocation = "$env:SystemRoot\System32\imageres.dll,-1023"
$link.Save()

Start-ScheduledTask -TaskName $TaskName
Write-Host "Fertig. Log: $(Join-Path $Target 'strata-pc-agent.log')"
