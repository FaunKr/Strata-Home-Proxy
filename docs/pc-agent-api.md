# Schnittstelle zwischen NAS-Proxy und PC-Dienst

Der Dienst auf dem PC (Schritt 3) bietet diese Endpunkte an. Der Proxy nutzt
sie schon jetzt; die Tests bilden den Dienst nach.

Alle Anfragen tragen `Authorization: Bearer <PC_AGENT_TOKEN>`, wenn ein
Token gesetzt ist. Der Dienst nimmt Anfragen nur vom NAS an.

## `GET /status`

```json
{ "gaming_mode": false, "strata_running": true }
```

Antwortet der Dienst nicht, geht der Proxy davon aus, dass der PC schläft,
und weckt ihn.

## `POST /sleep`

Der Proxy schickt das nach der Leerlaufzeit. Der Dienst prüft Gaming-Modus,
laufende Strata-Anfragen und Benutzeraktivität.

- `200 {"accepted": true}`: Standby wird ausgelöst.
- `409 {"accepted": false, "reason": "Benutzer aktiv"}`: abgelehnt. Der
  Proxy fragt nach `SLEEP_RETRY_SECS` erneut.

## `POST /gaming` (für die Weboberfläche)

```json
{ "enabled": true }
```

An: Strata beenden, Schlafen sperren. Aus: Strata starten, Schlafen erlauben.
