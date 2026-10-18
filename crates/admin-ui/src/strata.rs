//! The Strata page: Strata on the PC as its service reports it, and the
//! jobs that set it up, load and switch models, update and measure it.
//! A job's progress comes live with the overview stream.

use leptos::prelude::*;

use crate::{
    api::{
        CancelStrataJob, StartStrataJob, StrataInfo, StrataJob, StrataJobKind, StrataJobState,
        StrataModel, StrataPage, StrataSize, error_text, get_strata,
    },
    labels::strata_job_label,
    overview::Live,
};

#[derive(Clone, Copy)]
struct Actions {
    start: ServerAction<StartStrataJob>,
    cancel: ServerAction<CancelStrataJob>,
}

impl Actions {
    fn job(&self, kind: StrataJobKind) {
        self.start.dispatch(StartStrataJob {
            kind,
            family: None,
            size: None,
            context: None,
            model: None,
            speed_projection: None,
        });
    }

    fn model_job(&self, kind: StrataJobKind, model: String) {
        self.start.dispatch(StartStrataJob {
            kind,
            family: None,
            size: None,
            context: None,
            model: Some(model),
            speed_projection: None,
        });
    }
}

#[component]
pub fn StrataPage() -> impl IntoView {
    let live = Live::follow();
    let actions = Actions {
        start: ServerAction::new(),
        cancel: ServerAction::new(),
    };
    // The page is asked for again when a job ends, so it shows the result.
    let job_ended = RwSignal::new(0u32);
    let running = Memo::new(move |_| {
        live.overview
            .with(|o| o.as_ref().is_some_and(|o| o.strata_job.is_some()))
    });
    Effect::new(move |was: Option<bool>| {
        let now = running.get();
        if was == Some(true) && !now {
            job_ended.update(|n| *n += 1);
        }
        now
    });
    let page = Resource::new(
        move || {
            (
                actions.start.version().get(),
                actions.cancel.version().get(),
                job_ended.get(),
            )
        },
        |_| get_strata(),
    );
    view! {
        <h1>"Strata"</h1>
        {action_error(actions.start.value())}
        {action_error(actions.cancel.value())}
        <Transition fallback=|| view! { <p>"Lädt …"</p> }>
            {move || Suspend::new(async move {
                match page.await {
                    Ok(p) => view! { <Body page=p live actions /> }.into_any(),
                    Err(e) => view! { <p class="error">{error_text(&e)}</p> }.into_any(),
                }
            })}
        </Transition>
    }
}

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
fn Body(page: StrataPage, live: Live, actions: Actions) -> impl IntoView {
    // Until the stream answers, the job the page came with.
    let first_job = page
        .info
        .as_ref()
        .and_then(|i| i.job.clone())
        .filter(|j| j.state == StrataJobState::Running);
    let job = Memo::new(move |_| match live.overview.get() {
        Some(o) => o.strata_job,
        None => first_job.clone(),
    });
    let gaming = page.gaming;
    // Jobs wait while one runs; gaming mode refuses them.
    let blocked = Signal::derive(move || {
        gaming || job.with(Option::is_some) || actions.start.pending().get()
    });
    let problem = page
        .problem
        .clone()
        .map(|p| view! { <p class="warning" role="status">{p}</p> });
    let stale = page.stale.then(|| {
        view! { <p class="hint">"Stand vom letzten Mal, als der PC-Dienst geantwortet hat."</p> }
    });
    let gaming_note = gaming.then(|| {
        view! { <p class="warning" role="status">"Gaming-Modus ist an: bis er aus ist, startet kein Vorgang."</p> }
    });
    let content = match page.info {
        None => None,
        Some(info) if !info.installed => {
            Some(view! { <Install info actions blocked /> }.into_any())
        }
        Some(info) => Some(view! { <Installed info actions blocked /> }.into_any()),
    };
    view! {
        {problem}
        {stale}
        {gaming_note}
        {move || job.get().map(|j| view! { <JobPanel job=j cancel=actions.cancel /> })}
        {content}
    }
}

/// `81,0 GB`.
fn gb(value: f64) -> String {
    format!("{value:.1} GB").replace('.', ",")
}

fn bytes_gb(bytes: u64) -> String {
    gb(bytes as f64 / 1e9)
}

/// `1:05 h` or `4 min`.
fn eta_text(secs: u64) -> String {
    let mins = secs.div_ceil(60);
    if mins >= 60 {
        format!("{}:{:02} h", mins / 60, mins % 60)
    } else {
        format!("{mins} min")
    }
}

#[component]
fn JobPanel(job: StrataJob, cancel: ServerAction<CancelStrataJob>) -> impl IntoView {
    let title = match &job.model {
        Some(m) => format!("{} {m}", strata_job_label(job.kind)),
        None => strata_job_label(job.kind).to_string(),
    };
    let mut figures = Vec::new();
    if let (Some(done), Some(total)) = (job.done_bytes, job.total_bytes) {
        figures.push(format!("{} von {}", bytes_gb(done), bytes_gb(total)));
    }
    if let Some(speed) = job.bytes_per_sec {
        figures.push(format!("{:.1} MB/s", speed as f64 / 1e6).replace('.', ","));
    }
    if let Some(eta) = job.eta_secs {
        figures.push(format!("noch etwa {}", eta_text(eta)));
    }
    let lines = job.lines.join("\n");
    view! {
        <section class="panel job" role="status">
            <h2>{format!("Läuft: {title}")}</h2>
            {job.step.map(|s| view! { <p>{format!("Schritt {s}")}</p> })}
            {job
                .percent
                .map(|p| {
                    view! {
                        <progress max="100" value=p.to_string()>
                            {format!("{p} %")}
                        </progress>
                        <p class="hint">{format!("{p} %")}</p>
                    }
                })}
            {(!figures.is_empty()).then(|| view! { <p class="hint">{figures.join(" · ")}</p> })}
            {(!lines.is_empty()).then(|| view! { <pre class="lines">{lines}</pre> })}
            <p class="hint">"Der PC bleibt wach, bis der Vorgang fertig ist."</p>
            <div class="actions">
                <button
                    class="secondary"
                    disabled=move || cancel.pending().get()
                    on:click=move |_| {
                        if confirmed("Vorgang abbrechen? Ein Download macht später an derselben Stelle weiter.") {
                            cancel.dispatch(CancelStrataJob {});
                        }
                    }
                >
                    "Abbrechen"
                </button>
            </div>
        </section>
    }
}

fn confirmed(question: &str) -> bool {
    #[cfg(feature = "hydrate")]
    {
        window().confirm_with_message(question).unwrap_or(false)
    }
    #[cfg(not(feature = "hydrate"))]
    {
        let _ = question;
        true
    }
}

/// Size, context and the button for a model download; for the setup and
/// for another model.
#[component]
fn ModelChoice(
    info: StrataInfo,
    /// Sizes to leave out, e.g. those installed already.
    skip: Vec<String>,
    kind: StrataJobKind,
    button: &'static str,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    let sizes: Vec<StrataSize> = info
        .catalog
        .iter()
        .filter(|s| !skip.contains(&s.id))
        .cloned()
        .collect();
    if sizes.is_empty() {
        return view! { <p class="hint">"Alle Modelle aus Stratas Liste sind schon installiert."</p> }
            .into_any();
    }
    let first = info
        .recommended
        .clone()
        .filter(|r| sizes.iter().any(|s| &s.id == r))
        .or_else(|| sizes.iter().find(|s| s.fits).map(|s| s.id.clone()))
        .unwrap_or_else(|| sizes[0].id.clone());
    let choice = RwSignal::new(first);
    let context = RwSignal::new(info.default_context);
    let speed_projection = RwSignal::new(false);
    let takes_projection = {
        let sizes = sizes.clone();
        let families = info.families.clone();
        Memo::new(move |_| {
            choice.with(|id| {
                sizes.iter().filter(|s| &s.id == id).any(|s| {
                    families
                        .iter()
                        .any(|f| f.id == s.family && f.speed_projection)
                })
            })
        })
    };
    let family_title = |id: &str| {
        info.families
            .iter()
            .find(|f| f.id == id)
            .map_or_else(|| id.to_string(), |f| f.title.clone())
    };
    let options = sizes
        .iter()
        .map(|s| {
            let mut text = format!(
                "{} {}: {} Download, ab {} GB RAM",
                family_title(&s.family),
                s.size,
                gb(s.download_gb),
                s.ram_gb
            );
            if info.recommended.as_deref() == Some(s.id.as_str()) {
                text.push_str(" (empfohlen)");
            }
            if !s.fits {
                text.push_str(" (zu wenig RAM)");
            }
            if s.partly_downloaded {
                text.push_str(" (teils geladen)");
            }
            let id = s.id.clone();
            let selected = move || choice.with(|c| *c == id);
            view! {
                <option value=s.id.clone() selected=selected>
                    {text}
                </option>
            }
        })
        .collect_view();
    let contexts = info
        .contexts
        .iter()
        .map(|&c| {
            let mut text = format!("{}K Token", c / 1024);
            if c == info.default_context {
                text.push_str(" (empfohlen)");
            }
            view! {
                <option value=c.to_string() selected=move || context.get() == c>
                    {text}
                </option>
            }
        })
        .collect_view();
    let disk = info
        .disk_free_gb
        .map(|free| format!("Frei auf dem Laufwerk von {}: {}.", info.data_dir, gb(free)));
    let start = move |_| {
        let id = choice.get();
        let Some(size) = sizes.iter().find(|s| s.id == id) else {
            return;
        };
        actions.start.dispatch(StartStrataJob {
            kind,
            family: Some(size.family.clone()),
            size: Some(size.size.clone()),
            context: Some(context.get()),
            model: None,
            speed_projection: Some(takes_projection.get() && speed_projection.get()),
        });
    };
    let picker = format!("{kind:?}-size");
    let ctx = format!("{kind:?}-context");
    let projection = format!("{kind:?}-speed-projection");
    view! {
        <label for=picker.clone()>"Modell"</label>
        <select
            id=picker
            on:change=move |ev| {
                choice.set(event_target_value(&ev));
                // A tick counts only for the model it was given for.
                speed_projection.set(false);
            }
        >
            {options}
        </select>
        <label for=ctx.clone()>"Kontext"</label>
        <select
            id=ctx
            on:change=move |ev| {
                if let Ok(c) = event_target_value(&ev).parse() {
                    context.set(c);
                }
            }
        >
            {contexts}
        </select>
        <Show when=move || takes_projection.get()>
            <div>
                <input
                    id=projection.clone()
                    type="checkbox"
                    prop:checked=move || speed_projection.get()
                    on:change=move |ev| speed_projection.set(event_target_checked(&ev))
                />
                <label for=projection.clone() class="inline">
                    "Experimentelle Speed-Projection"
                </label>
                <p class="hint">
                    "Experimentell und trotz des Namens nicht schneller: Strata entfernt damit die Ablehnungsrichtung des Modells, es lehnt dann kaum noch Anfragen ab. Clients können sie pro Anfrage mit "
                    <code>"\"experimental_speed_projection\": false"</code>
                    " abschalten."
                </p>
            </div>
        </Show>
        {disk.map(|d| view! { <p class="hint">{d}</p> })}
        <div class="actions">
            <button disabled=move || blocked.get() on:click=start>
                {button}
            </button>
        </div>
    }
    .into_any()
}

#[component]
fn Install(info: StrataInfo, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let ram = format!("Der PC hat {} RAM.", gb(info.ram_gb));
    view! {
        <section class="panel stack">
            <h2>"Strata einrichten"</h2>
            <p>
                "Auf dem PC ist Strata noch nicht eingerichtet. Der PC-Dienst lädt Strata, richtet es mit dem gewählten Modell ein und startet es. Der Strata-Key wird dabei erzeugt und im Proxy eingetragen."
            </p>
            <p class="hint">{ram}</p>
            <ModelChoice
                info
                skip=vec![]
                kind=StrataJobKind::Install
                button="Strata einrichten"
                actions
                blocked
            />
        </section>
    }
}

#[component]
fn Installed(info: StrataInfo, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let installed: Vec<String> = info.models.iter().map(|m| m.id.clone()).collect();
    let version = info
        .version
        .clone()
        .unwrap_or_else(|| "unbekannt".to_string());
    let update = match info.update_available {
        Some(true) => "Eine neuere Version ist verfügbar.",
        Some(false) => "Strata ist aktuell.",
        None => "Ob es eine neuere Version gibt, ist nicht bekannt.",
    };
    let bench = info.benchmark.clone().map_or_else(
        || "Noch kein Benchmark gelaufen.".to_string(),
        |b| format!("Zuletzt: {} mit {}, {}.", b.result, b.model, b.at_text),
    );
    let models = info
        .models
        .iter()
        .cloned()
        .map(|m| view! { <ModelRow model=m actions blocked /> })
        .collect_view();
    view! {
        <section class="panel">
            <h2>"Installierte Modelle"</h2>
            <ul class="events">{models}</ul>
        </section>
        <section class="panel stack">
            <h2>"Weiteres Modell laden"</h2>
            <ModelChoice
                info
                skip=installed
                kind=StrataJobKind::AddModel
                button="Modell laden"
                actions
                blocked
            />
        </section>
        <section class="panel">
            <h2>"Version"</h2>
            <p>{format!("Stand {version}. {update}")}</p>
            <div class="actions">
                <button
                    disabled=move || blocked.get()
                    on:click=move |_| {
                        if confirmed("Strata aktualisieren? Strata ist dabei kurz nicht erreichbar.") {
                            actions.job(StrataJobKind::Update);
                        }
                    }
                >
                    "Aktualisieren"
                </button>
            </div>
        </section>
        <section class="panel">
            <h2>"Benchmark"</h2>
            <p>{bench}</p>
            <p class="hint">"Misst das aktive Modell; Anfragen warten so lange."</p>
            <div class="actions">
                <button
                    disabled=move || blocked.get()
                    on:click=move |_| actions.job(StrataJobKind::Benchmark)
                >
                    "Benchmark starten"
                </button>
            </div>
        </section>
        <section class="panel">
            <h2>"Strata-Key"</h2>
            <p class="hint">
                "Nur Proxy und Strata kennen ihn. Ein neuer Key gilt, sobald Strata ihn übernommen hat."
            </p>
            <div class="actions">
                <button
                    class="secondary"
                    disabled=move || blocked.get()
                    on:click=move |_| {
                        if confirmed("Neuen Strata-Key erzeugen? Strata startet dafür neu.") {
                            actions.job(StrataJobKind::SetKey);
                        }
                    }
                >
                    "Neu erzeugen"
                </button>
            </div>
        </section>
    }
}

#[component]
fn ModelRow(model: StrataModel, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let size = bytes_gb(model.disk_bytes);
    let text = format!("{} ({size})", model.id);
    let buttons = (!model.active).then(|| {
        let activate = model.id.clone();
        let delete = model.id.clone();
        let question = format!("{} löschen? Das gibt {size} frei.", model.id);
        view! {
            <span class="actions inline">
                <button
                    class="secondary"
                    disabled=move || blocked.get()
                    on:click=move |_| actions.model_job(StrataJobKind::Activate, activate.clone())
                >
                    "Aktivieren"
                </button>
                <button
                    class="secondary"
                    disabled=move || blocked.get()
                    on:click=move |_| {
                        if confirmed(&question) {
                            actions.model_job(StrataJobKind::Delete, delete.clone());
                        }
                    }
                >
                    "Löschen"
                </button>
            </span>
        }
    });
    view! {
        <li class="model">
            <span>{text}</span>
            {model.active.then(|| view! { <strong>" aktiv"</strong> })}
            {buttons}
        </li>
    }
}
