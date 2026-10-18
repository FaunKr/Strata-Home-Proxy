//! The area "Access": the clients' API keys, the admin password and signing
//! out.

use leptos::prelude::*;

use crate::{
    api::{ApiKeyInfo, CreateApiKey, CreatedKey, RevokeApiKey, error_text, get_api_keys},
    app::{PasswordFields, SessionActions, action_error},
};

#[component]
pub fn AccessPage() -> impl IntoView {
    view! {
        <div class="page-head">
            <h1>"Access"</h1>
            <p class="lead">"Who may use the proxy, and your admin password."</p>
        </div>
        <ApiKeys />
        <Password />
        <SignOut />
    }
}

#[component]
fn ApiKeys() -> impl IntoView {
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
                <section class="card created" role="status">
                    <h2>{format!("New key for “{}”", c.info.name)}</h2>
                    <label for="new-key">"Key"</label>
                    <input id="new-key" type="text" class="mono" readonly value=c.key />
                    <p class="hint">
                        "Copy it now and enter it in the client. It is not shown again."
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
        {created}
        <section class="card">
            <h2>"API keys"</h2>
            <p class="hint">
                "Each client (Claude Code, Open WebUI, …) gets its own key and sends it as “Authorization: Bearer …” or “x-api-key”. Without a valid key the proxy answers 401 and does not wake the PC."
            </p>
            <ActionForm action=create attr:class="form-row">
                <label for="key-name" class="visually-hidden">
                    "Client name"
                </label>
                <input
                    id="key-name"
                    type="text"
                    name="name"
                    placeholder="Client name, e.g. Claude Code laptop"
                    maxlength="64"
                    autocomplete="off"
                    required
                />
                <button type="submit">"Create key"</button>
            </ActionForm>
            {create_error}
            {action_error(revoke.value())}
            <Transition fallback=|| view! { <p>"Loading …"</p> }>
                {move || Suspend::new(async move {
                    match keys.await {
                        Ok(view) => {
                            let empty = view.keys.is_empty();
                            view! {
                                {(!view.persistent)
                                    .then(|| {
                                        view! {
                                            <p class="warning" role="status">
                                                "DATA_DIR is not set: keys are lost on the next restart."
                                            </p>
                                        }
                                    })}
                                {empty
                                    .then(|| {
                                        view! {
                                            <p class="warning" role="status">
                                                "No key yet. Until you create one, every client gets 401."
                                            </p>
                                        }
                                    })}
                                <ul class="list">
                                    {view
                                        .keys
                                        .into_iter()
                                        .map(|key| view! { <ApiKeyRow key /> })
                                        .collect_view()}
                                </ul>
                            }
                                .into_any()
                        }
                        Err(e) => view! { <p class="error">{error_text(&e)}</p> }.into_any(),
                    }
                })}
            </Transition>
        </section>
    }
}

#[component]
fn ApiKeyRow(key: ApiKeyInfo) -> impl IntoView {
    let revoke = expect_context::<ServerAction<RevokeApiKey>>();
    let used = key
        .last_used_text
        .as_deref()
        .map_or_else(|| "never used".to_string(), |t| format!("last used {t}"));
    let confirm_id = format!("confirm-{}", key.prefix);
    view! {
        <li class="api-key">
            <div class="grow">
                <p class="strong">{key.name.clone()}</p>
                <p class="hint">
                    <span class="mono">{format!("{}…", key.prefix)}</span>
                    {format!(" · created {} · {used}", key.created_text)}
                </p>
            </div>
            <ActionForm action=revoke attr:class="actions inline">
                <input type="hidden" name="name" value=key.name />
                <input type="hidden" name="prefix" value=key.prefix.clone() />
                <input id=confirm_id.clone() type="checkbox" required />
                <label for=confirm_id class="inline">
                    "Yes, this client gets 401 afterwards"
                </label>
                <button type="submit" class="ghost-danger">
                    "Revoke"
                </button>
            </ActionForm>
        </li>
    }
}

#[component]
fn Password() -> impl IntoView {
    let change = expect_context::<SessionActions>().change;
    let saved = move || {
        matches!(change.value().get(), Some(Ok(())))
            .then(|| view! { <p class="saved" role="status">"Password changed. Other devices are signed out."</p> })
    };
    view! {
        <section class="card">
            <h2>"Admin password"</h2>
            <ActionForm action=change attr:class="stack narrow">
                <PasswordFields />
                {action_error(change.value())}
                {saved}
                <div>
                    <button type="submit">"Change password"</button>
                </div>
            </ActionForm>
        </section>
    }
}

#[component]
fn SignOut() -> impl IntoView {
    let logout = expect_context::<SessionActions>().logout;
    view! {
        <section class="card">
            <div class="card-head">
                <div>
                    <h2>"Session"</h2>
                    <p class="hint">"Signed in on this device."</p>
                </div>
                <ActionForm action=logout>
                    <button type="submit" class="secondary">
                        "Sign out"
                    </button>
                </ActionForm>
            </div>
        </section>
    }
}
