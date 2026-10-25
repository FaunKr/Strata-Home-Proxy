//! The area "Access": the clients' API keys, the admin password and signing
//! out.

use leptos::{html::Input, prelude::*};

use crate::{
    api::{
        ApiKeyInfo, CreateApiKey, CreatedKey, KeyModelName, RevokeApiKey, SetApiKeyModelName,
        error_text, get_api_keys,
    },
    app::{PasswordFields, SessionActions, action_error},
    confirm::{Confirm, Question},
    copy::CopyField,
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
    let model_name = ServerAction::<SetApiKeyModelName>::new();
    provide_context(revoke);
    provide_context(model_name);
    let keys = Resource::new(
        move || {
            (
                create.version().get(),
                revoke.version().get(),
                model_name.version().get(),
            )
        },
        |_| get_api_keys(),
    );
    // The new key stays visible until the next revoke, so it does not linger
    // for a key that no longer exists.
    let shown = RwSignal::<Option<CreatedKey>>::new(None);
    // A failed create hides the previous key, so it does not sit next to the error.
    let name = NodeRef::<Input>::new();
    Effect::new(move |_| match create.value().get() {
        Some(Ok(c)) => {
            shown.set(Some(c));
            if let Some(input) = name.get_untracked() {
                input.set_value("");
            }
        }
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
                    <CopyField id="new-key" label="Key" text=c.key />
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
            <p class="muted small">
                "Each client (Claude Code, Open WebUI, …) gets its own key and sends it as “Authorization: Bearer …” or “x-api-key”. Without a valid key the proxy answers 401 and does not wake the PC."
            </p>
            <ActionForm action=create attr:class="form-row">
                <label for="key-name" class="visually-hidden">
                    "Client name"
                </label>
                <input
                    id="key-name"
                    node_ref=name
                    type="text"
                    name="name"
                    placeholder="Client name, e.g. Claude Code laptop"
                    maxlength="64"
                    autocomplete="off"
                    required
                />
                <label for="key-model-name" class="visually-hidden">
                    "Model name"
                </label>
                <select id="key-model-name" name="model_name" class="model-name">
                    <ModelNameOptions selected=KeyModelName::Global />
                </select>
                <button type="submit">"Create key"</button>
            </ActionForm>
            {create_error}
            {action_error(revoke.value())}
            {action_error(model_name.value())}
            <Transition fallback=|| view! { <p>"Loading …"</p> }>
                {move || Suspend::new(async move {
                    match keys.await {
                        Ok(view) => {
                            let empty = view.keys.is_empty();
                            view! {
                                {empty
                                    .then(|| {
                                        view! {
                                            <p class="warning" role="status">
                                                "No API keys yet. Until you create one, every client gets 401."
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

/// What a key's apps see as the model's name.
const MODEL_NAMES: [(KeyModelName, &str, &str); 3] = [
    (
        KeyModelName::Global,
        "global",
        "Model name as in Strata → Connection",
    ),
    (KeyModelName::Hide, "hide", "Hide model name"),
    (KeyModelName::Show, "show", "Show model name"),
];

#[component]
fn ModelNameOptions(selected: KeyModelName) -> impl IntoView {
    MODEL_NAMES
        .iter()
        .map(|(choice, value, text)| {
            view! {
                <option value=*value selected=*choice == selected>
                    {*text}
                </option>
            }
        })
        .collect_view()
}

#[component]
fn ApiKeyRow(key: ApiKeyInfo) -> impl IntoView {
    let revoke = expect_context::<ServerAction<RevokeApiKey>>();
    let model_name = expect_context::<ServerAction<SetApiKeyModelName>>();
    let select_id = format!("model-name-{}", key.prefix);
    let on_model_name = {
        let (name, prefix) = (key.name.clone(), key.prefix.clone());
        move |ev| {
            let value = event_target_value(&ev);
            if let Some((choice, ..)) = MODEL_NAMES.iter().find(|(_, v, _)| *v == value) {
                model_name.dispatch(SetApiKeyModelName {
                    name: name.clone(),
                    prefix: prefix.clone(),
                    model_name: *choice,
                });
            }
        }
    };
    let confirm = expect_context::<Confirm>();
    let used = key
        .last_used_text
        .as_deref()
        .map_or_else(|| "never used".to_string(), |t| format!("last used {t}"));
    let ApiKeyInfo { name, prefix, .. } = key.clone();
    let on_revoke = move |_| {
        let (name, prefix) = (name.clone(), prefix.clone());
        confirm.ask(
            Question {
                title: format!("Revoke “{name}”?"),
                text: "Clients using this key get 401 from now on. This cannot be undone.".into(),
                confirm: "Revoke key",
                danger: true,
            },
            move || {
                revoke.dispatch(RevokeApiKey {
                    name: name.clone(),
                    prefix: prefix.clone(),
                });
            },
        );
    };
    view! {
        <li>
            <div class="grow">
                <p class="strong">{key.name.clone()}</p>
                <p class="muted small">
                    <span class="mono">{format!("{}…", key.prefix)}</span>
                    {format!(" · created {} · {used}", key.created_text)}
                </p>
            </div>
            <label for=select_id.clone() class="visually-hidden">
                {format!("Model name for {}", key.name)}
            </label>
            <select
                id=select_id
                class="model-name"
                disabled=move || model_name.pending().get()
                on:change=on_model_name
            >
                <ModelNameOptions selected=key.model_name />
            </select>
            <button
                type="button"
                class="ghost-danger"
                aria-label=format!("Revoke {}", key.name)
                disabled=move || revoke.pending().get()
                on:click=on_revoke
            >
                "Revoke"
            </button>
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
