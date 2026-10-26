//! Web UI of the NAS proxy, under `/admin/` on the API port. Rendered on the
//! server and made interactive in the browser (Leptos, hydration).

// Leptos views are deeply nested types. The release wasm build computes their
// layout and, over the default limit of 128, fails with "queries overflow the
// depth limit". check.sh and ui-check.sh build debug, where it does not show:
// only the image build (Dockerfile) does.
#![recursion_limit = "256"]

pub mod access;
pub mod api;
pub mod app;
pub mod confirm;
pub mod copy;
pub mod hardware;
pub mod history;
pub mod icons;
pub mod labels;
pub mod overview;
pub mod pc_setup;
pub mod request_stats;
pub mod requests;
#[cfg(feature = "ssr")]
pub mod server;
pub mod settings;
pub mod strata;
pub mod strata_output;
#[cfg(feature = "hydrate")]
mod stream;

pub use app::{App, shell};

/// Browser entry point, called by the generated JavaScript.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(App);
}
