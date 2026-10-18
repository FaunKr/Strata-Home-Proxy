//! Server side: the axum routes for `/admin/`.

use std::path::PathBuf;

use axum::Router;
use leptos::prelude::*;
use leptos_axum::{LeptosRoutes, generate_route_list};

use crate::app::{App, shell};

/// Where the built browser files live (`LEPTOS_SITE_ROOT`, set in the Docker
/// image); `target/site` when developing with cargo-leptos.
pub fn site_root() -> PathBuf {
    std::env::var("LEPTOS_SITE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("target/site"))
}

pub fn options(site_root: PathBuf) -> LeptosOptions {
    LeptosOptions::builder()
        .output_name("admin-ui")
        .site_root(site_root.to_string_lossy().to_string())
        .site_pkg_dir("admin/pkg")
        .build()
}

/// Routes for the pages, the server functions and the browser files. `context`
/// runs for every page and server function call and provides what they need.
pub fn router<S>(
    options: LeptosOptions,
    context: impl Fn() + Clone + Send + Sync + 'static,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let pkg = PathBuf::from(options.site_root.as_ref()).join(options.site_pkg_dir.as_ref());
    let routes = generate_route_list(App);
    Router::new()
        .leptos_routes_with_context(&options, routes, context, {
            let options = options.clone();
            move || shell(options.clone())
        })
        .nest_service("/admin/pkg", tower_http::services::ServeDir::new(pkg))
        .with_state(options)
}
