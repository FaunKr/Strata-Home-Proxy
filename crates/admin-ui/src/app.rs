//! Pages and layout.

use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Route, Router, Routes},
    path,
};

use crate::{
    api::{
        ApiKeyInfo, ChangePassword, CreateApiKey, CreatedKey, Login, Logout, ResetSetting,
        RevokeApiKey, SaveSetting, Session, SettingEntry, Source, error_text, get_api_keys,
        get_session, get_settings,
    },
    labels::label,
    overview::OverviewPage,
    strata::StrataPage,
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
                    <Route path=path!("/admin") view=|| view! { <Protected><OverviewPage /></Protected> } />
                    <Route path=path!("/admin/") view=|| view! { <Protected><OverviewPage /></Protected> } />
                    <Route
                        path=path!("/admin/strata")
                        view=|| view! { <Protected><StrataPage /></Protected> }
                    />
                    <Route
                        path=path!("/admin/settings")
                        view=|| view! { <Protected><SettingsPage /></Protected> }
                    />
                    <Route
                        path=path!("/admin/api-keys")
                        view=|| view! { <Protected><ApiKeysPage /></Protected> }
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
            <A href="/admin/strata">"Strata"</A>
            <A href="/admin/settings">"Einstellungen"</A>
            <A href="/admin/api-keys">"API-Keys"</A>
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
    let refresh = SettingsRefresh(RwSignal::new(0));
    provide_context(refresh);
    let settings = Resource::new(move || refresh.0.get(), |_| get_settings());
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

/// Bumped after a value is saved or reset, so the page loads them again.
#[derive(Clone, Copy)]
struct SettingsRefresh(RwSignal<u32>);

#[component]
fn SettingRow(entry: SettingEntry) -> impl IntoView {
    let text = label(&entry.key);
    let shown = entry
        .value
        .clone()
        .unwrap_or_else(|| "nicht gesetzt".to_string());
    let source = source_text(&entry);
    let save = ServerAction::<SaveSetting>::new();
    let reset = ServerAction::<ResetSetting>::new();
    let refresh = expect_context::<SettingsRefresh>().0;
    Effect::new(move |_| {
        let saved = matches!(save.value().get(), Some(Ok(())));
        let was_reset = matches!(reset.value().get(), Some(Ok(())));
        if saved || was_reset {
            refresh.update(|n| *n += 1);
        }
    });

    let key = entry.key.clone();
    let input_id = format!("value-{key}");
    // What the field starts with: the saved value, else the one in effect.
    // A reported MAC is not prefilled, since the field holds only the fallback.
    let initial = if entry.secret {
        String::new()
    } else if entry.source == Source::Reported {
        entry.saved.clone().unwrap_or_default()
    } else {
        entry
            .saved
            .clone()
            .or(entry.value.clone())
            .unwrap_or_default()
    };
    let placeholder = if entry.secret {
        "Neuer Wert (leer = entfernen)"
    } else if entry.optional {
        "leer lassen = nicht gesetzt"
    } else {
        ""
    };
    let fallback = (entry.source == Source::Reported)
        .then(|| entry.saved.clone())
        .flatten()
        .map(|saved| view! { <p class="hint">"Eingetragener Ersatzwert: " {saved}</p> });
    let mac_note = (entry.key == "PC_MAC").then(|| {
        view! {
            <p class="hint">
                "Ein eingetragener Wert wirkt nur, solange der PC-Dienst keine eigene MAC meldet."
            </p>
        }
    });
    let can_reset = entry.source == Source::Saved || entry.saved.is_some();
    let (save_key, reset_key, section_id) = (key.clone(), key.clone(), key.clone());
    view! {
        <section class="panel setting" id=section_id>
            <h2>{text.title}</h2>
            <p class="hint">{text.help}</p>
            <p class="current">
                <code>{shown}</code>
                " "
                <span class="source">{source}</span>
            </p>
            {fallback}
            {entry.restart.then(|| view! { <p class="hint">"Gilt nach Neustart."</p> })}
            <ActionForm action=save attr:class="edit">
                <input type="hidden" name="key" value=save_key />
                <label for=input_id.clone() class="visually-hidden">
                    {format!("Neuer Wert für {}", text.title)}
                </label>
                <input
                    id=input_id
                    type=if entry.secret { "password" } else { "text" }
                    name="value"
                    value=initial
                    placeholder=placeholder
                    autocomplete="off"
                />
                <button type="submit">"Speichern"</button>
            </ActionForm>
            {mac_note}
            {can_reset
                .then(|| {
                    view! {
                        <ActionForm action=reset attr:class="reset">
                            <input type="hidden" name="key" value=reset_key />
                            <button type="submit" class="secondary">
                                "Zurücksetzen"
                            </button>
                            <span class="hint">" auf Umgebungsvariable oder Vorgabe"</span>
                        </ActionForm>
                    }
                })}
            {action_error(save.value())}
            {action_error(reset.value())}
            <p class="hint key">{key}</p>
        </section>
    }
}

#[component]
fn ApiKeysPage() -> impl IntoView {
    let create = ServerAction::<CreateApiKey>::new();
    let revoke = ServerAction::<RevokeApiKey>::new();
    provide_context(revoke);
    let keys = Resource::new(
        move || (create.version().get(), revoke.version().get()),
        |_| get_api_keys(),
    );
    // The new key stays visible until the next revoke, so it does not linger
    // for a key that no longer exists.
    let shown = RwSignal::<Option<CreatedKey>>::new(None);
    // A failed create hides the previous key, so it does not sit next to the error.
    Effect::new(move |_| match create.value().get() {
        Some(Ok(c)) => shown.set(Some(c)),
        Some(Err(_)) => shown.set(None),
        None => {}
    });
    Effect::new(move |_| {
        if revoke.version().get() > 0 {
            shown.set(None);
        }
    });
    let created = move || {
        shown.get().map(|c| {
            view! {
                <section class="panel created" role="status">
                    <h2>{format!("Neuer Key für „{}“", c.info.name)}</h2>
                    <label for="new-key">"Key"</label>
                    <input id="new-key" type="text" readonly value=c.key />
                    <p class="hint">
                        "Jetzt kopieren und im Client eintragen. Der Key wird nicht wieder angezeigt."
                    </p>
                </section>
            }
        })
    };
    let create_error = move || {
        create
            .value()
            .get()
            .and_then(|r| r.err())
            .map(|e| view! { <p class="error" role="alert">{error_text(&e)}</p> })
    };
    view! {
        <h1>"API-Keys"</h1>
        <p class="hint">
            "Jeder Client (Claude Code, Open WebUI, …) bekommt einen eigenen Key und schickt ihn als „Authorization: Bearer …“ oder „x-api-key“. Ohne gültigen Key antwortet der Proxy mit 401 und weckt den PC nicht."
        </p>
        {created}
        <section class="panel">
            <h2>"Neuer Key"</h2>
            <ActionForm action=create attr:class="edit">
                <label for="key-name" class="visually-hidden">"Name des Clients"</label>
                <input
                    id="key-name"
                    type="text"
                    name="name"
                    placeholder="z. B. Claude Code Laptop"
                    maxlength="64"
                    autocomplete="off"
                    required
                />
                <button type="submit">"Anlegen"</button>
            </ActionForm>
            {create_error}
        </section>
        {action_error(revoke.value())}
        <Transition fallback=|| view! { <p>"Lädt …"</p> }>
            {move || Suspend::new(async move {
                match keys.await {
                    Ok(view) => {
                        let empty = view.keys.is_empty();
                        view! {
                            {(!view.persistent)
                                .then(|| {
                                    view! {
                                        <p class="warning" role="status">
                                            "DATA_DIR ist nicht gesetzt: Keys gehen beim nächsten Neustart verloren."
                                        </p>
                                    }
                                })}
                            {empty
                                .then(|| {
                                    view! {
                                        <p class="warning" role="status">
                                            "Noch kein Key angelegt: bis dahin bekommt jeder Client 401."
                                        </p>
                                    }
                                })}
                            {view
                                .keys
                                .into_iter()
                                .map(|key| view! { <ApiKeyRow key /> })
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
fn ApiKeyRow(key: ApiKeyInfo) -> impl IntoView {
    let revoke = expect_context::<ServerAction<RevokeApiKey>>();
    let used = key.last_used_text.as_deref().map_or_else(
        || "noch nie benutzt".to_string(),
        |t| format!("zuletzt benutzt {t}"),
    );
    let confirm_id = format!("confirm-{}", key.prefix);
    view! {
        <section class="panel api-key">
            <h2>{key.name.clone()}</h2>
            <p class="current">
                <code>{format!("{}…", key.prefix)}</code>
            </p>
            <p class="hint">{format!("Angelegt {}, {used}", key.created_text)}</p>
            <ActionForm action=revoke attr:class="revoke">
                <input type="hidden" name="name" value=key.name />
                <input type="hidden" name="prefix" value=key.prefix.clone() />
                <input id=confirm_id.clone() type="checkbox" required />
                <label for=confirm_id class="inline">
                    "Ja, widerrufen: dieser Client bekommt danach 401"
                </label>
                <button type="submit" class="secondary">"Widerrufen"</button>
            </ActionForm>
        </section>
    }
}
