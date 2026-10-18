//! Pages and layout.

use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    path,
};

/// The HTML page around the app.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="de">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <HydrationScripts options />
                <MetaTags />
            </head>
            <body>
                <App />
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    view! {
        <Stylesheet id="admin" href="/admin/pkg/admin-ui.css" />
        <Title text="Strata-Proxy" />
        <Router>
            <header class="top">
                <span class="brand">"Strata-Proxy"</span>
                <nav>
                    <A href="/admin/">"Übersicht"</A>
                </nav>
            </header>
            <main>
                <Routes fallback=NotFound>
                    <Route path=path!("/admin") view=Home />
                    <Route path=path!("/admin/") view=Home />
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn Home() -> impl IntoView {
    view! {
        <h1>"Übersicht"</h1>
        <p>"Hier zeigt der Proxy bald den Zustand von PC und Strata."</p>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <h1>"Nicht gefunden"</h1>
        <p>
            <A href="/admin/">"Zur Übersicht"</A>
        </p>
    }
}
