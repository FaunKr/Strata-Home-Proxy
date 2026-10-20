# Typen der Weboberfläche spiegeln die Proxy-Typen

`strata-admin-ui` hat eigene Typen für alles, was eine Seite zeigt
(`SettingEntry`, `ApiKeyInfo`, `StrataInfo` …), auch wo der Proxy fast
dieselbe Struktur hat (`settings::Entry`, `api_keys::KeyInfo`). Das
UI-Crate wird auch für den Browser (wasm) gebaut und darf deshalb nicht vom
Proxy abhängen; die Umwandlung steht in `nas-proxy/src/admin.rs`.

Die doppelte Struktur ist gewollt und kein Review-Befund. Felder, die der
Browser nie sehen darf (Keys, Hashes), fehlen in den UI-Typen.

Ausnahme ist die Übersicht: `nas-proxy/src/overview.rs` baut `Overview`,
`HardwareView`, `PowerView` usw. direkt als Typen aus `strata_admin_ui::api`.
Diese Werte entstehen nur für die Seite; im Proxy gibt es keinen eigenen Typ,
den sie spiegeln könnten. Neue Teile der Übersicht folgen diesem Muster, ohne
Umwandlung in `admin.rs`.

Zweite Ausnahme ist, was der PC-Dienst meldet: Strata, seine Vorgänge und
die Hardwarewerte. Dafür nimmt die Weboberfläche die Typen aus
`strata-pc-protocol` (`StrataInfo`, `StrataJob`, `Hardware` …), die auch der
PC-Dienst schreibt und der Proxy liest; `strata_admin_ui::api` reicht sie
weiter. Das Crate hängt nur von serde ab und baut auch für wasm. Diese Werte
enthalten nichts, was der Browser nicht sehen darf (nie den Strata-Key). Was
der Proxy dazugibt, steht in eigenen Typen wie `StrataPage`; einzig die
Ortszeit eines Benchmarks (`StrataBenchmark::at_text`) trägt er im
Protokolltyp ein, der PC-Dienst schickt sie nie.
