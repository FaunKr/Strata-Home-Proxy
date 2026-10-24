//! The area "Strata": Strata on the PC as its service reports it, in tabs
//! for the models, the output, version and benchmark, and the connection.
//! A job's progress comes live with the overview stream.

use std::collections::BTreeMap;

use leptos::{html::Dialog, prelude::*};
use leptos_router::components::{A, Outlet};
use strata_pc_protocol::{BenchTool, MAX_SWITCHED_CONTEXT, Pictures, StrataFamily, StrataInstall};

use crate::{
    api::{
        CancelCodeBench, CancelStrataJob, CodeBenchRun, CodeBenchState, CodeBenchTask,
        CodeBenchTaskInfo, ForceRestartStrata, ForcedRestart, GenerateUpdateSummary,
        GetOptimizationPrompt, PcView, PresetState, SamplingField, SamplingSource, SamplingSupport,
        StartCodeBench, StartCodeBenchAll, StartStrataJob, StrataBenchmark, StrataInfo, StrataJob,
        StrataJobKind, StrataJobState, StrataModel, StrataPage, StrataPreset, StrataPresets,
        StrataSampling, StrataSize, StrataTune, StrataUpdate, TuneMode, TuneSpeed, TuneUse,
        UpdateSummary, error_text, get_code_bench, get_strata,
    },
    confirm::{Confirm, Question},
    copy::CopyText,
    labels::{STRATA_CONNECTION, STRATA_TIMEOUTS, TOO_OLD_FOR_KEY, strata_badge, strata_job_label},
    overview::{Live, strata_sentence},
    settings::SettingsSections,
};

/// What the experimental speed projection does, for whoever switches it on.
const SPEED_PROJECTION_TEXT: &str = "Experimental, and despite the name not faster: Strata removes the model's refusal direction, so it hardly refuses requests any more.";

/// What picture reading costs and does, for whoever ticks it.
const PICTURES_TEXT: &str = "Lets the model read pictures. About 0.9 GB extra download. On cards under 24 GB, pictures are read on the CPU (a few seconds each).";

/// What switching picture reading off does.
const PICTURES_OFF_TEXT: &str = "The model stops reading pictures. The picture file stays on disk, so switching on again needs no download.";

#[derive(Clone, Copy)]
struct Actions {
    start: ServerAction<StartStrataJob>,
    cancel: ServerAction<CancelStrataJob>,
    summarize: ServerAction<GenerateUpdateSummary>,
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
            pictures: None,
            tune_mode: None,
            tune_use: None,
            preset: None,
            options: None,
            env: None,
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
            pictures: None,
            tune_mode: None,
            tune_use: None,
            preset: None,
            options: None,
            env: None,
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
            pictures: None,
            tune_mode: None,
            tune_use: None,
            preset: None,
            options: None,
            env: None,
        });
    }

    fn pictures(&self, model: String, on: bool) {
        self.start.dispatch(StartStrataJob {
            kind: StrataJobKind::SetPictures,
            family: None,
            size: None,
            context: None,
            model: Some(model),
            speed_projection: None,
            pictures: Some(on),
            tune_mode: None,
            tune_use: None,
            preset: None,
            options: None,
            env: None,
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
            pictures: None,
            tune_mode: None,
            tune_use: None,
            preset: None,
            options: None,
            env: None,
        });
    }

    /// Saves the fields' texts as `model`'s engine options (none for a
    /// Strata that keeps none by hand) and engine environment.
    fn set_engine_settings(&self, model: String, options: Option<String>, env: String) {
        self.start.dispatch(StartStrataJob {
            kind: StrataJobKind::SetEngineSettings,
            family: None,
            size: None,
            context: None,
            model: Some(model),
            speed_projection: None,
            pictures: None,
            tune_mode: None,
            tune_use: None,
            preset: None,
            options,
            env: Some(env),
        });
    }

    fn tune(&self, model: String, mode: TuneMode, usage: TuneUse) {
        self.start.dispatch(StartStrataJob {
            kind: StrataJobKind::Tune,
            family: None,
            size: None,
            context: None,
            model: Some(model),
            speed_projection: None,
            pictures: None,
            tune_mode: Some(mode),
            tune_use: Some(usage),
            preset: None,
            options: None,
            env: None,
        });
    }

    /// Applies `preset` of `model`, or undoes it.
    fn preset(&self, model: String, preset: String, undo: bool) {
        self.start.dispatch(StartStrataJob {
            kind: if undo {
                StrataJobKind::UndoPreset
            } else {
                StrataJobKind::ApplyPreset
            },
            family: None,
            size: None,
            context: None,
            model: Some(model),
            speed_projection: None,
            pictures: None,
            tune_mode: None,
            tune_use: None,
            preset: Some(preset),
            options: None,
            env: None,
        });
    }

    /// Turns `model`'s active preset off; the preset stays saved.
    fn preset_off(&self, model: String) {
        self.start.dispatch(StartStrataJob {
            kind: StrataJobKind::PresetOff,
            family: None,
            size: None,
            context: None,
            model: Some(model),
            speed_projection: None,
            pictures: None,
            tune_mode: None,
            tune_use: None,
            preset: None,
            options: None,
            env: None,
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
        summarize: ServerAction::new(),
    };
    // The start of the failed job whose card was closed: a new page answer
    // must not bring it back.
    let closed_job = RwSignal::new(None::<u64>);
    // Counts up every few seconds while Strata writes the update summary.
    let tick = RwSignal::new(0u32);
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
    // Counts the jobs the PC service took: a refused one changes nothing, and
    // asking again would build the models anew and drop an open dialog's text.
    let started = RwSignal::new(0u32);
    Effect::new(move || {
        if matches!(actions.start.value().get(), Some(Ok(()))) {
            started.update(|n| *n += 1);
        }
    });
    let page = Resource::new(
        move || {
            (
                started.get(),
                actions.cancel.version().get(),
                actions.summarize.version().get(),
                job_ended.get(),
                tick.get(),
            )
        },
        |_| get_strata(),
    );
    #[cfg(feature = "hydrate")]
    {
        let writing = Memo::new(move |_| {
            page.get().and_then(|p| p.ok()).is_some_and(|p| {
                p.update
                    .is_some_and(|u| matches!(u.summary, UpdateSummary::Writing { .. }))
            })
        });
        let ticker = set_interval_with_handle(
            move || {
                if writing.get_untracked() {
                    tick.update(|n| *n += 1);
                }
            },
            SUMMARY_POLL,
        )
        .ok();
        on_cleanup(move || {
            if let Some(t) = ticker {
                t.clear();
            }
        });
    }
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
        <StrataState live />
        {action_error(actions.start.value())}
        {action_error(actions.cancel.value())}
        {action_error(actions.summarize.value())}
        <Transition>
            {move || Suspend::new(async move {
                page.await
                    .ok()
                    .map(|p| {
                        let job = area.job(&p);
                        // The last job, if it failed: its card stays with the
                        // error and the last lines until closed or the next job.
                        let failed = p
                            .info
                            .as_ref()
                            .and_then(|i| i.job.clone())
                            .filter(|j| {
                                matches!(
                                    j.state,
                                    StrataJobState::Failed | StrataJobState::Interrupted
                                )
                            });
                        let problem = p
                            .problem
                            .clone()
                            .map(|text| view! { <p class="warning" role="status">{text}</p> });
                        let stale = p
                            .stale
                            .then(|| {
                                let text = p.as_of.as_ref().map_or_else(
                                    || "As of the last time the PC service answered.".to_string(),
                                    |at| format!("As of {at}, when the PC service last answered."),
                                );
                                view! { <p class="hint">{text}</p> }
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
                            {move || {
                                (job.with(Option::is_none))
                                    .then(|| failed.clone())
                                    .flatten()
                                    .filter(|j| closed_job.get() != Some(j.started))
                                    .map(|j| view! { <FailedJob job=j closed=closed_job /> })
                            }}
                        }
                    })
            })}
        </Transition>
        <Outlet />
    }
}

/// Strata's state, live, with Force restart Strata while the PC is awake
/// and gaming mode is off, whatever state Strata is in.
#[component]
fn StrataState(live: Live) -> impl IntoView {
    let restart = ServerAction::<ForceRestartStrata>::new();
    let confirm = expect_context::<Confirm>();
    let ask = move |_| {
        confirm.ask(
            Question {
                title: "Force a Strata restart?".into(),
                text: "Strata is killed hard and started again: open requests fail, and a running Strata job is cancelled.".into(),
                confirm: "Restart Strata",
                danger: true,
            },
            move || {
                restart.dispatch(ForceRestartStrata {});
            },
        );
    };
    move || {
        let (badge, class, sentence, awake) = live.overview.with(|o| {
            o.as_ref().map(|o| {
                let (badge, class) = strata_badge(o);
                (
                    badge,
                    class,
                    strata_sentence(&o.strata),
                    o.pc == PcView::Awake,
                )
            })
        })?;
        let agent = live
            .overview
            .with(|o| o.as_ref().is_some_and(|o| o.setup.agent_configured));
        Some(view! {
            <section class="card strata-state" aria-label="Strata's state">
                <div class="strata-state-row">
                    <span class=format!("badge {class}")>{badge}</span>
                    <span class="muted">{sentence}</span>
                    {awake
                        .then(|| {
                            view! {
                                <button
                                    type="button"
                                    class="secondary"
                                    disabled=move || !agent || restart.pending().get()
                                    on:click=ask
                                >
                                    "Force restart Strata"
                                </button>
                            }
                        })}
                </div>
                {move || {
                    restart
                        .pending()
                        .get()
                        .then(|| view! { <p class="hint" role="status">"Restarting Strata …"</p> })
                }}
                {move || {
                    restart
                        .value()
                        .get()
                        .map(|r| match r {
                            Ok(forced) => {
                                view! { <p class="hint" role="status">{restart_text(&forced)}</p> }
                                    .into_any()
                            }
                            Err(e) => {
                                view! { <p class="error" role="alert">{error_text(&e)}</p> }
                                    .into_any()
                            }
                        })
                }}
            </section>
        })
    }
}

/// The result line of a forced Strata restart.
fn restart_text(forced: &ForcedRestart) -> String {
    let mut text = if forced.started {
        "Strata was killed and starts again; it loads the model now.".to_string()
    } else {
        "Strata was killed and stays stopped: nothing to start (no start script set up, or auto-start is off), the start failed (see the Strata output), or a Strata job took over.".to_string()
    };
    if let Some(kind) = forced.cancelled_job {
        text.push_str(&format!(" Cancelled job: {}.", strata_job_label(kind)));
    }
    text
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
    // The code benchmark loads on its own, outside the page's Suspend: a
    // resource created inside it does not hydrate.
    view! {
        {with_page(|area, page| {
            let blocked = area.blocked(&page);
            let actions = area.actions;
            let update = page.update;
            let asleep = page.stale;
            match page.info {
                None => update
                    .map(|update| {
                        view! {
                            <section class="card">
                                <h2>"Update"</h2>
                                <Changes update actions blocked />
                            </section>
                        }
                    })
                    .into_any(),
                Some(info) if !info.installed => not_installed().into_any(),
                Some(info) => view! { <Version info update asleep actions blocked /> }.into_any(),
            }
        })}
        <CodeBenchCard />
    }
}

#[component]
pub fn ConnectionTab() -> impl IntoView {
    view! {
        {with_page(|_, page| {
            let unreported = page.key_unreported;
            page.info
                .filter(|i| i.installed)
                .map(|_| view! { <StrataKey unreported /> })
        })}
        <SettingsSections sections=&[&STRATA_CONNECTION, &STRATA_TIMEOUTS] />
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

/// How long a job has run, as the tune's step log counts it: `12m`,
/// `1h 05m`.
fn running_text(secs: u64) -> String {
    let minutes = secs / 60;
    match minutes {
        0..60 => format!("{minutes}m"),
        _ => format!("{}h {:02}m", minutes / 60, minutes % 60),
    }
}

/// A running tune's measurements, one row each: the model's own settings
/// (Strata's `base`) in plain words, the others with their difference from
/// them, and the fastest marked.
fn speed_rows(speeds: &[TuneSpeed]) -> Vec<String> {
    let base = speeds.iter().find(|s| s.label == "base").map(|s| s.tok_s);
    // The first of equally fast ones.
    let fastest = speeds
        .iter()
        .enumerate()
        .fold(None, |best: Option<(usize, f64)>, (i, s)| match best {
            Some((_, tok_s)) if tok_s >= s.tok_s => best,
            _ => Some((i, s.tok_s)),
        })
        .map(|(i, _)| i);
    speeds
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut row = if s.label == "base" {
                format!("Your current settings: {:.1} tok/s", s.tok_s)
            } else {
                format!("{}: {:.1} tok/s", s.label, s.tok_s)
            };
            if let Some(base) = base.filter(|b| *b > 0.0)
                && s.label != "base"
            {
                row.push_str(&format!(" ({:+.1} %)", (s.tok_s / base - 1.0) * 100.0));
            }
            if fastest == Some(i) && speeds.len() > 1 {
                row.push_str(" · fastest so far");
            }
            row
        })
        .collect()
}

/// What a job does, with its model: `Loading model coder-IQ1_M`.
fn job_title(job: &StrataJob) -> String {
    match &job.model {
        Some(m) => format!("{} {m}", strata_job_label(job.kind)),
        None => strata_job_label(job.kind).to_string(),
    }
}

/// A job that failed: what it did, why it failed and its last lines.
#[component]
fn FailedJob(job: StrataJob, closed: RwSignal<Option<u64>>) -> impl IntoView {
    let what = job_title(&job);
    let (title, reason) = match job.state {
        StrataJobState::Interrupted => (
            format!("{what} was interrupted"),
            "The PC service restarted while it ran.",
        ),
        _ => (
            format!("{what} failed"),
            "The PC service gave no reason; the lines below are the job's last output.",
        ),
    };
    let error = job.error.clone().unwrap_or_else(|| reason.into());
    let started = job.started;
    let lines = job.lines.join("\n");
    view! {
        <section class="card job failed-job" role="status">
            <div class="card-head">
                <h2>{title}</h2>
            </div>
            <p class="error">{error}</p>
            {(!lines.is_empty()).then(|| view! { <pre class="lines">{lines}</pre> })}
            <div class="actions">
                <button type="button" class="secondary" on:click=move |_| closed.set(Some(started))>
                    "Close"
                </button>
            </div>
        </section>
    }
}

#[component]
fn JobCard(job: StrataJob, cancel: ServerAction<CancelStrataJob>) -> impl IntoView {
    let title = job_title(&job);
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
    let speeds = speed_rows(&job.speeds);
    let confirm = expect_context::<Confirm>();
    // Before its first measurement a tune has no share to show, and Strata
    // may not write again until it ends: the time it runs shows it moves.
    let running_for = (job.kind == StrataJobKind::Tune && job.percent.is_none()).then(|| {
        let live = expect_context::<Area>().live;
        let started = job.started;
        move || {
            live.now().map(|now| {
                let text = format!("Running for {}", running_text(now.saturating_sub(started)));
                view! { <p class="hint">{text}</p> }
            })
        }
    });
    view! {
        <section class="card job" role="status">
            <div class="card-head">
                <h2>{title}</h2>
                {job.percent.map(|p| view! { <span class="muted small">{format!("{p} %")}</span> })}
            </div>
            {job.step.map(|s| view! { <p>{format!("Step {s}")}</p> })}
            {running_for}
            {job
                .percent
                .map(|p| {
                    view! { <progress max="100" value=p.to_string() aria-label="Progress"></progress> }
                })}
            {(!figures.is_empty()).then(|| view! { <p class="hint">{figures.join(" · ")}</p> })}
            {(!lines.is_empty()).then(|| view! { <pre class="lines">{lines}</pre> })}
            {(!speeds.is_empty())
                .then(|| {
                    view! {
                        <p class="hint">"Measured so far"</p>
                        <ul class="speeds">
                            {speeds.into_iter().map(|r| view! { <li>{r}</li> }).collect_view()}
                        </ul>
                    }
                })}
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
    if let Some(why) = info.catalog_unreadable.clone() {
        return view! {
            <p class="hint">"Update Strata to choose a model."</p>
            <p class="muted small">{format!("Strata's model list could not be read: {why}")}</p>
        }
        .into_any();
    }
    if info.catalog_reading {
        return view! { <p class="hint">"Reading Strata's model list…"</p> }.into_any();
    }
    if info.catalog.is_empty() {
        return view! { <p class="hint">"Strata's model list comes with Strata."</p> }.into_any();
    }
    if sizes.is_empty() {
        return view! { <p class="hint">"Every model on Strata's list is installed already."</p> }
            .into_any();
    }
    let first = info
        .recommended
        .clone()
        .filter(|r| sizes.iter().any(|s| &s.id == r))
        // Never Gyro, as setup never pre-selects it.
        .or_else(|| {
            sizes
                .iter()
                .find(|s| s.fits && !s.memory_with_vram)
                .map(|s| s.id.clone())
        })
        .unwrap_or_else(|| sizes[0].id.clone());
    let choice = RwSignal::new(first);
    let context = RwSignal::new(info.default_context);
    let speed_projection = RwSignal::new(false);
    let pictures = RwSignal::new(true);
    let takes = |field: fn(&StrataSize, &StrataFamily) -> bool| {
        let sizes = sizes.clone();
        let families = info.families.clone();
        Memo::new(move |_| {
            choice.with(|id| {
                sizes
                    .iter()
                    .filter(|s| &s.id == id)
                    .any(|s| families.iter().any(|f| f.id == s.family && field(s, f)))
            })
        })
    };
    let takes_projection = takes(|_, f| f.speed_projection);
    // Per size from Strata's catalog; an older PC service says it per family.
    let takes_pictures = takes(|s, f| s.pictures.unwrap_or(f.pictures));
    // Strata's words about the chosen size.
    let strata_words = {
        let sizes = sizes.clone();
        move || {
            choice.with(|id| {
                let size = sizes.iter().find(|s| &s.id == id)?;
                let verdict = size.verdict.as_ref().map(|v| format!("On this PC: {v}."));
                let about =
                    (!size.about.is_empty()).then(|| format!("{}.", capitalized(&size.about)));
                let text = [about, verdict]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                (!text.is_empty()).then(|| view! { <p class="hint">{text}</p> })
            })
        }
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
            // Gyro's memory is RAM and VRAM together.
            let memory = if s.memory_with_vram {
                "RAM + VRAM"
            } else {
                "RAM"
            };
            let mut text = format!(
                "{} {} · {} download · needs {} GB {memory}",
                family_title(&s.family),
                s.size,
                gb(s.download_gb),
                s.ram_gb
            );
            if info.recommended.as_deref() == Some(s.id.as_str()) {
                text.push_str(" (recommended)");
            }
            if s.experimental {
                text.push_str(" (experimental)");
            }
            match s.verdict.as_deref().map(verdict_head) {
                // Strata's verdict; "fits" goes without saying.
                Some(head) if head != "fits" => text.push_str(&format!(" ({head})")),
                Some(_) => {}
                None if !s.fits => text.push_str(&format!(" (not enough {memory})")),
                None => {}
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
    let room = match info.vram_gb {
        Some(vram) => format!("The PC has {} RAM and {} VRAM.", gb(info.ram_gb), gb(vram)),
        None => format!("The PC has {} RAM.", gb(info.ram_gb)),
    };
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
            pictures: Some(takes_pictures.get() && pictures.get()),
            tune_mode: None,
            tune_use: None,
            preset: None,
            options: None,
            env: None,
        });
    };
    let picker = format!("{kind:?}-size");
    let ctx = format!("{kind:?}-context");
    let projection = format!("{kind:?}-speed-projection");
    let pictures_field = format!("{kind:?}-pictures");
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
                {strata_words}
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
            <Show when=move || takes_pictures.get()>
                <div>
                    <input
                        id=pictures_field.clone()
                        type="checkbox"
                        prop:checked=move || pictures.get()
                        on:change=move |ev| pictures.set(event_target_checked(&ev))
                    />
                    <label for=pictures_field.clone() class="inline">
                        " Pictures"
                    </label>
                    <p class="hint">{PICTURES_TEXT}</p>
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

/// The head of Strata's verdict for a list entry: `tight` of `tight: it
/// needs …`, `fits in the low-RAM mode` of `fits in the low-RAM mode
/// (slower: …)`.
fn verdict_head(verdict: &str) -> &str {
    verdict.split([':', '(']).next().unwrap_or(verdict).trim()
}

/// Strata writes its descriptions in lower case, as the middle of a line.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
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
    // Only with word from the PC service on the sampling: an older one does not say.
    let sampling_note = info.models.iter().any(|m| m.sampling.is_some()).then(|| {
        view! {
            <p class="hint">
                "Sampling for requests that send none, from the model card unless marked. "
                "A client's own sampling fields still win."
            </p>
        }
    });
    view! {
        <section class="card">
            <h2>"Installed models"</h2>
            <ul class="list">{models}</ul>
            {sampling_note}
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
    // Without Strata's catalog there are no contexts to offer.
    let switch = model
        .context
        .filter(|_| !contexts.is_empty())
        .map(|current| {
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
    if let Some(recovery) = &model.loop_recovery {
        meta.push_str(match recovery.as_str() {
            "recover" => " · loop recovery on",
            "stop" => " · loop recovery stops the reply",
            _ => " · loop recovery off",
        });
    }
    if let Some(pictures) = model.pictures {
        meta.push_str(match pictures {
            Pictures::Gpu => " · Pictures: on (GPU)",
            Pictures::Cpu => " · Pictures: on (CPU)",
            Pictures::Off => " · Pictures: off",
        });
    }
    let activate = (!model.active).then(|| {
        let id = model.id.clone();
        view! {
            <button
                type="button"
                class="secondary"
                disabled=move || blocked.get()
                on:click=move |_| actions.model_job(StrataJobKind::Activate, id.clone())
            >
                "Make active"
            </button>
        }
    });
    let delete = (!model.active).then(|| {
        let delete = model.id.clone();
        let title = format!("Delete {}?", model.id);
        let text = format!("This frees {size} on the PC. You can load it again later.");
        let confirm = expect_context::<Confirm>();
        view! {
            <button
                type="button"
                class="ghost-danger push"
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
    // Only with word from the PC service on the picture state: an older one
    // does not say. Switching runs setup for that model again.
    let pictures = model.pictures.map(|current| {
        let on = current == Pictures::Off;
        let id = model.id.clone();
        let label = if on {
            "Switch picture reading on"
        } else {
            "Switch picture reading off"
        };
        let question = Question {
            title: format!(
                "Switch picture reading {} for {id}?",
                if on { "on" } else { "off" }
            ),
            text: if on { PICTURES_TEXT } else { PICTURES_OFF_TEXT }.into(),
            confirm: if on { "Switch on" } else { "Switch off" },
            danger: false,
        };
        let confirm = expect_context::<Confirm>();
        view! {
            <button
                type="button"
                class="secondary"
                disabled=move || blocked.get()
                on:click=move |_| {
                    let id = id.clone();
                    confirm.ask(question.clone(), move || actions.pictures(id.clone(), on));
                }
            >
                {label}
            </button>
        }
    });
    let sampling = model.sampling.as_ref().map(sampling_lines);
    let last_tune = model
        .last_tune
        .as_ref()
        .map(|tune| view! { <p class="muted small">{last_tune_text(tune)}</p> });
    let presets = model.presets.as_ref();
    let preset_line = presets
        .and_then(preset_text)
        .map(|text| view! { <p class="muted small preset-line">{text}</p> });
    let preset_button = presets
        .and_then(|p| p.latest.clone())
        .map(|preset| view! { <PresetButton model=model.id.clone() preset actions blocked /> });
    // Only from a PC service that knows the job, which came with `env`.
    let preset_off = presets
        .and_then(|p| p.latest.clone())
        .filter(|preset| preset.active && model.env.is_some())
        .map(|preset| view! { <PresetOff model=model.id.clone() preset actions blocked /> });
    // Only from a PC service that knows the job that saves them.
    let engine = model.env.clone().map(|env| {
        let started = model.started_args.clone().unwrap_or_default();
        let preset = presets.and_then(|p| p.latest.clone());
        view! {
            <EngineSettings
                id=model.id.clone()
                active=model.active
                user=model.user_args.clone()
                env
                started
                preset
                actions
                blocked
            />
        }
        .into_any()
    });
    // Only with word from the PC service: an older one does not say.
    let tune = match (model.tunable, model.tune_unavailable.clone()) {
        (Some(true), _) => {
            Some(view! { <Tune model=model.id.clone() actions blocked /> }.into_any())
        }
        (Some(false), why) => Some(
            view! {
                <span class="muted small">
                    {why.unwrap_or_else(|| "Strata's tuner cannot tune this model".into())}
                </span>
            }
            .into_any(),
        ),
        (None, _) => None,
    };
    // Name and state on top, what it runs with below, then the context and
    // one row of buttons in the same order for each model; Delete stands apart.
    view! {
        <li class="model">
            <div class="model-head">
                <p class="strong">{model.id.clone()}</p>
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
                {(model.speed_projection == Some(true))
                    .then(|| {
                        view! {
                            <span class="badge is-warn">
                                <span class="dot"></span>
                                "Speed projection on"
                            </span>
                        }
                    })}
            </div>
            <p class="muted small">{meta}</p>
            {sampling}
            {last_tune}
            {preset_line}
            {switch}
            <div class="model-actions">
                {activate}{projection}{pictures}{engine}<OptimizationPrompt model=model.id.clone() />{preset_button}{preset_off}{tune}
                {delete}
            </div>
        </li>
    }
}

/// *Engine settings* for one model: a dialog with its engine options set by
/// hand (`user_args`, one option per line) and its engine environment
/// (`env`, one `NAME=value` per line), the args Strata starts it with and
/// one *Save* for both. Opened, it shows what the PC service reported; it
/// stays open with the reason when the save is refused, so nothing typed
/// is lost.
#[component]
fn EngineSettings(
    id: String,
    active: bool,
    /// `None` for a Strata that keeps no options by hand.
    user: Option<Vec<String>>,
    env: BTreeMap<String, String>,
    started: Vec<String>,
    preset: Option<StrataPreset>,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    let dialog = NodeRef::<Dialog>::new();
    let saved_options = user.as_ref().map(|user| option_lines(user, '\n'));
    let saved_env = env_lines(&env);
    let options = RwSignal::new(saved_options.clone().unwrap_or_default());
    let env_text = RwSignal::new(saved_env.clone());
    let title_id = format!("engine-title-{id}");
    let labelled_by = title_id.clone();
    let options_id = format!("{id}-engine-options");
    let env_id = format!("{id}-engine-env");
    let title = format!("Engine settings for {id}");
    let preset_note = preset.filter(applied).map(|p| {
        let sets: Vec<&String> = p
            .settings
            .iter()
            .filter(|s| !s.starts_with("per request"))
            .collect();
        let text = if sets.is_empty() {
            format!("Preset {} is active; its settings win over these.", p.name)
        } else {
            let sets: Vec<&str> = sets.iter().map(|s| s.as_str()).collect();
            format!(
                "Preset {} is active and sets {}, which win over these.",
                p.name,
                sets.join(", ")
            )
        };
        view! { <p class="muted small">{text}</p> }
    });
    let started_line = (!started.is_empty())
        .then(|| format!("Strata starts with: {}", option_lines(&started, ' ')));
    let option_rows = user.as_ref().map_or(2, |user| {
        (user.iter().filter(|a| a.starts_with("--")).count() + 1).clamp(3, 12)
    });
    let env_rows = (env.len() + 1).clamp(3, 12);
    let close = move || {
        if let Some(d) = dialog.get() {
            d.close();
        }
    };
    // The save's way: sent, then seen pending, then its answer.
    let saving = RwSignal::new(Saving::Idle);
    let refused = RwSignal::new(None::<String>);
    Effect::new(move || {
        let pending = actions.start.pending().get();
        match saving.get() {
            Saving::Sent if pending => saving.set(Saving::Pending),
            Saving::Pending if !pending => {
                saving.set(Saving::Idle);
                match actions.start.value().get_untracked() {
                    Some(Err(e)) => refused.set(Some(error_text(&e))),
                    _ => close(),
                }
            }
            _ => {}
        }
    });
    let (open_options, open_env) = (saved_options.clone(), saved_env.clone());
    let open = move |_| {
        refused.set(None);
        options.set(open_options.clone().unwrap_or_default());
        env_text.set(open_env.clone());
        if let Some(d) = dialog.get() {
            let _ = d.show_modal();
        }
    };
    let keeps_options = saved_options.is_some();
    let unchanged = {
        let (saved_options, saved_env) = (saved_options.clone(), saved_env.clone());
        move || {
            env_text.with(|t| *t == saved_env)
                && options.with(|t| Some(t) == saved_options.as_ref())
        }
    };
    let save_id = id.clone();
    let save = move |_| {
        let options = keeps_options.then(|| options.get());
        refused.set(None);
        saving.set(Saving::Sent);
        actions.set_engine_settings(save_id.clone(), options, env_text.get());
    };
    let options_field = if keeps_options {
        view! {
            <label for=options_id.clone()>"Engine options (user_args), one per line"</label>
            <textarea
                id=options_id
                class="code text"
                rows=option_rows.to_string()
                spellcheck="false"
                placeholder="none set by hand, e.g. --threads 8"
                prop:value=move || options.get()
                on:input=move |ev| options.set(as_lines(event_target_value(&ev)))
            >
                {saved_options.clone().unwrap_or_default()}
            </textarea>
        }
        .into_any()
    } else {
        view! {
            <p class="muted small">
                "Engine options set by hand need a newer Strata: update Strata."
            </p>
        }
        .into_any()
    };
    view! {
        <button
            type="button"
            class="secondary"
            disabled=move || blocked.get()
            on:click=open
        >
            "Engine settings"
        </button>
        <dialog class="confirm wide engine-settings" node_ref=dialog aria-labelledby=labelled_by>
            <h2 id=title_id>{title}</h2>
            <div class="stack">
                <div>{options_field}</div>
                <div>
                    <label for=env_id.clone()>"Engine environment (env), one NAME=value per line"</label>
                    <textarea
                        id=env_id
                        class="code text"
                        rows=env_rows.to_string()
                        spellcheck="false"
                        placeholder="none set, e.g. STRATA_PIN_BY_NEED=1"
                        prop:value=move || env_text.get()
                        on:input=move |ev| env_text.set(env_as_lines(event_target_value(&ev)))
                    >
                        {saved_env.clone()}
                    </textarea>
                    <p class="muted small">
                        "Setup sets its own variables again when it runs. Variables whose name ends in KEY, TOKEN, SECRET or PASSWORD stay on the PC and do not show here."
                    </p>
                </div>
            </div>
            {started_line.map(|text| view! { <p class="muted small started-args">{text}</p> })}
            {preset_note}
            {active
                .then(|| {
                    view! {
                        <p class="muted">
                            "Strata restarts with them once no request is running. Requests wait meanwhile. If it does not start, the old ones come back."
                        </p>
                    }
                })}
            {move || refused.get().map(|e| view! { <p class="error" role="alert">{e}</p> })}
            <div class="actions">
                <button type="button" class="secondary" on:click=move |_| close()>
                    "Cancel"
                </button>
                <button
                    type="button"
                    disabled=move || blocked.get() || unchanged() || saving.get() != Saving::Idle
                    on:click=save
                >
                    {if active { "Save and restart" } else { "Save" }}
                </button>
            </div>
        </dialog>
    }
}

/// Where a save of *Engine settings* is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Saving {
    Idle,
    Sent,
    Pending,
}

/// The engine environment as its field holds it: one `NAME=value` per line.
fn env_lines(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The environment field's text, a pasted JSON object of strings turned
/// into lines.
fn env_as_lines(text: String) -> String {
    if !text.trim_start().starts_with('{') {
        return text;
    }
    match serde_json::from_str::<BTreeMap<String, String>>(&text) {
        Ok(env) => env_lines(&env),
        Err(_) => text,
    }
}

/// The field's text, a pasted JSON list of strings turned into lines.
fn as_lines(text: String) -> String {
    if !text.trim_start().starts_with('[') {
        return text;
    }
    match serde_json::from_str::<Vec<String>>(&text) {
        Ok(args) => option_lines(&args, '\n'),
        Err(_) => text,
    }
}

/// Whether Strata starts the model with this preset, so that it wins over
/// the run config: active, and not set aside after a new graphics card or
/// model file (see [`preset_text`]).
fn applied(preset: &StrataPreset) -> bool {
    preset.active && preset.state != Some(PresetState::Hard)
}

/// Args with each option and its values, options apart by `between`; a
/// value that is empty or holds a space or a quote is quoted.
fn option_lines(args: &[String], between: char) -> String {
    let mut out = String::new();
    for arg in args {
        if arg.starts_with("--") {
            if !out.is_empty() {
                out.push(between);
            }
        } else if !out.is_empty() {
            out.push(' ');
        }
        if arg.is_empty() || arg.contains(char::is_whitespace) || arg.contains('"') {
            out.push('"');
            out.push_str(&arg.replace('\\', "\\\\").replace('"', "\\\""));
            out.push('"');
        } else {
            out.push_str(arg);
        }
    }
    out
}

/// The optimization prompt of one model: a button, and a dialog with the
/// text and *Copy*. Asked for when opened, so it is never older than the
/// proxy's data; works while a job runs and while the PC sleeps.
#[component]
fn OptimizationPrompt(model: String) -> impl IntoView {
    let dialog = NodeRef::<Dialog>::new();
    let fetch = ServerAction::<GetOptimizationPrompt>::new();
    let title_id = format!("prompt-title-{model}");
    let labelled_by = title_id.clone();
    let text_id = format!("prompt-{model}");
    let title = format!("Optimization prompt for {model}");
    let open = move |_| {
        fetch.dispatch(GetOptimizationPrompt {
            model: model.clone(),
        });
        if let Some(d) = dialog.get() {
            let _ = d.show_modal();
        }
    };
    let body = move || {
        if fetch.pending().get() {
            return view! { <p class="hint">"Loading …"</p> }.into_any();
        }
        match fetch.value().get() {
            None => ().into_any(),
            Some(Ok(text)) => {
                view! { <CopyText id=text_id.clone() label="Prompt" text /> }.into_any()
            }
            Some(Err(e)) => view! { <p class="error" role="alert">{error_text(&e)}</p> }.into_any(),
        }
    };
    view! {
        <button type="button" class="secondary" on:click=open>
            "Optimization prompt"
        </button>
        <dialog class="confirm wide" node_ref=dialog aria-labelledby=labelled_by>
            <h2 id=title_id>{title}</h2>
            <p class="muted">
                "Paste it into an AI chat to get faster settings for this model on your PC. "
                "It holds no keys, paths or addresses."
            </p>
            {body}
            <div class="actions">
                <button
                    type="button"
                    class="secondary"
                    on:click=move |_| {
                        if let Some(d) = dialog.get() {
                            d.close();
                        }
                    }
                >
                    "Close"
                </button>
            </div>
        </dialog>
    }
}

/// `Last tune (quick, coding): 31.2 → 35.1 tok/s with --kv-grow; not applied`.
fn last_tune_text(tune: &StrataTune) -> String {
    let mode = match tune.mode {
        TuneMode::Quick => "quick",
        TuneMode::Thorough => "thorough",
    };
    let usage = match tune.usage {
        TuneUse::Coding => "coding",
        TuneUse::Chat => "chat",
    };
    let found = match (tune.code.as_str(), tune.base_tps, tune.tuned_tps) {
        ("faster", Some(base), Some(tuned)) if tune.changed.is_empty() => {
            format!("{base:.1} → {tuned:.1} tok/s")
        }
        ("faster", Some(base), Some(tuned)) => format!(
            "{base:.1} → {tuned:.1} tok/s with {}",
            tune.changed.join(", ")
        ),
        ("already_fastest", ..) => "your current settings are already the fastest".into(),
        _ => tune
            .message
            .clone()
            .unwrap_or_else(|| format!("no result ({})", tune.code)),
    };
    format!("Last tune ({mode}, {usage}): {found}")
}

/// The model's preset line: the preset Strata starts it with and Strata's
/// verdict on it, one that waits for *Apply*, or none for its context.
fn preset_text(presets: &StrataPresets) -> Option<String> {
    let Some(preset) = &presets.latest else {
        return presets
            .other_context
            .then(|| "No tuning for this context".into());
    };
    let name = match preset.gain_pct {
        Some(gain) => format!("{} (+{gain:.0}%)", preset.name),
        None => preset.name.clone(),
    };
    let reasons = preset.reasons.join(", ");
    Some(match (preset.active, preset.state) {
        (false, _) => format!("Preset {name}: not applied"),
        (true, Some(PresetState::Hard)) => {
            format!("Preset {name}: not applied, because {reasons}; tune again")
        }
        (true, Some(PresetState::Soft)) => {
            format!("Preset {name}: active; re-check offered, because {reasons}")
        }
        (true, _) => format!("Preset {name}: active"),
    })
}

/// *Apply* for a preset that is not active, *Undo* for the active one;
/// either restarts Strata when the model is the one running.
#[component]
fn PresetButton(
    model: String,
    preset: StrataPreset,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    let undo = preset.active;
    let question = if undo {
        Question {
            title: format!("Undo the preset for {model}?"),
            text: format!(
                "Strata goes back to the settings it ran {model} with before \"{}\". \
                 If {model} is running, Strata restarts.",
                preset.name
            ),
            confirm: "Undo",
            danger: false,
        }
    } else {
        Question {
            title: format!("Apply the preset for {model}?"),
            text: format!(
                "Strata starts {model} with the tuned settings of \"{}\"; Undo brings the \
                 old ones back. If {model} is running, Strata restarts.",
                preset.name
            ),
            confirm: "Apply",
            danger: false,
        }
    };
    let confirm = expect_context::<Confirm>();
    let id = preset.id;
    view! {
        <button
            type="button"
            class="secondary"
            disabled=move || blocked.get()
            on:click=move |_| {
                let (model, id) = (model.clone(), id.clone());
                confirm.ask(question.clone(), move || actions.preset(model.clone(), id.clone(), undo));
            }
        >
            {if undo { "Undo preset" } else { "Apply preset" }}
        </button>
    }
}

/// *Turn off* for the active preset: Strata goes back to its standard
/// settings, the preset stays saved; restarts Strata when the model runs.
#[component]
fn PresetOff(
    model: String,
    preset: StrataPreset,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    let question = Question {
        title: format!("Turn the preset off for {model}?"),
        text: format!(
            "Strata starts {model} with its standard settings again; \"{}\" stays saved \
             and can be applied later. If {model} is running, Strata restarts.",
            preset.name
        ),
        confirm: "Turn off",
        danger: false,
    };
    let confirm = expect_context::<Confirm>();
    view! {
        <button
            type="button"
            class="secondary"
            disabled=move || blocked.get()
            on:click=move |_| {
                let model = model.clone();
                confirm.ask(question.clone(), move || actions.preset_off(model.clone()));
            }
        >
            "Turn off preset"
        </button>
    }
}

/// *Tune* for one model: a dialog with how long and for what, then Strata's
/// tuner runs as a job with its progress above. Nothing it finds is applied.
#[component]
fn Tune(model: String, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let dialog = NodeRef::<Dialog>::new();
    let mode = RwSignal::new(TuneMode::Quick);
    let usage = RwSignal::new(TuneUse::Coding);
    let title_id = format!("tune-title-{model}");
    let labelled_by = title_id.clone();
    let mode_id = format!("tune-mode-{model}");
    let use_id = format!("tune-use-{model}");
    let title = format!("Tune {model}");
    let close = move || {
        if let Some(d) = dialog.get() {
            d.close();
        }
    };
    let start = move |_| {
        actions.tune(model.clone(), mode.get(), usage.get());
        close();
    };
    view! {
        <button
            type="button"
            class="secondary"
            disabled=move || blocked.get()
            on:click=move |_| {
                if let Some(d) = dialog.get() {
                    let _ = d.show_modal();
                }
            }
        >
            "Tune"
        </button>
        <dialog class="confirm" node_ref=dialog aria-labelledby=labelled_by>
            <h2 id=title_id>{title}</h2>
            <p class="muted">
                "Strata measures settings for this model on your PC and keeps the fastest. "
                "It never tries settings that change answers. Strata is off while it runs; "
                "requests get \"Strata is tuning\" until it ends. Nothing is applied on its own."
            </p>
            <div class="stack">
                <div>
                    <label for=mode_id.clone()>"How long"</label>
                    <select
                        id=mode_id
                        on:change=move |ev| {
                            mode.set(
                                if event_target_value(&ev) == "thorough" {
                                    TuneMode::Thorough
                                } else {
                                    TuneMode::Quick
                                },
                            )
                        }
                    >
                        <option value="quick" selected>"Quick (about 20 min)"</option>
                        <option value="thorough">"Thorough (up to 2 hours)"</option>
                    </select>
                </div>
                <div>
                    <label for=use_id.clone()>"For"</label>
                    <select
                        id=use_id
                        on:change=move |ev| {
                            usage.set(
                                if event_target_value(&ev) == "chat" {
                                    TuneUse::Chat
                                } else {
                                    TuneUse::Coding
                                },
                            )
                        }
                    >
                        <option value="coding" selected>"Coding (long prompts count too)"</option>
                        <option value="chat">"Chat (answer speed only)"</option>
                    </select>
                </div>
            </div>
            <div class="actions">
                <button type="button" class="secondary" on:click=move |_| close()>
                    "Cancel"
                </button>
                <button type="button" disabled=move || blocked.get() on:click=start>
                    "Start tuning"
                </button>
            </div>
        </dialog>
    }
}

/// What a request without sampling fields gets, with and without thinking.
fn sampling_lines(sampling: &StrataSampling) -> impl IntoView + use<> {
    if sampling.support == SamplingSupport::None {
        return view! {
            <p class="muted small">"This Strata version ignores sampling; update it."</p>
        }
        .into_any();
    }
    let line = |label: &str, fields: &[SamplingField]| {
        let text = if fields.is_empty() {
            "greedy".to_string()
        } else {
            fields
                .iter()
                .map(sampling_text)
                .collect::<Vec<_>>()
                .join(" · ")
        };
        view! { <p class="muted small">{format!("{label}: {text}")}</p> }
    };
    view! {
        {line("With thinking", &sampling.thinking)}
        {line("Without thinking", &sampling.instruct)}
    }
    .into_any()
}

/// `temperature 0.6 (config)`; a value from the model card is unmarked.
fn sampling_text(field: &SamplingField) -> String {
    let source = match field.source {
        SamplingSource::ModelCard => "",
        SamplingSource::Config => " (config)",
        SamplingSource::ChatSettings => " (Chat settings)",
    };
    format!("{} {}{source}", field.name, field.value)
}

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
            match (c == current, c == default_context) {
                (true, true) => text.push_str(" (current, recommended)"),
                (true, false) => text.push_str(" (current)"),
                (false, true) => text.push_str(" (recommended)"),
                (false, false) => {}
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
        <div class="inline-form">
            <label for=picker.clone() class="muted small">
                "Context"
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
fn Version(
    info: StrataInfo,
    update: Option<StrataUpdate>,
    /// The PC sleeps: the update wakes it first.
    asleep: bool,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    let confirm = expect_context::<Confirm>();
    let text = if asleep {
        "The PC wakes up first. Strata then restarts with the new version and cannot answer requests for a moment."
    } else {
        "Strata restarts with the new version and cannot answer requests for a moment."
    };
    let version = info
        .version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let (badge, class) = match info.update_available {
        Some(true) => ("Update available", "badge is-warn"),
        Some(false) => ("Up to date", "badge is-ok"),
        None => ("Not known", "badge is-idle"),
    };
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
            {update
                .filter(|_| info.update_available == Some(true))
                .map(|update| view! { <Changes update actions blocked /> })}
            <div class="actions">
                <button
                    type="button"
                    disabled=move || blocked.get()
                    on:click=move |_| {
                        confirm
                            .ask(
                                Question {
                                    title: "Update Strata?".into(),
                                    text: text.into(),
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
        <TerminalSwitch info=info.clone() asleep actions blocked />
        <Benchmarks info actions blocked />
    }
}

/// The way from the old Strata folder to the Strata Terminal: the switch
/// while Strata runs from the old folder, then removing the old folder
/// once the Terminal's Strata has served. Nothing once that is done.
#[component]
fn TerminalSwitch(
    info: StrataInfo,
    /// The PC sleeps: the job wakes it first.
    asleep: bool,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    if !info.old_strata_folder {
        return None;
    }
    let confirm = expect_context::<Confirm>();
    let wakes = if asleep {
        "The PC wakes up first. "
    } else {
        ""
    };
    let on_terminal = info.install == Some(StrataInstall::Terminal);
    let (text, button, question, kind) = if info.install == Some(StrataInstall::Source) {
        let model = info
            .models
            .iter()
            .find(|m| m.active)
            .map_or("the active model".to_string(), |m| m.id.clone());
        (
            "Strata runs from the old Strata folder. Strata itself now installs and runs through the Strata Terminal.",
            "Switch to Strata Terminal",
            Question {
                title: "Switch to the Strata Terminal?".into(),
                text: format!(
                    "{wakes}The PC service installs the Strata Terminal and takes over {model} with its context, speed projection and sampling; model files already in the Strata data folder are not downloaded again. Strata cannot answer requests while it restarts. If the switch fails, the old Strata folder serves again."
                ),
                confirm: "Switch to Strata Terminal",
                danger: false,
            },
            StrataJobKind::SwitchToTerminal,
        )
    } else if on_terminal && info.terminal_served {
        (
            "Strata runs on the Strata Terminal. The old Strata folder is no longer needed.",
            "Remove old Strata folder",
            Question {
                title: "Remove the old Strata folder?".into(),
                text: format!(
                    "{wakes}The PC service deletes the old Strata folder. The Strata data folder with the models stays."
                ),
                confirm: "Remove old Strata folder",
                danger: true,
            },
            StrataJobKind::RemoveOldStrataFolder,
        )
    } else {
        return None;
    };
    let class = if question.danger { "ghost-danger" } else { "" };
    Some(view! {
        <section class="card">
            <h2>"Strata Terminal"</h2>
            <p class="muted">{text}</p>
            <div class="actions">
                <button
                    type="button"
                    class=class
                    disabled=move || blocked.get()
                    on:click=move |_| confirm.ask(question.clone(), move || actions.job(kind))
                >
                    {button}
                </button>
            </div>
        </section>
    })
}

/// How often the card asks again while a code benchmark runs.
#[cfg(feature = "hydrate")]
const CODE_BENCH_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// The code benchmark: start, progress and each model's last result.
#[component]
fn CodeBenchCard() -> impl IntoView {
    let start = ServerAction::<StartCodeBench>::new();
    let start_all = ServerAction::<StartCodeBenchAll>::new();
    let cancel = ServerAction::<CancelCodeBench>::new();
    let tick = RwSignal::new(0u32);
    let page = Resource::new(
        move || {
            (
                tick.get(),
                start.version().get(),
                start_all.version().get(),
                cancel.version().get(),
            )
        },
        |_| get_code_bench(),
    );
    let running = Memo::new(move |_| {
        page.get().and_then(|p| p.ok()).is_some_and(|p| {
            // A run over all models stays live while it goes, so a task
            // between two models still shows as running.
            p.all.is_some()
                || p.run
                    .as_ref()
                    .is_some_and(|r| r.state == CodeBenchState::Running)
        })
    });
    #[cfg(feature = "hydrate")]
    {
        let ticker = set_interval_with_handle(
            move || {
                if running.get_untracked() {
                    tick.update(|n| *n += 1);
                }
            },
            CODE_BENCH_POLL,
        )
        .ok();
        on_cleanup(move || {
            if let Some(t) = ticker {
                t.clear();
            }
        });
    }
    let confirm = expect_context::<Confirm>();
    let run = move |tasks: usize, minutes: u64| {
        confirm.ask(
            Question {
                title: "Run the code benchmark?".into(),
                text: format!(
                    "The active model solves {tasks} tasks with opencode, at most {} in all. The PC wakes with the first request; other requests keep working meanwhile.",
                    hours_text(minutes)
                ),
                confirm: "Run code benchmark",
                danger: false,
            },
            move || {
                start.dispatch(StartCodeBench {});
            },
        );
    };
    let run_all = move |models: usize, tasks: usize, minutes: u64| {
        confirm.ask(
            Question {
                title: "Benchmark all models?".into(),
                text: format!(
                    "{models} models, {tasks} tasks each, at most {} per model and {} in all. Requests of other clients wait until the run ends, at most as long as for a wake-up, and then fail. The model active before the run comes back at the end.",
                    hours_text(minutes),
                    hours_text(models as u64 * minutes),
                ),
                confirm: "Benchmark all models",
                danger: false,
            },
            move || {
                start_all.dispatch(StartCodeBenchAll {});
            },
        );
    };
    view! {
        <section class="card">
            <h2>"Code benchmark"</h2>
            <p class="muted small">
                "The active model solves three Python tasks and a real feature of this repo in Rust with opencode in the code-bench container on the NAS. Hidden tests score what it leaves."
            </p>
            {action_error(start.value())}
            {action_error(start_all.value())}
            {action_error(cancel.value())}
            <Transition>
                {move || Suspend::new(async move {
                    match page.await {
                        Err(e) => view! { <p class="error">{error_text(&e)}</p> }.into_any(),
                        Ok(p) => {
                            let problem = p
                                .problem
                                .clone()
                                .map(|text| view! { <p class="warning" role="status">{text}</p> });
                            let blocked = p.problem.is_some();
                            let all = p.all.clone();
                            // While a model loads there is no run to cancel
                            // below, so the run over all models has its own.
                            let between = p
                                .run
                                .as_ref()
                                .is_none_or(|r| r.state != CodeBenchState::Running);
                            let progress = all.map(|a| {
                                let cancel_all = between.then(|| view! {
                                    <div class="actions">
                                        <button
                                            type="button"
                                            class="secondary"
                                            disabled=move || cancel.pending().get()
                                            on:click=move |_| {
                                                confirm
                                                    .ask(
                                                        Question {
                                                            title: "Cancel the run over all models?".into(),
                                                            text: "The models still to come do not start. The finished results stay and the model active before the run comes back."
                                                                .into(),
                                                            confirm: "Cancel",
                                                            danger: true,
                                                        },
                                                        move || {
                                                            cancel.dispatch(CancelCodeBench {});
                                                        },
                                                    );
                                            }
                                        >
                                            "Cancel"
                                        </button>
                                    </div>
                                });
                                view! {
                                    <div class="job" role="status">
                                        <p class="strong">{if a.task == 0 {
                                            format!("Model {} of {} · loading", a.model, a.models)
                                        } else {
                                            format!(
                                                "Model {} of {} · task {} of {}",
                                                a.model, a.models, a.task, a.tasks
                                            )
                                        }}</p>
                                        <p class="muted small">{format!(
                                            "The model active before the run, {}, comes back at the end.", a.restore
                                        )}</p>
                                        {cancel_all}
                                    </div>
                                }
                            });
                            let in_all = p.all.is_some();
                            let current = p
                                .run
                                .clone()
                                .map(|r| view! { <CodeBenchCurrent run=r cancel in_all /> });
                            // Sorted by the proxy: tests passed, then time.
                            let results = p.results.clone();
                            let results = if results.is_empty() {
                                view! { <p>"No code benchmark has run yet."</p> }.into_any()
                            } else {
                                results
                                    .into_iter()
                                    .map(|r| view! { <CodeBenchResult run=r tasks=p.tasks.clone() /> })
                                    .collect_view()
                                    .into_any()
                            };
                            let models = p.models;
                            let tasks = p.tasks.len();
                            // The limits of one model's run, all tasks running out of time.
                            let minutes = p
                                .tasks
                                .iter()
                                .map(|t| t.limit_secs)
                                .sum::<u64>()
                                .div_ceil(60)
                                .max(1);
                            view! {
                                {problem}
                                {progress}
                                {current}
                                {results}
                                <div class="actions">
                                    <button
                                        type="button"
                                        class="secondary"
                                        disabled=move || {
                                            blocked || running.get() || start.pending().get()
                                        }
                                        on:click=move |_| run(tasks, minutes)
                                    >
                                        "Run code benchmark"
                                    </button>
                                    <button
                                        type="button"
                                        class="secondary"
                                        disabled=move || {
                                            blocked
                                                || running.get()
                                                || start_all.pending().get()
                                                || models < 2
                                        }
                                        on:click=move |_| run_all(models, tasks, minutes)
                                    >
                                        "Benchmark all models"
                                    </button>
                                </div>
                            }
                                .into_any()
                        }
                    }
                })}
            </Transition>
        </section>
    }
}

/// `90` → `1:30 h`, `45` → `45 min`.
fn hours_text(minutes: u64) -> String {
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{}:{:02} h", minutes / 60, minutes % 60)
    }
}

/// Hidden tests passed and seconds in total.
fn run_totals(run: &CodeBenchRun) -> (u32, f64) {
    (
        run.tasks.iter().map(|t| t.passed).sum(),
        run.tasks.iter().filter_map(|t| t.total_secs).sum(),
    )
}

/// `754.2` → `12:34 min`, `42.5` → `43 s`.
fn duration_text(secs: f64) -> String {
    let secs = secs.round() as u64;
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{}:{:02} min", secs / 60, secs % 60)
    }
}

/// The running run, or the last one that ended without a result.
#[component]
fn CodeBenchCurrent(
    run: CodeBenchRun,
    cancel: ServerAction<CancelCodeBench>,
    /// Part of a run over all models: Cancel stops the models still to come too.
    in_all: bool,
) -> impl IntoView {
    let confirm = expect_context::<Confirm>();
    match run.state {
        CodeBenchState::Running => {
            let at = run
                .tasks
                .iter()
                .position(|t| t.state == "running")
                .unwrap_or(0);
            let task = run.tasks.get(at).cloned();
            let mut figures = vec![format!("Task {} of {}", at + 1, run.tasks.len())];
            if let Some(t) = &task {
                figures.push(t.title.clone());
                figures.push(format!("{} model steps", t.steps));
                if let Some(secs) = t.total_secs {
                    figures.push(duration_text(secs));
                }
            }
            view! {
                <div class="job" role="status">
                    <p class="strong">{format!("Running with {}", run.model)}</p>
                    <p class="hint">{figures.join(" · ")}</p>
                    <div class="actions">
                        <button
                            type="button"
                            class="secondary"
                            disabled=move || cancel.pending().get()
                            on:click=move |_| {
                                confirm
                                    .ask(
                                        Question {
                                            title: "Cancel the code benchmark?".into(),
                                            text: if in_all {
                                                "The task that runs now ends, the other tasks and models do not start. The finished results stay and the model active before the run comes back."
                                            } else {
                                                "The task that runs now ends, the others do not start."
                                            }
                                            .into(),
                                            confirm: "Cancel",
                                            danger: true,
                                        },
                                        move || {
                                            cancel.dispatch(CancelCodeBench {});
                                        },
                                    );
                            }
                        >
                            "Cancel code benchmark"
                        </button>
                    </div>
                </div>
            }
                .into_any()
        }
        CodeBenchState::Cancelled => view! {
            <p class="hint">{format!("The last run with {} was cancelled.", run.model)}</p>
        }
        .into_any(),
        // A done run is among the results.
        CodeBenchState::Done => ().into_any(),
        CodeBenchState::Failed => {
            let why = run
                .error
                .clone()
                .unwrap_or_else(|| "no reason given".into());
            view! {
                <p class="error">{format!("The last run with {} failed: {why}", run.model)}</p>
            }
            .into_any()
        }
    }
}

/// A model's last finished run: the total, then each task. A run from
/// before a task was added lacks it: its row shows "–" and the total says so.
#[component]
fn CodeBenchResult(run: CodeBenchRun, tasks: Vec<CodeBenchTaskInfo>) -> impl IntoView {
    let (passed, secs) = run_totals(&run);
    let total: u32 = run.tasks.iter().map(|t| t.total).sum();
    let output: u64 = run.tasks.iter().map(|t| t.output_tokens).sum();
    // The time the model spent, from each task's speed.
    let model_secs: f64 = run
        .tasks
        .iter()
        .filter_map(|t| Some(t.output_tokens as f64 / t.output_tps.filter(|v| *v > 0.0)?))
        .sum();
    let mut summary = vec![
        format!("{passed}/{total} tests"),
        duration_text(secs),
        format!("{output} output tokens"),
    ];
    if model_secs > 0.0 {
        summary.push(tps((output as f64 / model_secs * 10.0).round() / 10.0));
    }
    let missing: Vec<CodeBenchTaskInfo> = tasks
        .into_iter()
        .filter(|t| !run.tasks.iter().any(|r| r.name == t.name))
        .collect();
    if !missing.is_empty() {
        summary.insert(
            0,
            format!(
                "{} of {} tasks",
                run.tasks.len(),
                run.tasks.len() + missing.len()
            ),
        );
    }
    // An error row has no tasks, so no totals either.
    let totals = (!run.tasks.is_empty())
        .then(|| view! { <p>{format!("Total: {}", summary.join(" · "))}</p> });
    let missing = (!run.tasks.is_empty()).then(|| {
        missing
            .into_iter()
            .map(|t| {
                view! {
                    <li>
                        <div class="grow">
                            <p class="strong">{t.title}</p>
                            <p class="muted small">"Not part of this run."</p>
                        </div>
                        <span class="badge">"–"</span>
                    </li>
                }
            })
            .collect_view()
    });
    let tasks = run
        .tasks
        .into_iter()
        .map(|t| view! { <CodeBenchTaskRow task=t /> })
        .collect_view();
    let failed = run
        .error
        .clone()
        .map(|e| view! { <p class="error">{format!("{}: {e}", run.model)}</p> });
    // A later run that failed left this result standing.
    let failed_later = run.failed_later.clone().map(|e| {
        view! {
            <p class="error small">
                {format!("A later run failed ({}): {e}", run.failed_at_text)}
            </p>
        }
    });
    view! {
        <div class="bench-run">
            <h3>{run.model.clone()}</h3>
            {failed}
            {failed_later}
            {totals}
            <p class="muted small">{run.at_text.clone()}</p>
            <ul class="list">{tasks}{missing}</ul>
        </div>
    }
}

#[component]
fn CodeBenchTaskRow(task: CodeBenchTask) -> impl IntoView {
    let mut figures = vec![format!("{}/{} tests", task.passed, task.total)];
    if let Some(secs) = task.total_secs {
        figures.push(duration_text(secs));
    }
    figures.push(format!("{} steps", task.steps));
    figures.push(format!(
        "{} in / {} out tokens",
        task.input_tokens, task.output_tokens
    ));
    if let Some(v) = task.output_tps {
        figures.push(tps(v));
    }
    let class = if task.total > 0 && task.passed == task.total {
        "badge is-ok"
    } else if task.passed > 0 {
        "badge is-warn"
    } else {
        "badge is-bad"
    };
    let badge = if task.timed_out {
        "Time limit".to_string()
    } else {
        format!("{}/{}", task.passed, task.total)
    };
    let files = task
        .files
        .iter()
        .map(|(path, text)| {
            view! {
                <p class="small strong">{path.clone()}</p>
                <pre class="lines">{text.clone()}</pre>
            }
        })
        .collect_view();
    let repo = task.kind == "repo";
    let no_files = (task.files.is_empty() && !repo)
        .then(|| view! { <p class="hint">"The agent left no Python file."</p> });
    let diff = repo.then(
        || match task.diff.clone().filter(|d| !d.trim().is_empty()) {
            Some(diff) => view! {
                <p class="small strong">"Change against the checkout"</p>
                <pre class="lines">{diff}</pre>
            }
            .into_any(),
            None => view! { <p class="hint">"The agent changed nothing."</p> }.into_any(),
        },
    );
    // Tests that passed before the change and fail after it.
    let broken = (!task.broken.is_empty()).then(|| {
        view! {
            <p class="error small">
                {match task.broken.len() {
                    1 => "Broken: 1 earlier test fails now".to_string(),
                    n => format!("Broken: {n} earlier tests fail now"),
                }}
            </p>
        }
    });
    let broken_list = (!task.broken.is_empty()).then(|| {
        view! {
            <p class="small strong">"Broken tests"</p>
            <pre class="lines">{task.broken.join("\n")}</pre>
        }
    });
    view! {
        <li>
            <div class="grow">
                <p class="strong">{task.title.clone()}</p>
                <p class="muted small">{figures.join(" · ")}</p>
                {task.error.clone().map(|e| view! { <p class="error small">{e}</p> })}
                {broken}
                <details class="advanced">
                    <summary>"Code and test output"</summary>
                    {files}
                    {no_files}
                    {diff}
                    {broken_list}
                    <p class="small strong">"Hidden tests"</p>
                    <pre class="lines">{task.test_output.clone()}</pre>
                </details>
            </div>
            <span class=class>
                <span class="dot"></span>
                {badge}
            </span>
        </li>
    }
}

/// About how long one benchmark takes, as the PC service says.
const BENCH_MINUTES: usize = 10;

/// Each model's last benchmark, fastest first, and the buttons that run one
/// or compare them all.
#[component]
fn Benchmarks(info: StrataInfo, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let confirm = expect_context::<Confirm>();
    let mut rows: Vec<StrataBenchmark> = info.benchmarks.clone();
    // An older PC service keeps only the last run.
    if rows.is_empty() {
        rows.extend(info.benchmark.clone());
    }
    rows.sort_by(|a, b| {
        b.decode_tps
            .unwrap_or(-1.0)
            .total_cmp(&a.decode_tps.unwrap_or(-1.0))
            .then_with(|| a.model.cmp(&b.model))
    });
    let active = info.models.iter().find(|m| m.active).map(|m| m.id.clone());
    let installed: Vec<String> = info.models.iter().map(|m| m.id.clone()).collect();
    // On the Strata Terminal, results from the old folder's BENCH.bat
    // measured differently: they are marked and never the fastest.
    let terminal = info.install == Some(StrataInstall::Terminal);
    let comparable =
        move |row: &StrataBenchmark| !terminal || row.tool == Some(BenchTool::StrataBench);
    let marked = move |row: &StrataBenchmark| row.decode_tps.is_some() && !comparable(row);
    let older = rows.iter().any(marked);
    let fastest = rows
        .iter()
        .find(|r| comparable(r))
        .filter(|r| r.decode_tps.is_some())
        .map(|r| r.model.clone());
    let list = if rows.is_empty() {
        view! { <p>"No benchmark has run yet."</p> }.into_any()
    } else {
        let items = rows
            .into_iter()
            .map(|row| {
                let is_active = active.as_deref() == Some(row.model.as_str());
                let is_fastest = fastest.as_deref() == Some(row.model.as_str());
                let can_activate =
                    !is_active && row.decode_tps.is_some() && installed.contains(&row.model);
                let older = marked(&row);
                view! { <BenchRow row is_active is_fastest can_activate older actions blocked /> }
            })
            .collect_view();
        view! { <ul class="list">{items}</ul> }.into_any()
    };
    let runs = info.models.len();
    let compare = move |_| {
        let question = Question {
            title: "Compare all models?".into(),
            text: format!(
                "{runs} runs, about {} minutes. Strata switches between the models, and requests wait meanwhile. The active model comes back at the end.",
                runs * BENCH_MINUTES
            ),
            confirm: "Compare models",
            danger: false,
        };
        confirm.ask(question, move || actions.job(StrataJobKind::CompareModels));
    };
    view! {
        <section class="card">
            <h2>"Benchmark"</h2>
            {list}
            {older
                .then(|| {
                    view! {
                        <p class="muted small">
                            "Results marked BENCH.bat came from the old Strata folder. Strata's standard benchmark measures differently, so they are not comparable; run the benchmark again."
                        </p>
                    }
                })}
            <p class="muted small">
                "Run benchmark measures the active model. Requests wait while it runs."
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="secondary"
                    disabled=move || blocked.get()
                    on:click=move |_| actions.job(StrataJobKind::Benchmark)
                >
                    "Run benchmark"
                </button>
                <button
                    type="button"
                    class="secondary"
                    disabled=move || blocked.get() || runs < 2
                    on:click=compare
                >
                    "Compare models"
                </button>
            </div>
        </section>
    }
}

/// How often the Strata page asks again while the update summary is written.
#[cfg(feature = "hydrate")]
const SUMMARY_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// What an update brings: the summary, then the commits it is based on.
#[component]
fn Changes(update: StrataUpdate, actions: Actions, blocked: Signal<bool>) -> impl IntoView {
    let (lines, note, by_strata, can_generate) = match update.summary {
        UpdateSummary::Written { lines, model } => {
            (lines, format!("Summary by Strata ({model})"), true, false)
        }
        UpdateSummary::Writing { fallback } => (
            fallback,
            "Strata is writing a summary…".to_string(),
            false,
            false,
        ),
        UpdateSummary::Unavailable { fallback, reason } => (
            fallback,
            format!(
                "Summary not available: {}",
                reason.as_deref().unwrap_or("Strata isn't ready")
            ),
            false,
            true,
        ),
    };
    let generate = can_generate.then(|| {
        let summarize = actions.summarize;
        let why_not = update.blocked.clone();
        let refused = why_not.is_some();
        let hint = (update.wakes_pc && !refused)
            .then(|| view! { <span class="muted small">"Wakes the PC"</span> });
        view! {
            <div class="actions">
                <button
                    type="button"
                    class="secondary"
                    disabled=move || refused || blocked.get() || summarize.pending().get()
                    on:click=move |_| {
                        summarize.dispatch(GenerateUpdateSummary {});
                    }
                >
                    "Generate summary now"
                </button>
                {hint}
            </div>
            {why_not.map(|w| view! { <p class="muted small">{w}</p> })}
        }
    });
    let count = update.commits.len() as u64 + update.left_out;
    let more = (update.left_out > 0).then(|| format!("and {} more", update.left_out));
    view! {
        <div class="update-changes">
            {if by_strata {
                view! {
                    <ul class="update-summary">
                        {lines.into_iter().map(|l| view! { <li>{l}</li> }).collect_view()}
                    </ul>
                }
                    .into_any()
            } else {
                lines.into_iter().map(|l| view! { <p>{l}</p> }).collect_view().into_any()
            }}
            <p class="muted small">{note}</p>
            {generate}
            {(!update.commits.is_empty())
                .then(|| {
                    view! {
                        <details class="update-commits">
                            <summary>
                                {format!("Show {count} change{}", if count == 1 { "" } else { "s" })}
                            </summary>
                            <ul>
                                {update
                                    .commits
                                    .into_iter()
                                    .map(|c| {
                                        let short = match c.url {
                                            Some(url) => {
                                                view! {
                                                    <a href=url target="_blank" rel="noopener noreferrer">
                                                        <code>{c.short}</code>
                                                    </a>
                                                }
                                                    .into_any()
                                            }
                                            None => view! { <code>{c.short}</code> }.into_any(),
                                        };
                                        view! {
                                            <li>
                                                {short}
                                                " "
                                                <span>{c.subject}</span>
                                                " "
                                                <span class="muted small">{c.date_text}</span>
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ul>
                            {more.map(|m| view! { <p class="muted small">{m}</p> })}
                        </details>
                    }
                })}
            {update
                .compare_url
                .map(|url| {
                    view! {
                        <p class="small">
                            <a href=url target="_blank" rel="noopener noreferrer">
                                "Compare on GitHub"
                            </a>
                        </p>
                    }
                })}
        </div>
    }
}

/// `38.5` → `38.5 tok/s`.
fn tps(v: f64) -> String {
    format!("{v} tok/s")
}

#[component]
fn BenchRow(
    row: StrataBenchmark,
    is_active: bool,
    is_fastest: bool,
    can_activate: bool,
    /// Measured with the old folder's BENCH.bat, not comparable.
    older: bool,
    actions: Actions,
    blocked: Signal<bool>,
) -> impl IntoView {
    // A failed run keeps the model's last measurement, if it had one.
    let speeds = match (&row.error, row.decode_tps) {
        (Some(error), None) => {
            view! { <p class="error small">{format!("No result: {error}")}</p> }.into_any()
        }
        (error, Some(decode)) => {
            let mut text = format!("Decode {}", tps(decode));
            text.push_str(&match row.prompt_tps {
                Some(prompt) => format!(" · Prompt {}", tps(prompt)),
                None => " · Prompt –".into(),
            });
            let failed = error
                .clone()
                .map(|e| view! { <p class="error small">{format!("Last run failed: {e}")}</p> });
            view! {
                <p>{text}</p>
                {failed}
            }
            .into_any()
        }
        (None, None) => view! { <p>{row.result.clone()}</p> }.into_any(),
    };
    let mut with = Vec::new();
    if let Some(context) = row.context {
        with.push(format!("{}K context", context / 1024));
    }
    match row.speed_projection {
        Some(true) => with.push("speed projection on".into()),
        Some(false) => with.push("speed projection off".into()),
        None => {}
    }
    if let Some(version) = &row.version {
        with.push(format!("Strata {version}"));
    }
    if older {
        with.push("BENCH.bat, not comparable".into());
    }
    with.push(row.at_text.clone());
    let id = row.model.clone();
    view! {
        <li>
            <div class="grow">
                <p class="strong">{row.model.clone()}</p>
                {speeds}
                <p class="muted small">{with.join(" · ")}</p>
            </div>
            {is_fastest
                .then(|| {
                    view! {
                        <span class="badge is-ok">
                            <span class="dot"></span>
                            "Fastest"
                        </span>
                    }
                })}
            {is_active
                .then(|| {
                    view! {
                        <span class="badge is-idle">
                            <span class="dot"></span>
                            "Active"
                        </span>
                    }
                })}
            {can_activate
                .then(|| {
                    view! {
                        <button
                            type="button"
                            class="secondary"
                            disabled=move || blocked.get()
                            on:click=move |_| actions.model_job(StrataJobKind::Activate, id.clone())
                        >
                            "Make active"
                        </button>
                    }
                })}
        </li>
    }
}

#[component]
fn StrataKey(unreported: bool) -> impl IntoView {
    view! {
        <section class="card">
            <h2>"Strata key"</h2>
            <p class="muted">
                "Only the proxy and Strata know it. The PC service makes a new one at every start of Strata, and the proxy takes it over by itself. Clients use their own API keys under Access."
            </p>
            {unreported.then(|| view! { <p class="warning" role="status">{TOO_OLD_FOR_KEY}</p> })}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_time_counts_minutes_then_hours() {
        assert_eq!(running_text(59), "0m");
        assert_eq!(running_text(12 * 60 + 30), "12m");
        assert_eq!(running_text(65 * 60), "1h 05m");
    }

    #[test]
    fn engine_options_go_one_per_line_with_odd_values_quoted() {
        let args: Vec<String> = [
            "--threads",
            "8",
            "--x",
            "a b",
            "",
            "say \"hi\"",
            "--no-mmap",
        ]
        .map(Into::into)
        .into();
        assert_eq!(
            option_lines(&args, '\n'),
            "--threads 8\n--x \"a b\" \"\" \"say \\\"hi\\\"\"\n--no-mmap"
        );
        assert_eq!(option_lines(&[], '\n'), "");
        let path: Vec<String> = ["--x", "C:\\My Models\\"].map(Into::into).into();
        assert_eq!(option_lines(&path, '\n'), "--x \"C:\\\\My Models\\\\\"");
    }

    #[test]
    fn a_pasted_json_list_becomes_lines() {
        assert_eq!(
            as_lines(r#" ["--threads", "8", "--x", "a b"]"#.into()),
            "--threads 8\n--x \"a b\""
        );
        assert_eq!(as_lines("[\"--threads\", ".into()), "[\"--threads\", ");
        assert_eq!(as_lines("--threads 8".into()), "--threads 8");
    }

    #[test]
    fn a_pasted_json_object_becomes_environment_lines() {
        assert_eq!(
            env_as_lines(r#" {"STRATA_X": "1", "A": "b c=d"}"#.into()),
            "A=b c=d\nSTRATA_X=1"
        );
        assert_eq!(env_as_lines("{\"X\": 1}".into()), "{\"X\": 1}");
        assert_eq!(env_as_lines("X=1".into()), "X=1");
    }

    fn speed(label: &str, tok_s: f64) -> TuneSpeed {
        TuneSpeed {
            label: label.into(),
            tok_s,
        }
    }

    #[test]
    fn a_tunes_speeds_name_the_current_settings_and_mark_the_fastest() {
        let rows = speed_rows(&[
            speed("base", 171.4),
            speed("Keep more of the conversation in graphics memory", 172.2),
            speed("Run the control thread on the last CPU core", 168.0),
        ]);
        assert_eq!(
            rows,
            [
                "Your current settings: 171.4 tok/s",
                "Keep more of the conversation in graphics memory: 172.2 tok/s (+0.5 %) · fastest so far",
                "Run the control thread on the last CPU core: 168.0 tok/s (-2.0 %)",
            ]
        );
        // The current settings alone are no leader yet.
        assert_eq!(
            speed_rows(&[speed("base", 171.4)]),
            ["Your current settings: 171.4 tok/s"]
        );
        // Without them, nothing to compare with.
        assert_eq!(
            speed_rows(&[
                speed("Shorter speculative window", 30.0),
                speed("Longer", 29.0)
            ]),
            [
                "Shorter speculative window: 30.0 tok/s · fastest so far",
                "Longer: 29.0 tok/s"
            ]
        );
        // Of equally fast ones, the first leads.
        assert_eq!(
            speed_rows(&[speed("base", 171.4), speed("Longer", 171.4)]),
            [
                "Your current settings: 171.4 tok/s · fastest so far",
                "Longer: 171.4 tok/s (+0.0 %)"
            ]
        );
        assert!(speed_rows(&[]).is_empty());
    }

    fn presets(active: bool, state: Option<PresetState>, reasons: &[&str]) -> StrataPresets {
        StrataPresets {
            latest: Some(StrataPreset {
                id: "3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6".into(),
                name: "IQ3_XXS 64K – Full speed".into(),
                active,
                state,
                reasons: reasons.iter().map(|r| r.to_string()).collect(),
                gain_pct: Some(12.4),
                settings: vec!["--kv-resident 65536".into()],
                decode_tps: Some(35.1),
                base_tps: Some(31.2),
            }),
            other_context: false,
        }
    }

    #[test]
    fn the_preset_line_says_each_state() {
        let line = |p: &StrataPresets| preset_text(p).unwrap();
        assert_eq!(
            line(&presets(true, Some(PresetState::Fresh), &[])),
            "Preset IQ3_XXS 64K – Full speed (+12%): active"
        );
        assert_eq!(
            line(&presets(
                true,
                Some(PresetState::Soft),
                &["the graphics driver changed"]
            )),
            "Preset IQ3_XXS 64K – Full speed (+12%): active; re-check offered, because the graphics driver changed"
        );
        assert_eq!(
            line(&presets(
                true,
                Some(PresetState::Hard),
                &["the graphics card changed", "the model file changed"]
            )),
            "Preset IQ3_XXS 64K – Full speed (+12%): not applied, because the graphics card changed, the model file changed; tune again"
        );
        assert_eq!(
            line(&presets(false, None, &[])),
            "Preset IQ3_XXS 64K – Full speed (+12%): not applied"
        );
    }

    #[test]
    fn after_a_context_switch_nothing_is_tuned_for_it() {
        let other = StrataPresets {
            latest: None,
            other_context: true,
        };
        assert_eq!(preset_text(&other).unwrap(), "No tuning for this context");
        let never = StrataPresets {
            latest: None,
            other_context: false,
        };
        assert_eq!(preset_text(&never), None);
    }
}
