//! The frame around the pages: routes, login, header and navigation.

use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{A, Outlet, ParentRoute, Route, Router, Routes},
    path,
};

use crate::{
    access::AccessPage,
    api::{ChangePassword, Login, Logout, Session, error_text, get_session},
    icons::Icon,
    labels::{pc_badge, strata_badge},
    overview::{Live, OverviewPage},
    pc_setup::PcPage,
    settings::SystemPage,
    strata::StrataPage,
    strata_output::StrataOutputPage,
};

/// The HTML page around the app.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
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
pub struct SessionActions {
    login: ServerAction<Login>,
    pub logout: ServerAction<Logout>,
    pub change: ServerAction<ChangePassword>,
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
    view! {
        <Stylesheet id="admin" href="/admin/pkg/admin-ui.css" />
        <Title text="Strata Proxy" />
        <Router>
            <Routes fallback=NotFound>
                <ParentRoute path=path!("/admin") view=Layout>
                    <Route path=path!("") view=OverviewPage />
                    <Route path=path!("pc") view=PcPage />
                    <Route path=path!("strata") view=StrataPage />
                    <Route path=path!("strata/output") view=StrataOutputPage />
                    <Route path=path!("access") view=AccessPage />
                    <Route path=path!("system") view=SystemPage />
                </ParentRoute>
            </Routes>
        </Router>
    }
}

/// The current session, fetched again after login, logout and password change.
#[derive(Clone, Copy)]
struct SessionResource(Resource<Result<Session, ServerFnError>>);

/// Every page under `/admin`: the frame for a logged-in user with an own
/// password; otherwise the login or the "set a new password" form.
#[component]
fn Layout() -> impl IntoView {
    let session = expect_context::<SessionResource>().0;
    view! {
        <Transition fallback=|| view! { <p class="auth">"Loading …"</p> }>
            {move || Suspend::new(async move {
                match session.await {
                    Ok(Session::Ready) => view! { <Frame /> }.into_any(),
                    Ok(Session::MustChange) => view! { <FirstPassword /> }.into_any(),
                    Ok(Session::Anonymous) => view! { <LoginForm /> }.into_any(),
                    Err(e) => {
                        view! {
                            <AuthCard>
                                <p class="error">{error_text(&e)}</p>
                            </AuthCard>
                        }
                            .into_any()
                    }
                }
            })}
        </Transition>
    }
}

/// Header with the live state, navigation, and the page. The overview stream
/// is opened here once and shared with the pages.
#[component]
fn Frame() -> impl IntoView {
    let live = Live::follow();
    provide_context(live);
    let pc = move || live.overview.with(|o| o.as_ref().map(|o| pc_badge(o.pc)));
    let strata = move || live.overview.with(|o| o.as_ref().map(strata_badge));
    view! {
        <div class="shell">
            <header class="top">
                <a href="/admin" class="brand">
                    <Brand />
                </a>
                <div class="badges" aria-live="polite">
                    <StateBadge what="PC" state=Signal::derive(pc) />
                    <StateBadge what="Strata" state=Signal::derive(strata) />
                </div>
            </header>
            <nav class="side" aria-label="Main">
                <NavItem href="/admin" icon=Icon::House label="Overview" exact=true />
                <NavItem href="/admin/pc" icon=Icon::Monitor label="PC" />
                <NavItem href="/admin/strata" icon=Icon::Layers label="Strata" />
                <NavItem href="/admin/access" icon=Icon::KeyRound label="Access" />
                <NavItem href="/admin/system" icon=Icon::Sliders label="System" />
            </nav>
            <main class="content">
                <Outlet />
            </main>
        </div>
    }
}

#[component]
fn Brand() -> impl IntoView {
    view! {
        <span class="brand-mark">
            <Icon icon=Icon::Layers size=18 />
        </span>
        <span class="brand-name">"Strata Proxy"</span>
    }
}

/// "PC · Awake" as a colored pill that leads to the overview; "…" until the
/// first overview arrived.
#[component]
fn StateBadge(
    what: &'static str,
    state: Signal<Option<(&'static str, &'static str)>>,
) -> impl IntoView {
    let text = move || format!("{what} · {}", state.get().map_or("…", |(text, _)| text));
    let class = move || {
        format!(
            "badge {}",
            state.get().map_or("is-idle", |(_, class)| class)
        )
    };
    view! {
        <a href="/admin" class=class>
            <span class="dot"></span>
            {text}
        </a>
    }
}

#[component]
fn NavItem(
    href: &'static str,
    icon: Icon,
    label: &'static str,
    #[prop(optional)] exact: bool,
) -> impl IntoView {
    view! {
        <A href=href exact=exact attr:class="nav-item">
            <Icon icon=icon size=22 />
            <span>{label}</span>
        </A>
    }
}

/// A card on its own, for the pages before login.
#[component]
fn AuthCard(children: Children) -> impl IntoView {
    view! {
        <div class="auth">
            <div class="card auth-card">
                <div class="brand">
                    <Brand />
                </div>
                {children()}
            </div>
        </div>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <AuthCard>
            <h1>"Not found"</h1>
            <p class="hint">"There is no page at this address."</p>
            <a class="button" href="/admin">
                "Go to the overview"
            </a>
        </AuthCard>
    }
}

/// Shows the error of an action's last run, if any.
pub fn action_error(
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
        <AuthCard>
            <h1>"Sign in"</h1>
            <ActionForm action=login attr:class="stack">
                <div>
                    <label for="password">"Password"</label>
                    <input
                        id="password"
                        type="password"
                        name="password"
                        autocomplete="current-password"
                        required
                    />
                </div>
                {action_error(login.value())}
                <button type="submit" class="full">
                    "Sign in"
                </button>
            </ActionForm>
        </AuthCard>
    }
}

#[component]
pub fn PasswordFields() -> impl IntoView {
    view! {
        <div>
            <label for="current">"Current password"</label>
            <input id="current" type="password" name="current" autocomplete="current-password" required />
        </div>
        <div>
            <label for="new">"New password"</label>
            <input id="new" type="password" name="new" autocomplete="new-password" minlength="8" required />
            <p class="hint">"At least 8 characters."</p>
        </div>
        <div>
            <label for="repeat">"Repeat new password"</label>
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
        <AuthCard>
            <h1>"Set a new password"</h1>
            <p class="warning">
                "You are signed in with the default password. Set your own before you continue."
            </p>
            <ActionForm action=change attr:class="stack">
                <PasswordFields />
                {action_error(change.value())}
                <button type="submit" class="full">
                    "Save password"
                </button>
            </ActionForm>
        </AuthCard>
    }
}
