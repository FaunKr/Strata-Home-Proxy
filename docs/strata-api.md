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

## Setup und Dateien (für die Strata-Vorgänge des PC-Dienstes)

Quellen: `setup.py`, `START-HERE.bat`, `UPDATE.bat`, `BENCH.bat`, `tools/strata_bench.py`.

- **Modell-ID** = Präfix der Familie + Größe: `qwen` ohne Präfix (`IQ3_XXS`), sonst `swift-`, `coder-`, `unsloth-`, `qwen36-`. Die Tabellen dazu spiegelt `crates/pc-agent/src/catalog.rs`.
- **Setup ohne Fragen:** `START-HERE.bat` startet `.venv\Scripts\python.exe setup.py %*`. Welche Argumente der Dienst übergibt, steht in `strata.rs` (`setup`). `--setup` braucht es, sobald ein Modell installiert ist.
- **Experimentelle Speed-Projection:** `--experimental-speed-projection on|off`, nur für `qwen` und `coder`. Trotz des Namens keine Beschleunigung: ein Steuervektor, der laut Stratas `docs/DETAILS.md` die Ablehnungsrichtung des Modells entfernt. Ohne das Argument übernimmt Setup den Wert einer früheren Installation, der Dienst übergibt es deshalb immer. Clients schalten sie pro Anfrage mit `"experimental_speed_projection": false` ab.
- **Ausgabe von Setup:** Schritte als `=== Step N: Titel ===`, Downloads als `datei: x / y GB (p%)`, Fehler beginnen mit `[X]`.
- **Ablage eines Modells:**
  - `strata-<id klein>.json` (Konfiguration), `run-<id klein>.bat` (Start) und `strata-<id klein>.log` im Strata-Ordner
  - `<data>/models/<ID>` (GGUF-Dateien, auch `.part`) und `<data>/packs/<id klein>` (Tokenizer)
  - `<data>/mtp` teilen sich alle Modelle.
- **Modell-Konfiguration:** ein `strata-*.json` mit `exe` und einem Array `args` (so erkennt `find_configs` in `strata_bench.py` sie). `strata-*.shared-settings.json` ist keine. Strata schreibt sie mit `json.dumps(cfg, indent=1)`. Der Key steht in `api_key`.
- **Aktives Modell:** `run-<id>.bat` startet `serve/server.py`, das `strata.exe` startet und neu startet, wenn es stirbt; beenden also über den Ordner. Ohne eigenes Startskript startet `START-HERE.bat` die Konfiguration, die zuletzt benutzt wurde (die jüngste Änderungszeit, denn ein Start fasst sie an).
- **Empfehlung von Setup:** ab 60 GB RAM `IQ3_XXS`, ab 44 GB `Q2_0`, ab 28 GB `coder-IQ1_M`. Kontext 64K bei 14–20 GB VRAM.
- **Update** wie `UPDATE.bat`: `git pull --ff-only`, dann `START-HERE.bat --update` bei beendetem Strata (die Engine wird sonst nicht ersetzt). Modelldateien bleiben.
- **Benchmark:** `BENCH.bat --config <strata-<id>.json>` nutzt ein laufendes Strata, sonst startet es selbst eines. Ergebnis ist die Zeile `  chat decode tok/s   38.50   (37.10 - 39.20, n=3)` der Tabelle am Ende. Danach folgen weitere Zeilen.
