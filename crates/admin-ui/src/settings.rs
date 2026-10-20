//! Settings as sections in their areas, and the area "System". A section's
//! fields are edited in place and saved together; a value can be reset on
//! its own.

use leptos::{html::Details, prelude::*};

use crate::{
    api::{
        ResetSetting, SaveSettings, SettingEntry, SettingValue, Source, error_text, get_settings,
        setting_error,
    },
    labels::{ENERGY, SYSTEM, SectionDef, label, unit},
};

#[component]
pub fn SystemPage() -> impl IntoView {
    view! {
        <div class="page-head">
            <h1>"System"</h1>
            <p class="lead">"Technical settings of the proxy itself. Rarely needs changing."</p>
        </div>
        <ProxyFacts />
        <SettingsSections sections=&[&SYSTEM, &ENERGY] />
    }
}

/// The proxy's version and data directory, with a warning while `DATA_DIR`
/// is missing.
#[component]
fn ProxyFacts() -> impl IntoView {
    let settings = OnceResource::new(get_settings());
    view! {
        <Transition>
            {move || Suspend::new(async move {
                settings
                    .await
                    .ok()
                    .map(|view| {
                        let note = (!view.persistent).then(|| view! { <DataDirNote /> });
                        let data_dir = view.data_dir.unwrap_or_else(|| "Not set".to_string());
                        view! {
                            {note}
                            <section class="card">
                                <h2>"Proxy"</h2>
                                <dl class="facts">
                                    <dt>"Version"</dt>
                                    <dd>{format!("Version {}", view.version)}</dd>
                                    <dt>"Data directory"</dt>
                                    <dd class="mono">{data_dir}</dd>
                                </dl>
                            </section>
                        }
                    })
            })}
        </Transition>
    }
}

/// What a section keeps while it is drawn again after saving.
#[derive(Clone, Copy)]
struct SectionUi {
    /// Saved just now; shown until the next change.
    saved: RwSignal<bool>,
    advanced_open: RwSignal<bool>,
}

/// The given sections of the settings. Each loads on its own, so saving one
/// leaves unsaved changes in another alone.
#[component]
pub fn SettingsSections(sections: &'static [&'static SectionDef]) -> impl IntoView {
    sections
        .iter()
        .map(|def| view! { <LoadedSection def /> })
        .collect_view()
}

#[component]
fn LoadedSection(def: &'static SectionDef) -> impl IntoView {
    let refresh = RwSignal::new(0u32);
    let settings = Resource::new(move || refresh.get(), |_| get_settings());
    let ui = SectionUi {
        saved: RwSignal::new(false),
        advanced_open: RwSignal::new(false),
    };
    view! {
        <Transition fallback=|| view! { <p class="hint">"Loading …"</p> }>
            {move || Suspend::new(async move {
                match settings.await {
                    Ok(view) => view! { <Section def entries=view.entries ui refresh /> }.into_any(),
                    Err(e) => view! { <p class="error">{error_text(&e)}</p> }.into_any(),
                }
            })}
        </Transition>
    }
}

#[component]
fn DataDirNote() -> impl IntoView {
    view! {
        <section class="card warning" role="status">
            <h2>"Changes are lost on restart"</h2>
            <p>
                "DATA_DIR is not set, so settings and API keys only live in memory. Set DATA_DIR in the proxy's environment (for example in docker-compose.yml) and restart the proxy."
            </p>
        </section>
    }
}

/// One value in a section: what the page got and what the field holds now.
#[derive(Clone)]
struct Row {
    entry: SettingEntry,
    /// What the field started with.
    initial: String,
    draft: RwSignal<String>,
}

impl Row {
    fn new(entry: SettingEntry) -> Self {
        // A secret is never sent back, so its field starts empty. A reported
        // MAC field holds only the fallback entered here.
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
        Self {
            draft: RwSignal::new(initial.clone()),
            initial,
            entry,
        }
    }

    /// The field differs from what is in effect; an empty secret stays.
    fn changed(&self) -> bool {
        self.draft.with(|d| {
            if self.entry.secret {
                !d.trim().is_empty()
            } else {
                d.trim() != self.initial.trim()
            }
        })
    }
}

#[component]
fn Section(
    def: &'static SectionDef,
    entries: Vec<SettingEntry>,
    ui: SectionUi,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let row = |key: &&str| {
        entries
            .iter()
            .find(|e| e.key == *key)
            .cloned()
            .map(Row::new)
    };
    let main: Vec<Row> = def.keys.iter().filter_map(row).collect();
    let advanced: Vec<Row> = def.advanced.iter().filter_map(row).collect();
    let all = StoredValue::new([main.clone(), advanced.clone()].concat());
    let dirty = Memo::new(move |_| all.with_value(|rows| rows.iter().any(Row::changed)));
    let save = ServerAction::<SaveSettings>::new();
    let reset = ServerAction::<ResetSetting>::new();
    Effect::new(move |_| {
        if dirty.get() {
            ui.saved.set(false);
        }
    });
    Effect::new(move |_| {
        if matches!(save.value().get(), Some(Ok(()))) {
            ui.saved.set(true);
            refresh.update(|n| *n += 1);
        }
    });
    Effect::new(move |_| {
        if matches!(reset.value().get(), Some(Ok(()))) {
            refresh.update(|n| *n += 1);
        }
    });
    // The last error, as the key it names and its message.
    let error = Memo::new(move |_| {
        let save_error = save.value().get().and_then(Result::err);
        let reset_error = reset.value().get().and_then(Result::err);
        save_error.or(reset_error).map(|e| {
            let text = error_text(&e);
            let (key, message) = setting_error(&text);
            (key.map(str::to_string), message.to_string())
        })
    });
    let keys: Vec<&str> = def.keys.iter().chain(def.advanced).copied().collect();
    let section_error = move || {
        error.get().and_then(|(key, message)| {
            let shown_at_row = key.as_deref().is_some_and(|k| keys.contains(&k));
            (!shown_at_row).then(|| view! { <p class="error" role="alert">{message}</p> })
        })
    };
    let on_save = move |_| {
        let values = all.with_value(|rows| {
            rows.iter()
                .filter(|r| r.changed())
                .map(|r| SettingValue {
                    key: r.entry.key.clone(),
                    value: r.draft.get_untracked(),
                })
                .collect()
        });
        ui.saved.set(false);
        save.dispatch(SaveSettings { values });
    };
    let on_discard = move |_| {
        all.with_value(|rows| {
            for r in rows {
                r.draft.set(r.initial.clone());
            }
        });
    };
    let pending = move || save.pending().get();
    let advanced_part = (!advanced.is_empty()).then(|| {
        let changed = advanced
            .iter()
            .filter(|r| r.entry.source != Source::Default)
            .count();
        let summary = match changed {
            0 => format!("{} values, all default", advanced.len()),
            n => format!(
                "{} values, {n} set here or in the environment",
                advanced.len()
            ),
        };
        let details = NodeRef::<Details>::new();
        let rows = advanced
            .into_iter()
            .map(|r| view! { <SettingRow row=r reset error /> })
            .collect_view();
        view! {
            <details
                class="advanced"
                node_ref=details
                open=move || ui.advanced_open.get()
                on:toggle=move |_| {
                    if let Some(d) = details.get() {
                        ui.advanced_open.set(d.open());
                    }
                }
            >
                <summary>
                    <crate::icons::Icon icon=crate::icons::Icon::ChevronRight size=16 />
                    "Advanced"
                    <span class="muted small">{summary}</span>
                </summary>
                <div class="rows">{rows}</div>
            </details>
        }
    });
    let heading = format!("{}-heading", def.id);
    let labelled_by = heading.clone();
    view! {
        <section class="card settings" id=def.id aria-labelledby=labelled_by>
            <h2 id=heading>{def.title}</h2>
            <p class="hint">{def.help}</p>
            <div class="rows">
                {main
                    .into_iter()
                    .map(|r| view! { <SettingRow row=r reset error /> })
                    .collect_view()}
            </div>
            {advanced_part}
            {section_error}
            <div class="savebar">
                {move || {
                    dirty
                        .get()
                        .then(|| {
                            view! {
                                <span class="unsaved" role="status">
                                    <span class="dot"></span>
                                    "Unsaved changes"
                                </span>
                            }
                        })
                }}
                {move || {
                    (ui.saved.get() && !dirty.get())
                        .then(|| view! { <span class="saved" role="status">"Saved"</span> })
                }}
                <span class="spacer"></span>
                <button
                    type="button"
                    class="secondary"
                    disabled=move || !dirty.get() || pending()
                    on:click=on_discard
                >
                    "Discard"
                </button>
                <button type="button" disabled=move || !dirty.get() || pending() on:click=on_save>
                    {move || if pending() { "Saving …" } else { "Save" }}
                </button>
            </div>
        </section>
    }
}

fn source_text(source: Source) -> (&'static str, &'static str) {
    match source {
        Source::Reported => ("Reported by PC", "src s-reported"),
        Source::Saved => ("Saved here", "src s-saved"),
        Source::Env => ("Environment", "src s-env"),
        Source::Default => ("Default", "src s-default"),
    }
}

#[component]
fn SettingRow(
    row: Row,
    reset: ServerAction<ResetSetting>,
    error: Memo<Option<(Option<String>, String)>>,
) -> impl IntoView {
    let Row {
        entry,
        draft,
        initial,
    } = row;
    let text = label(&entry.key);
    let id = format!("setting-{}", entry.key);
    let unit = unit(&entry.key);
    let placeholder = if entry.secret {
        if entry.value.is_some() {
            "Set · leave empty to keep"
        } else {
            "Not set"
        }
        .to_string()
    } else if entry.source == Source::Reported {
        "Only used if the PC reports none".to_string()
    } else if let Some(s) = &entry.suggestion {
        s.secs.to_string()
    } else if entry.optional {
        "Not set".to_string()
    } else {
        String::new()
    };
    let suggestion = entry.suggestion.as_ref().map(|s| {
        // Suggestions are whole minutes.
        let text = format!(
            "Suggested: {} s ({} min), from {}.",
            s.secs,
            s.secs / 60,
            s.from
        );
        view! { <p class="small note">{text}</p> }
    });
    let reported = (entry.source == Source::Reported).then(|| {
        let mac = entry.value.clone().unwrap_or_default();
        view! {
            <p class="small note">
                {format!("The PC service reports {mac}. This field is only a fallback.")}
            </p>
        }
    });
    let restart = entry
        .restart
        .then(|| view! { <p class="small note">"Takes effect after a restart."</p> });
    let (source, source_class) = source_text(entry.source);
    let can_reset = entry.source == Source::Saved || entry.saved.is_some();
    let key = entry.key.clone();
    let reset_button = can_reset.then(|| {
        let key = key.clone();
        view! {
            <button
                type="button"
                class="link small"
                aria-label=format!("Reset {}", text.title)
                disabled=move || reset.pending().get()
                on:click=move |_| {
                    reset.dispatch(ResetSetting { key: key.clone() });
                }
            >
                "Reset"
            </button>
        }
    });
    let row_error = {
        let key = key.clone();
        move || {
            error.get().and_then(|(k, message)| {
                (k.as_deref() == Some(key.as_str()))
                    .then(|| view! { <p class="error" role="alert">{message}</p> })
            })
        }
    };
    view! {
        <div class="row">
            <div class="row-label">
                <label for=id.clone()>{text.title}</label>
                <p class="hint">{text.help}</p>
                {reported}
                {restart}
            </div>
            <div>
                <div class=if unit.is_some() { "input-wrap has-unit" } else { "input-wrap" }>
                    <input
                        id=id
                        type=if entry.secret { "password" } else { "text" }
                        value=initial
                        prop:value=move || draft.get()
                        on:input=move |ev| draft.set(event_target_value(&ev))
                        placeholder=placeholder
                        autocomplete="off"
                    />
                    {unit.map(|u| view! { <span class="unit">{u}</span> })}
                </div>
                <div class="row-meta">
                    <span class=source_class>{source}</span>
                    {reset_button}
                    <span class="hint mono small">{key}</span>
                </div>
                {suggestion}
                {row_error}
            </div>
        </div>
    }
}
