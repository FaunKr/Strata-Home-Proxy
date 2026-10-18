//! The area "Strata": Strata on the PC as its service reports it, in tabs
//! for the models, the output, version and benchmark, and the connection.
//! A job's progress comes live with the overview stream.

use leptos::prelude::*;
use leptos_router::components::{A, Outlet};

use crate::{
    api::{
        CancelStrataJob, StartStrataJob, StrataInfo, StrataJob, StrataJobKind, StrataJobState,
        StrataModel, StrataPage, StrataSize, error_text, get_strata,
    },
    confirm::{Confirm, Question},
    labels::{STRATA_CONNECTION, strata_job_label},
    overview::Live,
    settings::SettingsSections,
};

/// What the experimental speed projection does, for whoever switches it on.
const SPEED_PROJECTION_TEXT: &str = "Experimental, and despite the name not faster: Strata removes the model's refusal direction, so it hardly refuses requests any more.";

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

    fn speed_projection(&self, model: String, on: bool) {
        self.start.dispatch(StartStrataJob {
            kind: StrataJobKind::SetSpeedProjection,
            family: None,
            size: None,
            context: None,
            model: Some(model),
            speed_projection: Some(on),
        });
    }

    fn set_context(&self, model: String, context: u32) {
        self.start.dispatch(StartStrataJob {
            kind: StrataJobKind::SetContext,
            family: None,
            size: None,
            context: Some(context),
            model: Some(model),
            speed_projection: None,
        });
    }
}

/// What the tabs share: Strata as the PC service reported it, and the
/// actions that start and cancel jobs.
#[derive(Clone, Copy)]
struct Area {
    page: Resource<Result<StrataPage, ServerFnError>>,
    actions: Actions,
    live: Live,
}

impl Area {
    /// The running job, live once the stream answers; until then the one
    /// the page came with.
    fn job(&self, page: &StrataPage) -> Memo<Option<StrataJob>> {
        let first = page
            .info
            .as_ref()
            .and_then(|i| i.job.clone())
            .filter(|j| j.state == StrataJobState::Running);
        let live = self.live;
        Memo::new(move |_| match live.overview.get() {
            Some(o) => o.strata_job,
            None => first.clone(),
        })
    }

    /// Jobs wait while one runs; gaming mode refuses them.
    fn blocked(&self, page: &StrataPage) -> Signal<bool> {
        let job = self.job(page);
        let gaming = page.gaming;
        let start = self.actions.start;
        Signal::derive(move || gaming || job.with(Option::is_some) || start.pending().get())
    }
}

/// The frame of the area: heading, tabs, notes and a running job; the tab
/// comes below.
#[component]
pub fn StrataArea() -> impl IntoView {
    let live = expect_context::<Live>();
    let actions = Actions {
        start: ServerAction::new(),
        cancel: ServerAction::new(),
    };
    // Asked for again when a job ends, so the tabs show the result.
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
    let area = Area {
        page,
        actions,
        live,
    };
    provide_context(area);
    let tabs = NodeRef::<leptos::html::Nav>::new();
    #[cfg(feature = "hydrate")]
    {
        // On a narrow screen the tabs scroll; keep the current one in view.
        let location = leptos_router::hooks::use_location();
        Effect::new(move |_| {
            location.pathname.track();
            request_animation_frame(move || {
                use wasm_bindgen::JsCast;
                let Some(nav) = tabs.get_untracked() else {
                    return;
                };
                if let Ok(Some(tab)) = nav.query_selector("[aria-current=page]")
                    && let Ok(tab) = tab.dyn_into::<web_sys::HtmlElement>()
                {
                    nav.set_scroll_left(tab.offset_left() - nav.offset_left() - 16);
                }
            });
        });
    }
    view! {
        <div class="page-head">
            <h1>"Strata"</h1>
            <p class="lead">"The engine on your PC that runs the models."</p>
        </div>
        <nav class="tabs" aria-label="Strata" node_ref=tabs>
            <A href="/admin/strata" exact=true attr:class="tab">
                "Models"
            </A>
            <A href="/admin/strata/output" attr:class="tab">
                "Output"
            </A>
            <A href="/admin/strata/version" attr:class="tab">
                "Version & benchmark"
            </A>
            <A href="/admin/strata/connection" attr:class="tab">
                "Connection"
            </A>
        </nav>
        {action_error(actions.start.value())}
        {action_error(actions.cancel.value())}
        <Transition>
            {move || Suspend::new(async move {
                page.await
                    .ok()
                    .map(|p| {
                        let job = area.job(&p);
                        let problem = p
                            .problem
                            .clone()
                            .map(|text| view! { <p class="warning" role="status">{text}</p> });
                        let stale = p
                            .stale
                            .then(|| {
                                view! {
                                    <p class="hint">
                                        "As of the last time the PC service answered."
                                    </p>
                                }
                            });
                        let gaming = p
                            .gaming
                            .then(|| {
                                view! {
                                    <p class="warning" role="status">
                                        "Gaming mode is on: no Strata job starts until it is off."
                                    </p>
                                }
                            });
                        view! {
                            {problem}
                            {stale}
                            {gaming}
                            {move || {
                                job.get().map(|j| view! { <JobCard job=j cancel=actions.cancel /> })
                            }}
                        }
                    })
            })}
        </Transition>
        <Outlet />
    }
}

/// A tab's content once Strata's state has arrived.
fn with_page<V: IntoView + 'static>(
    content: impl Fn(Area, StrataPage) -> V + Clone + Send + Sync + 'static,
) -> impl IntoView {
    let area = expect_context::<Area>();
    view! {
        <Transition fallback=|| view! { <p class="hint">"Loading …"</p> }>
            {
                let content = content.clone();
                move || {
                    let content = content.clone();
                    Suspend::new(async move {
                        match area.page.await {
                            Ok(p) => content(area, p).into_any(),
                            Err(e) => view! { <p class="error">{error_text(&e)}</p> }.into_any(),
                        }
                    })
                }
            }
        </Transition>
    }
}

#[component]
pub fn ModelsTab() -> impl IntoView {
    with_page(|area, page| {
        let blocked = area.blocked(&page);
        let actions = area.actions;
        match page.info {
            None => ().into_any(),
            Some(info) if !info.installed => view! { <Install info actions blocked /> }.into_any(),
            Some(info) => view! { <Models info actions blocked /> }.into_any(),
        }
    })
}

#[component]
pub fn VersionTab() -> impl IntoView {
    with_page(|area, page| {
        let blocked = area.blocked(&page);
        let actions = area.actions;
        match page.info {
            None => ().into_any(),
            Some(info) if !info.installed => not_installed().into_any(),
            Some(info) => view! { <Version info actions blocked /> }.into_any(),
        }
    })
}

#[component]
pub fn ConnectionTab() -> impl IntoView {
    view! {
        {with_page(|area, page| {
            let blocked = area.blocked(&page);
            let actions = area.actions;
            page.info
                .filter(|i| i.installed)
                .map(|_| view! { <StrataKey actions blocked /> })
        })}
        <SettingsSections sections=&[&STRATA_CONNECTION] />
    }
}

fn not_installed() -> impl IntoView {
    view! {
        <p class="hint">
            "Strata is not set up on the PC yet. "
            <a href="/admin/strata">"Set it up under Models."</a>
        </p>
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

/// `81.0 GB`.
fn gb(value: f64) -> String {
    format!("{value:.1} GB")
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
fn JobCard(job: StrataJob, cancel: ServerAction<CancelStrataJob>) -> impl IntoView {
    let title = match &job.model {
        Some(m) => format!("{} {m}", strata_job_label(job.kind)),
        None => strata_job_label(job.kind).to_string(),
    };
    let mut figures = Vec::new();
    if let (Some(done), Some(total)) = (job.done_bytes, job.total_bytes) {
        figures.push(format!("{} of {}", bytes_gb(done), bytes_gb(total)));
    }
    if let Some(speed) = job.bytes_per_sec {
        figures.push(format!("{:.1} MB/s", speed as f64 / 1e6));
    }
    if let Some(eta) = job.eta_secs {
        figures.push(format!("about {} left", eta_text(eta)));
    }
    let lines = job.lines.join("\n");
    let confirm = expect_context::<Confirm>();
    view! {
        <section class="card job" role="status">
            <div class="card-head">
                <h2>{title}</h2>
                {job.percent.map(|p| view! { <span class="muted small">{format!("{p} %")}</span> })}
            </div>
            {job.step.map(|s| view! { <p>{format!("Step {s}")}</p> })}
            {job
                .percent
                .map(|p| {
                    view! { <progress max="100" value=p.to_string() aria-label="Progress"></progress> }
                })}
            {(!figures.is_empty()).then(|| view! { <p class="hint">{figures.join(" · ")}</p> })}
            {(!lines.is_empty()).then(|| view! { <pre class="lines">{lines}</pre> })}
            <p class="muted small">
                "The PC stays awake and other Strata actions wait until this has finished."
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="secondary"
                    disabled=move || cancel.pending().get()
                    on:click=move |_| {
                        confirm
                            .ask(
                                Question {
                                    title: "Cancel the job?".into(),
                                    text: "A download continues from the same point next time."
                                        .into(),
                                    confirm: "Cancel job",
                                    danger: true,
                                },
                                move || {
                                    cancel.dispatch(CancelStrataJob {});
                                },
                            );
                    }
                >
                    "Cancel job"
                </button>
            </div>
        </section>
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
        return view! { <p class="hint">"Every model on Strata's list is installed already."</p> }
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
                "{} {} · {} download · needs {} GB RAM",
                family_title(&s.family),
                s.size,
                gb(s.download_gb),
                s.ram_gb
            );
            if info.recommended.as_deref() == Some(s.id.as_str()) {
                text.push_str(" (recommended)");
            }
            if !s.fits {
                text.push_str(" (not enough RAM)");
            }
            if s.partly_downloaded {
                text.push_str(" (partly downloaded)");
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
            let mut text = format!("{}K tokens", c / 1024);
            if c == info.default_context {
                text.push_str(" (recommended)");
            }
            view! {
                <option value=c.to_string() selected=move || context.get() == c>
                    {text}
                </option>
            }
        })
        .collect_view();
    let room = format!("The PC has {} RAM.", gb(info.ram_gb));
    let disk = info
        .disk_free_gb
        .map(|free| format!(" Free on the drive of {}: {}.", info.data_dir, gb(free)));
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
        <div class="stack">
            <div>
                <label for=picker.clone()>"Model"</label>
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
            </div>
            <div>
                <label for=ctx.clone()>"Context"</label>
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
                <p class="hint">{room}{disk}</p>
            </div>
            <Show when=move || takes_projection.get()>
                <div>
                    <input
                        id=projection.clone()
                        type="checkbox"
                        prop:checked=move || speed_projection.get()
                        on:change=move |ev| speed_projection.set(event_target_checked(&ev))
                    />
                    <label for=projection.clone() class="inline">
                        " Experimental speed projection"
                    </label>
                    <p class="hint">
                        {SPEED_PROJECTION_TEXT}
                        " Clients can switch it off per request with "
                        <code>"\"experimental_speed_projection\": false"</code>
                        "."
                    </p>
                </div>
            </Show>
            <div>
                <button type="button" disabled=move || blocked.get() on:click=start>
                    {button}
                </button>
            </div>
        </div>
    }
    .into_any()
}

#[component]
fn Install(info: StrataInfo, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    view! {
        <section class="card">
            <h2>"Set up Strata"</h2>
            <p class="muted">
                "Strata is not set up on the PC yet. The PC service downloads Strata, sets it up with the model you choose and starts it. It also creates the Strata key and enters it in the proxy."
            </p>
            <ModelChoice
                info
                skip=vec![]
                kind=StrataJobKind::Install
                button="Set up Strata"
                actions
                blocked
            />
        </section>
    }
}

#[component]
fn Models(info: StrataInfo, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let installed: Vec<String> = info.models.iter().map(|m| m.id.clone()).collect();
    let models = info
        .models
        .iter()
        .cloned()
        .map(|m| {
            let takes_projection = info
                .families
                .iter()
                .any(|f| f.speed_projection && m.family.as_deref() == Some(f.id.as_str()));
            let contexts = info.contexts.clone();
            let default_context = info.default_context;
            view! {
                <ModelRow model=m takes_projection contexts default_context actions blocked />
            }
        })
        .collect_view();
    view! {
        <section class="card">
            <h2>"Installed models"</h2>
            <ul class="list">{models}</ul>
        </section>
        <section class="card">
            <h2>"Load another model"</h2>
            <ModelChoice
                info
                skip=installed
                kind=StrataJobKind::AddModel
                button="Load model"
                actions
                blocked
            />
        </section>
    }
}

#[component]
fn ModelRow(
    model: StrataModel,
    /// Its family offers the speed projection.
    takes_projection: bool,
    /// The contexts setup offers, and its pick.
    contexts: Vec<u32>,
    default_context: u32,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    let size = bytes_gb(model.disk_bytes);
    // Only with word from the PC service on the context: an older one does not say.
    let switch = model.context.map(|current| {
        view! {
            <ContextSwitch
                id=model.id.clone()
                active=model.active
                current
                contexts
                default_context
                actions
                blocked
            />
        }
    });
    let mut meta = match &model.size {
        Some(quant) if *quant != model.id => format!("{quant} · {size}"),
        _ => size.clone(),
    };
    if let Some(context) = model.context {
        meta.push_str(&format!(" · {}K context", context / 1024));
    }
    let buttons = (!model.active).then(|| {
        let activate = model.id.clone();
        let delete = model.id.clone();
        let title = format!("Delete {}?", model.id);
        let text = format!("This frees {size} on the PC. You can load it again later.");
        let confirm = expect_context::<Confirm>();
        view! {
            <span class="actions inline">
                <button
                    type="button"
                    class="secondary"
                    disabled=move || blocked.get()
                    on:click=move |_| actions.model_job(StrataJobKind::Activate, activate.clone())
                >
                    "Make active"
                </button>
                <button
                    type="button"
                    class="ghost-danger"
                    disabled=move || blocked.get()
                    on:click=move |_| {
                        let delete = delete.clone();
                        confirm
                            .ask(
                                Question {
                                    title: title.clone(),
                                    text: text.clone(),
                                    confirm: "Delete model",
                                    danger: true,
                                },
                                move || actions.model_job(StrataJobKind::Delete, delete.clone()),
                            );
                    }
                >
                    "Delete"
                </button>
            </span>
        }
    });
    // Only with word from the PC service on its state: an older one does not say.
    let projection = model.speed_projection.filter(|_| takes_projection).map(|on| {
        let id = model.id.clone();
        let (label, question) = if on {
            (
                "Switch speed projection off",
                Question {
                    title: format!("Switch the speed projection off for {id}?"),
                    text: "The model answers as it was trained again.".into(),
                    confirm: "Switch off",
                    danger: false,
                },
            )
        } else {
            (
                "Switch speed projection on",
                Question {
                    title: format!("Switch the speed projection on for {id}?"),
                    text: SPEED_PROJECTION_TEXT.into(),
                    confirm: "Switch on",
                    danger: false,
                },
            )
        };
        let confirm = expect_context::<Confirm>();
        view! {
            <button
                type="button"
                class="secondary"
                disabled=move || blocked.get()
                on:click=move |_| {
                    let id = id.clone();
                    confirm.ask(question.clone(), move || actions.speed_projection(id.clone(), !on));
                }
            >
                {label}
            </button>
        }
    });
    view! {
        <li>
            <div class="grow">
                <p class="strong">{model.id.clone()}</p>
                <p class="muted small">{meta}</p>
            </div>
            {(model.speed_projection == Some(true))
                .then(|| {
                    view! {
                        <span class="badge is-warn">
                            <span class="dot"></span>
                            "Speed projection on"
                        </span>
                    }
                })}
            {model
                .active
                .then(|| {
                    view! {
                        <span class="badge is-ok">
                            <span class="dot"></span>
                            "Active"
                        </span>
                    }
                })}
            {projection}
            {buttons}
            {switch}
        </li>
    }
}

/// The longest context the switch offers: the model's trained length. Past
/// it setup adds rope scaling, which only setup does.
const MAX_SWITCHED_CONTEXT: u32 = 262_144;

/// Picks another context for an installed model.
#[component]
fn ContextSwitch(
    id: String,
    /// Strata restarts with it: that is asked first.
    active: bool,
    current: u32,
    contexts: Vec<u32>,
    default_context: u32,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    let offered: Vec<u32> = contexts
        .into_iter()
        .filter(|&c| c <= MAX_SWITCHED_CONTEXT)
        .collect();
    let first = if offered.contains(&current) {
        current
    } else {
        default_context
    };
    let choice = RwSignal::new(first);
    let options = offered
        .iter()
        .map(|&c| {
            let mut text = format!("{}K tokens", c / 1024);
            if c == current {
                text.push_str(" (current)");
            } else if c == default_context {
                text.push_str(" (recommended)");
            }
            view! {
                <option value=c.to_string() selected=move || choice.get() == c>
                    {text}
                </option>
            }
        })
        .collect_view();
    let picker = format!("{id}-new-context");
    let confirm = expect_context::<Confirm>();
    let change = move |_| {
        let (id, context) = (id.clone(), choice.get());
        if !active {
            actions.set_context(id, context);
            return;
        }
        let question = Question {
            title: format!("Change the context of {id} to {}K tokens?", context / 1024),
            text: "Strata restarts with it once no request is running. Requests wait meanwhile."
                .into(),
            confirm: "Change context",
            danger: false,
        };
        confirm.ask(question, move || actions.set_context(id.clone(), context));
    };
    view! {
        <div class="row-form">
            <label for=picker.clone() class="visually-hidden">
                "New context"
            </label>
            <select
                id=picker
                on:change=move |ev| {
                    if let Ok(c) = event_target_value(&ev).parse() {
                        choice.set(c);
                    }
                }
            >
                {options}
            </select>
            <button
                type="button"
                class="secondary"
                disabled=move || blocked.get() || choice.get() == current
                on:click=change
            >
                "Change context"
            </button>
        </div>
    }
}

#[component]
fn Version(info: StrataInfo, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let confirm = expect_context::<Confirm>();
    let version = info
        .version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let (badge, class) = match info.update_available {
        Some(true) => ("Update available", "badge is-warn"),
        Some(false) => ("Up to date", "badge is-ok"),
        None => ("Not known", "badge is-idle"),
    };
    let bench = info.benchmark.clone().map_or_else(
        || "No benchmark has run yet.".to_string(),
        |b| format!("Last: {} with {}, {}.", b.result, b.model, b.at_text),
    );
    view! {
        <section class="card">
            <div class="card-head">
                <h2>"Version"</h2>
                <span class=class>
                    <span class="dot"></span>
                    {badge}
                </span>
            </div>
            <dl class="facts">
                <dt>"Installed"</dt>
                <dd>{version}</dd>
            </dl>
            <div class="actions">
                <button
                    type="button"
                    disabled=move || blocked.get()
                    on:click=move |_| {
                        confirm
                            .ask(
                                Question {
                                    title: "Update Strata?".into(),
                                    text: "Strata restarts with the new version and cannot answer requests for a moment."
                                        .into(),
                                    confirm: "Update Strata",
                                    danger: false,
                                },
                                move || actions.job(StrataJobKind::Update),
                            );
                    }
                >
                    "Update Strata"
                </button>
            </div>
        </section>
        <section class="card">
            <h2>"Benchmark"</h2>
            <p>{bench}</p>
            <p class="muted small">"Measures the active model. Requests wait while it runs."</p>
            <div class="actions">
                <button
                    type="button"
                    class="secondary"
                    disabled=move || blocked.get()
                    on:click=move |_| actions.job(StrataJobKind::Benchmark)
                >
                    "Run benchmark"
                </button>
            </div>
        </section>
    }
}

#[component]
fn StrataKey(actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let confirm = expect_context::<Confirm>();
    view! {
        <section class="card">
            <h2>"Strata key"</h2>
            <p class="muted">
                "Only the proxy and Strata know it. Clients use their own API keys under Access. A new key applies once Strata has taken it over."
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="secondary"
                    disabled=move || blocked.get()
                    on:click=move |_| {
                        confirm
                            .ask(
                                Question {
                                    title: "Create a new Strata key?".into(),
                                    text: "Strata restarts to take it over and cannot answer requests for a moment. The proxy switches to the new key by itself."
                                        .into(),
                                    confirm: "Create new key",
                                    danger: false,
                                },
                                move || actions.job(StrataJobKind::SetKey),
                            );
                    }
                >
                    "Create new Strata key"
                </button>
            </div>
        </section>
    }
}
