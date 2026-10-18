//! Pages and layout.

use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    path,
};

use crate::{
    api::{
        ChangePassword, Login, Logout, Session, SettingEntry, Source, error_text, get_session,
        get_settings,
    },
    labels::label,
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

/// Actions that change the session; the session is fetched again after each.
#[derive(Clone, Copy)]
struct SessionActions {
    login: ServerAction<Login>,
    logout: ServerAction<Logout>,
    change: ServerAction<ChangePassword>,
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    let actions = SessionActions {
        login: ServerAction::new(),
        logout: ServerAction::new(),
        change: ServerAction::new(),
    };
    provide_context(actions);
    let session = SessionResource(Resource::new(
        move || {
            (
                actions.login.version().get(),
                actions.logout.version().get(),
                actions.change.version().get(),
            )
        },
        |_| get_session(),
    ));
    provide_context(session);
    let session = session.0;
    view! {
        <Stylesheet id="admin" href="/admin/pkg/admin-ui.css" />
        <Title text="Strata-Proxy" />
        <Router>
            <header class="top">
                <span class="brand">"Strata-Proxy"</span>
                <Transition>
                    {move || Suspend::new(async move {
                        (session.await == Ok(Session::Ready)).then(|| view! { <Nav /> })
                    })}
                </Transition>
            </header>
            <main>
                <Routes fallback=NotFound>
                    <Route path=path!("/admin") view=|| view! { <Protected><Home /></Protected> } />
                    <Route path=path!("/admin/") view=|| view! { <Protected><Home /></Protected> } />
                    <Route
                        path=path!("/admin/settings")
                        view=|| view! { <Protected><SettingsPage /></Protected> }
                    />
                    <Route
                        path=path!("/admin/password")
                        view=|| view! { <Protected><PasswordPage /></Protected> }
                    />
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn Nav() -> impl IntoView {
    let logout = expect_context::<SessionActions>().logout;
    view! {
        <nav>
            <A href="/admin/">"Übersicht"</A>
            <A href="/admin/settings">"Einstellungen"</A>
            <A href="/admin/password">"Passwort"</A>
        </nav>
        <ActionForm action=logout>
            <button type="submit" class="secondary">"Abmelden"</button>
        </ActionForm>
    }
}

/// The current session, fetched again after login, logout and password change.
#[derive(Clone, Copy)]
struct SessionResource(Resource<Result<Session, ServerFnError>>);

/// Shows its page only to a logged-in user with an own password; otherwise
/// the login or the "set a new password" form.
#[component]
fn Protected(children: ChildrenFn) -> impl IntoView {
    let session = expect_context::<SessionResource>().0;
    let children = StoredValue::new(children);
    view! {
        <Transition fallback=|| view! { <p>"Lädt …"</p> }>
            {move || Suspend::new(async move {
                match session.await {
                    Ok(Session::Ready) => children.read_value()().into_any(),
                    Ok(Session::MustChange) => view! { <FirstPassword /> }.into_any(),
                    Ok(Session::Anonymous) => view! { <LoginForm /> }.into_any(),
                    Err(e) => view! { <p class="error">{error_text(&e)}</p> }.into_any(),
                }
            })}
        </Transition>
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

/// Shows the error of an action's last run, if any.
fn action_error(
    value: impl Get<Value = Option<Result<(), ServerFnError>>> + Send + Sync + 'static,
) -> impl IntoView {
    move || {
        value
            .get()
            .and_then(|r| r.err())
            .map(|e| view! { <p class="error" role="alert">{error_text(&e)}</p> })
    }
}

#[component]
fn LoginForm() -> impl IntoView {
    let login = expect_context::<SessionActions>().login;
    view! {
        <h1>"Anmelden"</h1>
        <div class="panel">
            <ActionForm action=login attr:class="stack">
                <div>
                    <label for="password">"Passwort"</label>
                    <input
                        id="password"
                        type="password"
                        name="password"
                        autocomplete="current-password"
                        required
                    />
                </div>
                {action_error(login.value())}
                <button type="submit">"Anmelden"</button>
            </ActionForm>
        </div>
    }
}

#[component]
fn PasswordFields() -> impl IntoView {
    view! {
        <div>
            <label for="current">"Aktuelles Passwort"</label>
            <input id="current" type="password" name="current" autocomplete="current-password" required />
        </div>
        <div>
            <label for="new">"Neues Passwort"</label>
            <input id="new" type="password" name="new" autocomplete="new-password" minlength="8" required />
            <p class="hint">"Mindestens 8 Zeichen."</p>
        </div>
        <div>
            <label for="repeat">"Neues Passwort wiederholen"</label>
            <input id="repeat" type="password" name="repeat" autocomplete="new-password" minlength="8" required />
        </div>
    }
}

/// After the first login with the default password: nothing else works until
/// it is changed.
#[component]
fn FirstPassword() -> impl IntoView {
    let change = expect_context::<SessionActions>().change;
    view! {
        <h1>"Neues Passwort festlegen"</h1>
        <p class="warning">
            "Du bist mit dem Standardpasswort angemeldet. Lege ein eigenes fest, bevor es weitergeht."
        </p>
        <div class="panel">
            <ActionForm action=change attr:class="stack">
                <PasswordFields />
                {action_error(change.value())}
                <button type="submit">"Passwort speichern"</button>
            </ActionForm>
        </div>
    }
}

#[component]
fn PasswordPage() -> impl IntoView {
    let change = expect_context::<SessionActions>().change;
    let saved = move || {
        matches!(change.value().get(), Some(Ok(())))
            .then(|| view! { <p class="hint" role="status">"Passwort geändert. Andere Geräte sind abgemeldet."</p> })
    };
    view! {
        <h1>"Passwort ändern"</h1>
        <div class="panel">
            <ActionForm action=change attr:class="stack">
                <PasswordFields />
                {action_error(change.value())}
                {saved}
                <button type="submit">"Passwort speichern"</button>
            </ActionForm>
        </div>
    }
}

fn source_text(entry: &SettingEntry) -> &'static str {
    match entry.source {
        Source::Reported => "vom PC gemeldet",
        Source::Saved => "auf dieser Seite gespeichert",
        Source::Env => "aus Umgebungsvariable",
        Source::Default => "Vorgabe",
    }
}

#[component]
fn SettingsPage() -> impl IntoView {
    let settings = Resource::new(|| (), |_| get_settings());
    view! {
        <h1>"Einstellungen"</h1>
        <Transition fallback=|| view! { <p>"Lädt …"</p> }>
            {move || Suspend::new(async move {
                match settings.await {
                    Ok(view) => {
                        view! {
                            {(!view.configured)
                                .then(|| {
                                    view! {
                                        <p class="warning" role="status">
                                            "Der Proxy ist noch nicht eingerichtet: die Strata-Adresse fehlt. Bis dahin beantwortet er API-Anfragen mit 503."
                                        </p>
                                    }
                                })}
                            {(!view.persistent)
                                .then(|| {
                                    view! {
                                        <p class="warning" role="status">
                                            "DATA_DIR ist nicht gesetzt: Änderungen gehen beim nächsten Neustart verloren."
                                        </p>
                                    }
                                })}
                            {view
                                .entries
                                .into_iter()
                                .map(|entry| view! { <SettingRow entry /> })
                                .collect_view()}
                        }
                            .into_any()
                    }
                    Err(e) => view! { <p class="error">{error_text(&e)}</p> }.into_any(),
                }
            })}
        </Transition>
    }
}

#[component]
fn SettingRow(entry: SettingEntry) -> impl IntoView {
    let text = label(&entry.key);
    let shown = entry
        .value
        .clone()
        .unwrap_or_else(|| "nicht gesetzt".to_string());
    let source = source_text(&entry);
    let fallback = (entry.source == Source::Reported)
        .then(|| entry.saved.clone())
        .flatten()
        .map(|saved| view! { <p class="hint">"Eingetragener Ersatzwert: " {saved}</p> });
    view! {
        <section class="panel setting" id=entry.key.clone()>
            <h2>{text.title}</h2>
            <p class="hint">{text.help}</p>
            <p class="current">
                <code>{shown}</code>
                " "
                <span class="source">{source}</span>
            </p>
            {fallback}
            {entry.restart.then(|| view! { <p class="hint">"Gilt nach Neustart."</p> })}
            <p class="hint key">{entry.key.clone()}</p>
        </section>
    }
}
