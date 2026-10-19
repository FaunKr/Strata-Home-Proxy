//! The area "PC": the PC service with its updates, the one-line install
//! command, and the settings for reaching, waking and sleeping the PC; the
//! other tab of the area is the Verlauf in `history`.

use leptos::prelude::*;
use leptos_router::components::{A, Outlet};

use crate::{
    api::{NewPcSetupCommand, PcServiceView, PcView, UpdatePcService, error_text},
    copy::CopyField,
    labels::{PC_CONNECTION, PC_WAKE_SLEEP},
    overview::Live,
    settings::SettingsSections,
};

/// The frame of the area: its heading, its tabs and the page inside them.
#[component]
pub fn PcArea() -> impl IntoView {
    view! {
        <div class="page-head">
            <h1>"PC"</h1>
            <p class="lead">"The PC service, and how the proxy wakes your PC and lets it sleep."</p>
        </div>
        <nav class="tabs" aria-label="PC">
            <A href="/admin/pc" exact=true attr:class="tab">
                "PC service"
            </A>
            <A href="/admin/pc/history" attr:class="tab">
                "History"
            </A>
        </nav>
        <Outlet />
    }
}

/// What the PC does on the PC: its service, the command that installs it, and
/// the settings for reaching, waking and sleeping it.
#[component]
pub fn PcSetupTab() -> impl IntoView {
    view! {
        <PcService />
        <Install />
        <SettingsSections sections=&[&PC_CONNECTION, &PC_WAKE_SLEEP] />
    }
}

/// The PC service's version, live, and the update when the NAS carries
/// another one.
#[component]
fn PcService() -> impl IntoView {
    let live = expect_context::<Live>();
    let update = ServerAction::<UpdatePcService>::new();
    let body = move || {
        let Some(o) = live.overview.get() else {
            return view! { <p class="hint">"Loading …"</p> }.into_any();
        };
        match o.pc_service {
            Some(service) => view! { <ServiceState service pc=o.pc update /> }.into_any(),
            None => {
                let (badge, text) = if o.setup.agent_configured {
                    (
                        view! { <span class="badge is-idle"><span class="dot"></span>"Not reached yet"</span> },
                        "The proxy has not heard from it since it started. It reports once the PC is awake.",
                    )
                } else {
                    (
                        view! { <span class="badge is-warn"><span class="dot"></span>"Not installed"</span> },
                        "Without it there is no gaming mode and no automatic sleep. Install it with the command below.",
                    )
                };
                view! {
                    <div class="card-head">
                        <h2>"PC service"</h2>
                        {badge}
                    </div>
                    <p class="muted">{text}</p>
                }
                .into_any()
            }
        }
    };
    view! {
        <section class="card" aria-label="PC service">
            {body}
            {move || {
                update
                    .value()
                    .get()
                    .and_then(Result::err)
                    .map(|e| view! { <p class="error" role="alert">{error_text(&e)}</p> })
            }}
        </section>
    }
}

#[component]
fn ServiceState(
    service: PcServiceView,
    pc: PcView,
    update: ServerAction<UpdatePcService>,
) -> impl IntoView {
    let awake = matches!(pc, PcView::Awake | PcView::Gaming);
    let (badge, class) = if service.updating {
        ("Updating", "badge is-warn")
    } else if service.update_available {
        ("Update available", "badge is-warn")
    } else {
        ("Up to date", "badge is-ok")
    };
    let installed = service
        .version
        .clone()
        .unwrap_or_else(|| "old, reports no version".to_string());
    let available = service.available.clone().unwrap_or_else(|| "–".to_string());
    let reported_mac = service
        .reported_mac
        .clone()
        .unwrap_or_else(|| "–".to_string());
    let target = service
        .available
        .clone()
        .map_or_else(|| "Update".to_string(), |v| format!("Update to {v}"));
    let button = (service.update_available && !service.updating).then(|| {
        view! {
            <div class="actions">
                <button
                    type="button"
                    disabled=move || !awake || update.pending().get()
                    on:click=move |_| {
                        update.dispatch(UpdatePcService {});
                    }
                >
                    {move || match (awake, update.pending().get()) {
                        (false, _) => "Update (PC is asleep)".to_string(),
                        (true, true) => "Updating …".to_string(),
                        (true, false) => target.clone(),
                    }}
                </button>
            </div>
        }
    });
    view! {
        <div class="card-head">
            <h2>"PC service"</h2>
            <span class=class>
                <span class="dot"></span>
                {badge}
            </span>
        </div>
        <p class="muted">"Runs on your PC. It puts the PC to sleep, switches gaming mode and starts Strata."</p>
        <dl class="facts">
            <dt>"Installed"</dt>
            <dd>{installed}</dd>
            <dt>"Available"</dt>
            <dd>{available}</dd>
            <dt>"Reported MAC"</dt>
            <dd class="mono">{reported_mac}</dd>
        </dl>
        {service.updating.then(|| view! { <p class="hint" role="status">"The PC service restarts with the new version …"</p> })}
        {service
            .update_error
            .map(|e| view! { <p class="error" role="alert">{format!("Last update failed: {e}")}</p> })}
        {button}
    }
}

/// The one-line command that installs the PC service and registers it here.
#[component]
fn Install() -> impl IntoView {
    let create = ServerAction::<NewPcSetupCommand>::new();
    let shown = move || {
        create.value().get().map(|result| match result {
            Ok(c) => view! {
                <div class="stack" role="status">
                    <CopyField
                        id="pc-setup-command"
                        label="Paste into PowerShell on the PC"
                        text=c.command
                    />
                    <p class="hint">
                        {format!(
                            "Valid for {} minutes and for one PC. Windows asks once for admin rights. The proxy then enters the PC service's token and address itself.",
                            c.valid_minutes,
                        )}
                    </p>
                    <CopyField
                        id="pc-uninstall-command"
                        label="To remove it from the PC"
                        text=c.uninstall_command
                    />
                </div>
            }
            .into_any(),
            Err(e) => view! { <p class="error" role="alert">{error_text(&e)}</p> }.into_any(),
        })
    };
    view! {
        <section class="card">
            <h2>"Install on a PC"</h2>
            <p class="muted">
                "The command downloads the PC service from the proxy, installs it and registers it here. Running it on a PC that already has it replaces the program and registers the PC again."
            </p>
            {shown}
            <ActionForm action=create attr:class="actions">
                <button type="submit" class="secondary">
                    {move || {
                        if create.value().get().is_some_and(|r| r.is_ok()) {
                            "Create new command"
                        } else {
                            "Create install command"
                        }
                    }}
                </button>
                <span class="hint">"A new command invalidates the previous one."</span>
            </ActionForm>
        </section>
    }
}
