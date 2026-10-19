# Strata Home Proxy

Ein Proxy auf dem NAS, der Anfragen an Strata auf dem PC weiterleitet, den PC dafür weckt und ihn nach einer Leerlaufzeit schlafen schickt.

Die _Avoid_-Listen gelten für Doku, Issues, Code-Bezeichner und deutsche Texte. Die englische Weboberfläche darf Produkte beim Namen nennen, wo der Nutzer das Gerät daran erkennt (z. B. „Shelly plug“).

## Beteiligte

**Strata**:
Die eigene LLM-Engine auf dem PC (Niko1221/Strata) mit einer OpenAI- und Anthropic-kompatiblen API. Kein llama-server, auch wenn manche Endpunkte dessen Namen tragen.
_Avoid_: llama-server, llama.cpp, Modellserver

**PC-Dienst**:
Das kleine Programm auf dem PC, das dem Proxy Zustand, Gaming-Modus und Benutzeraktivität meldet und auf Anfrage den Standby auslöst.
_Avoid_: Agent, PC-Agent (nur im Code und in Dateinamen)

**Proxy**:
Der Dienst auf dem NAS, über den alle Anfragen an Strata laufen.
_Avoid_: NAS-Proxy (nur im Code), Gateway

**Weboberfläche**:
Die Seiten des Proxys unter `/admin`, gegliedert in die Bereiche Overview (Übersicht), PC, Strata, Access und System, dazu die Anmeldung. Die Oberfläche ist englisch; die Fachbegriffe hier bleiben deutsch.
_Avoid_: Admin-UI, Dashboard

**Einrichtungsbefehl**:
Der PowerShell-Einzeiler aus dem Bereich *PC* der Weboberfläche, der den PC-Dienst vom NAS lädt, einrichtet und beim Proxy anmeldet.
_Avoid_: Installer, Setup-Link

**Einmalcode**:
Der Code im Einrichtungsbefehl. Gilt kurz und nur für eine Anmeldung; ein neuer Einrichtungsbefehl macht den alten ungültig.
_Avoid_: Pairing-Code, Token (das ist das gemeinsame Geheimnis von Proxy und PC-Dienst)

**Übersicht**:
Die Startseite der Weboberfläche mit dem Live-Zustand von PC und Strata und den Knöpfen.
_Avoid_: Status-Seite, Home

## Zustand des PCs

**Aufwachen**:
Der PC kommt nach einem Weckpaket hoch, bis Strata (oder für den Gaming-Modus der PC-Dienst) antwortet.
_Avoid_: Booten, Starten

**Leerlaufzeit**:
Die Zeit ohne Anfrage über den Proxy, nach der der Proxy den PC schlafen schicken will. Sie beginnt neu bei jeder Anfrage, beim Aufwachen und nach dem Gaming-Modus.
_Avoid_: Idle-Timeout, Standby-Zeit

**Schlafen-Anfrage**:
Die Bitte des Proxys an den PC-Dienst, den Standby auszulösen. Der PC-Dienst kann sie mit einem Grund ablehnen.
_Avoid_: Shutdown, Sleep-Befehl

**Nachlauf**:
Die kurze Zeit nach einer angenommenen Schlafen-Anfrage, in der der PC noch antworten kann und trotzdem als schlafend gilt.
_Avoid_: Settling (nur im Code)

**Hardwarewerte**:
Was der PC-Dienst über Last, Temperatur, Speicher und Lüfter von Grafikkarte und CPU meldet; die Übersicht zeigt sie.
_Avoid_: Stats, Telemetrie

**Messsteckdose**:
Die Shelly-Steckdose, an der der PC hängt. Der Proxy fragt sie nur, wenn sie eingerichtet ist (`SHELLY_URL`): für die Übersicht und einmal pro Minute für den Verlauf (`PLUG_SAMPLE_SECS`). Auf Wecken und Schlafen hat sie keinen Einfluss.
_Avoid_: Shelly, Smart Plug (nur im Code)

**Leistung**:
Was die Messsteckdose als aktuelle Leistungsaufnahme des PCs meldet, in Watt; die Übersicht zeigt sie im Bereich Hardware als *Power*, auch während der PC schläft.
_Avoid_: Stromverbrauch, Energie

**Gaming-Modus**:
Ein Zustand des PCs, in dem Strata beendet ist, Anfragen abgewiesen werden und der Proxy den PC nicht schlafen schickt.
_Avoid_: Spielemodus

**Wach halten**:
Ein Zeitraum, in dem der Proxy den PC nicht schlafen schickt, gewählt auf der Übersicht.
_Avoid_: Keep-Alive

## Zustand von Strata

**Modell nicht geladen**:
Strata antwortet, hält das Modell aber nicht im Speicher und lädt es mit der nächsten Anfrage.
_Avoid_: Entladen, Unloaded (nur im Code)

**Lädt**:
Strata läuft laut PC-Dienst, antwortet aber noch nicht.
_Avoid_: Startet

**Ereignis**:
Ein Eintrag im Verlauf des Proxys über PC, Wecken, Schlafen oder die Weboberfläche.
_Avoid_: Log-Eintrag

**Strata-Ausgabe**:
Was ein vom PC-Dienst gestartetes Strata auf seine Konsole schreibt; der PC-Dienst merkt sich die letzten 500 Zeilen, die Weboberfläche zeigt sie live im Bereich Strata (Reiter *Output*). Kein Ereignis.
_Avoid_: Strata-Log, Log-Eintrag (nur `strata-log` und `strata.log` im Code und in Dateinamen)
