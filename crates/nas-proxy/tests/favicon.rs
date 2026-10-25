//! The web UI's icons: the favicon and the home screen icon, served without a
//! login and linked from every page.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;

const FAVICON_LINK: &str = r#"<link rel="icon" href="/admin/favicon.svg" type="image/svg+xml">"#;
const TOUCH_ICON_LINK: &str = r#"<link rel="apple-touch-icon" href="/admin/apple-touch-icon.png">"#;

/// A GET without a session cookie or API key.
async fn get(url: &str) -> reqwest::Response {
    reqwest::get(url).await.unwrap()
}

#[tokio::test]
async fn the_favicon_is_the_brand_mark_without_a_login() {
    let s = setup(false, |_| {}).await;
    let resp = get(&format!("{}/admin/favicon.svg", s.url)).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "image/svg+xml");
    assert_eq!(resp.headers()["cache-control"], "public, max-age=86400");
    let svg = resp.text().await.unwrap();
    assert!(svg.starts_with("<svg"), "{svg}");
    assert!(svg.contains(r##"fill="#6d28d9""##), "{svg}");
    // The Layers icon of the header's brand mark: its top and two lower layers.
    for path in [
        "M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08",
        "M2 12a1 1 0 0 0 .58.91",
        "M2 17a1 1 0 0 0 .58.91",
    ] {
        assert!(svg.contains(path), "{path} missing in {svg}");
    }
}

#[tokio::test]
async fn the_home_screen_icon_is_a_180_pixel_png_without_a_login() {
    let s = setup(false, |_| {}).await;
    let resp = get(&format!("{}/admin/apple-touch-icon.png", s.url)).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "image/png");
    assert_eq!(resp.headers()["cache-control"], "public, max-age=86400");
    let png = resp.bytes().await.unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    // The first chunk is IHDR: length, type, then width and height.
    assert_eq!(&png[12..16], b"IHDR");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (180, 180));
}

#[tokio::test]
async fn every_page_links_both_icons() {
    let s = setup(true, |_| {}).await;
    let login = Browser::new(&s.url).page("/admin").await;
    assert!(login.contains("Sign in"), "{login}");
    let overview = logged_in(&s.url).await.page("/admin").await;
    for (page, html) in [("login", login), ("overview", overview)] {
        assert!(html.contains(FAVICON_LINK), "{page}: {html}");
        assert!(html.contains(TOUCH_ICON_LINK), "{page}: {html}");
    }
}

#[tokio::test]
async fn the_icons_neither_wake_the_pc_nor_reach_strata() {
    let s = setup(false, |_| {}).await;
    for path in ["/admin/favicon.svg", "/admin/apple-touch-icon.png"] {
        assert_eq!(get(&format!("{}{path}", s.url)).await.status(), 200);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_root_favicon_still_needs_an_api_key() {
    let s = setup(false, |_| {}).await;
    assert_eq!(get(&format!("{}/favicon.ico", s.url)).await.status(), 401);
}
