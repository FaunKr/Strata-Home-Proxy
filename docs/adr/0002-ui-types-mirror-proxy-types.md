# Web UI types mirror the proxy types

`strata-admin-ui` has its own types for everything a page shows
(`SettingEntry`, `ApiKeyInfo`, `StrataInfo` …), even where the proxy has almost
the same structure (`settings::Entry`, `api_keys::KeyInfo`). The
UI crate is also built for the browser (wasm) and therefore must not depend on the
proxy; the conversion is in `nas-proxy/src/admin.rs`.

The duplicate structure is intended and not a review finding. Fields the
browser must never see (keys, hashes) are missing from the UI types.

The exception is the Overview: `nas-proxy/src/overview.rs` builds `Overview`,
`HardwareView`, `PowerView` etc. directly as types from `strata_admin_ui::api`.
These values exist only for the page; the proxy has no type of its own
they could mirror. New parts of the Overview follow this pattern, without
conversion in `admin.rs`.

The second exception is what the PC service reports: Strata, its jobs and
the hardware readings. For these the web UI takes the types from
`strata-pc-protocol` (`StrataInfo`, `StrataJob`, `Hardware` …), which the
PC service also writes and the proxy reads; `strata_admin_ui::api` passes them
on. The crate depends only on serde and also builds for wasm. These values
contain nothing the browser must not see (never the Strata key). What
the proxy adds is in types of its own like `StrataPage`; only the
local time of a benchmark (`StrataBenchmark::at_text`) is entered by the proxy in the
protocol type, the PC service never sends it.
