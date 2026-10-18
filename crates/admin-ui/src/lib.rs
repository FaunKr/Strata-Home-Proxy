//! Web UI of the NAS proxy, under `/admin/` on the API port. Rendered on the
//! server and made interactive in the browser (Leptos, hydration).

pub mod api;
pub mod app;
pub mod labels;
pub mod overview;
pub mod pc_setup;
#[cfg(feature = "ssr")]
pub mod server;
pub mod strata;

pub use app::{App, shell};

/// Browser entry point, called by the generated JavaScript.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(App);
}
