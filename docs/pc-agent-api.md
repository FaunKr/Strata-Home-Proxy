# Schnittstelle zwischen NAS-Proxy und PC-Dienst

Der Dienst auf dem PC (Schritt 3) bietet diese Endpunkte an. Der Proxy nutzt
sie schon jetzt; die Tests bilden den Dienst nach.

Alle Anfragen tragen `Authorization: Bearer <PC_AGENT_TOKEN>`; der Dienst
verlangt das Token immer. Er nimmt Anfragen nur von `allowed_clients` (dem
NAS) und von 127.0.0.1 an.

## `GET /status`

```json
{ "gaming_mode": false, "strata_running": true, "user_idle_secs": 1840 }
```

Antwortet der Dienst nicht, geht der Proxy davon aus, dass der PC schläft,
und weckt ihn.

## `POST /sleep`

Der Proxy schickt das nach der Leerlaufzeit. Der Dienst prüft Gaming-Modus,
laufende Strata-Anfragen (über `/slots`, falls Strata das anbietet) und
Benutzeraktivität (letzte Tastatur- oder Mauseingabe, Vorgabe 10 min).
Die Antwort kommt zuerst, Standby folgt 2 s später.

- `200 {"accepted": true}`: Standby wird ausgelöst.
- `409 {"accepted": false, "reason": "Benutzer aktiv"}`: abgelehnt. Der
  Proxy fragt nach `SLEEP_RETRY_SECS` erneut.

## `POST /gaming` (für die Weboberfläche)

```json
{ "enabled": true }
```

An: Strata beenden, Schlafen sperren. Aus: Strata starten, Schlafen erlauben.
