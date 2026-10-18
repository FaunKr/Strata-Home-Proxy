# Was der Proxy von Strata nutzt

Strata ist eine eigene Engine ([Niko1221/Strata](https://github.com/Niko1221/Strata)), kein llama-server. Einige Endpunkte tragen llama-server- oder vLLM-Namen, damit fremde Werkzeuge funktionieren. Was sie wirklich liefern, steht nur im Code von Strata.

Stand: Commit [`477ac13`](https://github.com/Niko1221/Strata/tree/477ac13). Die Zeilennummern beziehen sich darauf.

## Quellen lesen

Lade eine Datei roh in das Scratchpad und lies sie mit `grep` und `sed`:

```sh
curl -sS -o server.py https://raw.githubusercontent.com/Niko1221/Strata/477ac13/serve/server.py
```

`serve/server.py` hat über 7000 Zeilen. Ein Webabruf mit Zusammenfassung schneidet sie ab und zitiert keine Handler. Die Routen stehen in `do_GET` ab Zeile 5372.

## API-Key

Ist in Strata ein Key gesetzt, verlangen die geschützten Endpunkte `Authorization: Bearer <key>` oder `x-api-key: <key>`. Fehlt er, antworten sie mit 401 (`_authorized`, Zeile 5362). Der Proxy nimmt dafür `STRATA_API_KEY`.

## `GET /health`

Immer 200 und ohne Key (Zeile 5460):

```json
{"status": "ok", "max_context": 32768, "model": "…", "images": false, "api_key": true,
 "image_generation": false, "loaded": true, "service": "strata"}
```

`loaded` ist false, solange das Modell nicht im Speicher ist. Strata lädt es mit der nächsten Anfrage. Antwortet `/health` gar nicht, läuft Strata nicht oder startet noch.

## Metriken: `GET /metrics?format=prometheus`

Diesen Endpunkt nutzt die Übersicht. Er braucht den Key (Zeile 5416) und liefert Prometheus-Text mit vLLM-Namen und `strata:`-Namen (`serve/prometheus.py`, `render`). Jede Zeile trägt das Label `model_name`:

| Name | Bedeutung |
|---|---|
| `vllm:num_requests_running` | Anfragen, die gerade lesen oder erzeugen |
| `vllm:num_requests_waiting` | wartende Anfragen |
| `strata:live_tok_s` | erzeugte Tokens/s der letzten Sekunden, 0 ohne Erzeugung |
| `strata:live_state{state="…"}` | 1 für `unloaded`, `idle`, `reading` oder `generating` |

Ohne `format=prometheus` (und ohne `Accept: text/plain`) liefert `/metrics` JSON für den Monitor-Tab. Die Felder dort sind dieselben, z. B. `live.tok_s`.

Die Tests lesen echte Ausgaben von `render` aus `crates/nas-proxy/tests/fixtures/strata-metrics-*.prom`. Ändert sich Strata, erzeuge sie neu aus dem neuen `serve/prometheus.py` und passe den Commit oben an.

## `GET /metrics/prometheus`: nicht für die Übersicht

Dieser Endpunkt liefert llama-server-Namen (`llamacpp:*`, `Service.prometheus`, Zeile 3792), braucht den Key und eignet sich nicht für die Übersicht. Zwei Gründe:
- `llamacpp:predicted_tokens_seconds` zählt nur fertige Anfragen und steht während einer laufenden Anfrage auf 0.
- Jeder Abruf setzt das Zeitfenster dieser Werte zurück, auch für andere Abfrager wie Grafana.

## Weitere Endpunkte

- **`/slots`:** im Stil von llama-server, mit dem Zustand jedes Platzes.
- **`/status` und `/v1/status`:** was der Server gerade tut.

Der Proxy nutzt beide nicht. Der PC-Dienst fragt `/slots` ab (siehe [pc-agent-api.md](pc-agent-api.md)).
