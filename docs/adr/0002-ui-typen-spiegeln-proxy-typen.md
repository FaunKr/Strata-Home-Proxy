# Typen der Weboberfläche spiegeln die Proxy-Typen

`strata-admin-ui` hat eigene Typen für alles, was eine Seite zeigt
(`SettingEntry`, `ApiKeyInfo`, `StrataInfo` …), auch wo der Proxy fast
dieselbe Struktur hat (`settings::Entry`, `api_keys::KeyInfo`). Das
UI-Crate wird auch für den Browser (wasm) gebaut und darf deshalb nicht vom
Proxy abhängen; die Umwandlung steht in `nas-proxy/src/admin.rs`.

Die doppelte Struktur ist gewollt und kein Review-Befund. Felder, die der
Browser nie sehen darf (Keys, Hashes), fehlen in den UI-Typen.
