//! Login and password of the web UI, over HTTP like a browser.

mod common;

use std::sync::Arc;

use common::*;
use strata_nas_proxy::{AppState, Config, api_router};

#[tokio::test]
async fn without_login_only_the_login_page_is_shown() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    let html = b.page("/admin/").await;
    assert!(html.contains("Anmelden"), "{html}");
    assert!(!html.contains("Hier zeigt der Proxy"), "{html}");
    assert_eq!(b.session().await, "Anonymous");
    let resp = change(&mut b, "admin", "ein langes Passwort").await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn first_login_forces_a_new_password() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    let resp = b.call("login", &[("password", "falsch")]).await;
    assert_eq!(resp.status(), 401);
    assert!(resp.text().await.unwrap().contains("Falsches Passwort"));
    assert_eq!(
        b.call("login", &[("password", "admin")]).await.status(),
        200
    );
    assert_eq!(b.session().await, "MustChange");
    let html = b.page("/admin/").await;
    assert!(html.contains("Neues Passwort festlegen"), "{html}");

    let resp = change(&mut b, "admin", "kurz").await;
    assert_ne!(resp.status(), 200);
    assert_eq!(
        change(&mut b, "admin", "ein langes Passwort")
            .await
            .status(),
        200
    );
    assert_eq!(b.session().await, "Ready");
    let html = b.page("/admin/").await;
    assert!(html.contains("Hier zeigt der Proxy"), "{html}");
    assert!(kinds(&s.state).contains(&strata_nas_proxy::events::EventKind::PasswordChanged));

    // The default password is gone.
    let mut other = Browser::new(&s.url);
    assert_ne!(
        other.call("login", &[("password", "admin")]).await.status(),
        200
    );
}

#[tokio::test]
async fn ui_password_replaces_the_default() {
    let s = setup(true, |c| c.ui_password = Some("start-123".into())).await;
    let mut b = Browser::new(&s.url);
    assert_ne!(
        b.call("login", &[("password", "admin")]).await.status(),
        200
    );
    assert_eq!(
        b.call("login", &[("password", "start-123")]).await.status(),
        200
    );
    assert_eq!(b.session().await, "MustChange");
}

#[tokio::test]
async fn repeated_failures_are_slowed_down() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    for _ in 0..5 {
        b.call("login", &[("password", "falsch")]).await;
    }
    let resp = b.call("login", &[("password", "admin")]).await;
    assert_ne!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("Zu viele Fehlversuche"));
}

#[tokio::test]
async fn changing_the_password_ends_other_sessions() {
    let s = setup(true, |_| {}).await;
    let mut first = Browser::new(&s.url);
    first.call("login", &[("password", "admin")]).await;
    change(&mut first, "admin", "erstes Passwort").await;
    let mut second = Browser::new(&s.url);
    second
        .call("login", &[("password", "erstes Passwort")])
        .await;
    assert_eq!(second.session().await, "Ready");

    assert_eq!(
        change(&mut first, "erstes Passwort", "zweites Passwort")
            .await
            .status(),
        200
    );
    assert_eq!(first.session().await, "Ready");
    assert_eq!(second.session().await, "Anonymous");

    // Logging out ends the session also for a copy of the cookie.
    let mut copy = Browser::new(&s.url);
    copy.cookie = first.cookie.clone();
    first.call("logout", &[]).await;
    assert_eq!(first.session().await, "Anonymous");
    assert_eq!(copy.session().await, "Anonymous");
}

#[tokio::test]
async fn wrong_current_password_counts_as_failed_attempt() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    b.call("login", &[("password", "admin")]).await;
    for _ in 0..5 {
        assert_ne!(
            change(&mut b, "geraten", "ein langes Passwort")
                .await
                .status(),
            200
        );
    }
    let resp = change(&mut b, "admin", "ein langes Passwort").await;
    assert!(resp.text().await.unwrap().contains("Zu viele Fehlversuche"));
}

async fn proxy_on(dir: &std::path::Path) -> String {
    let cfg = Config {
        data_dir: Some(dir.to_path_buf()),
        ..Config::default()
    };
    let state: Arc<AppState> = AppState::new(cfg);
    let addr = serve(api_router(state)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn password_and_session_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("strata-login-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut b = Browser::new(&proxy_on(&dir).await);
    b.call("login", &[("password", "admin")]).await;
    change(&mut b, "admin", "dauerhaftes Passwort").await;

    // Same data directory, new proxy: the cookie still works.
    b.base = proxy_on(&dir).await;
    assert_eq!(b.session().await, "Ready");
    let mut fresh = Browser::new(&b.base);
    assert_eq!(
        fresh
            .call("login", &[("password", "dauerhaftes Passwort")])
            .await
            .status(),
        200
    );

    // Deleting the password file brings the default back.
    std::fs::remove_file(dir.join("admin-password.json")).unwrap();
    let mut reset = Browser::new(&proxy_on(&dir).await);
    assert_eq!(
        reset.call("login", &[("password", "admin")]).await.status(),
        200
    );
    assert_eq!(reset.session().await, "MustChange");
    std::fs::remove_dir_all(&dir).unwrap();
}
