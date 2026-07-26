//! Frontend-serving integration tests (ticket #2): the embedded SPA + API + WS
//! share one origin (ADR-0007). These drive the real app over HTTP and adapt to
//! whether `client/dist` has been built — so they pass in CI both before and
//! after the frontend build step.

mod common;
use common::{TestApp, header_of};

use radio_scout::web;

/// `/` returns HTML — the built SPA shell when embedded, else the backend page.
#[tokio::test]
async fn serves_frontend_at_root() {
    let app = TestApp::spawn().await;

    let resp = app.get("/").await;

    assert_eq!(resp.status(), 200);
    let content_type = header_of(&resp, "content-type").unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "got {content_type:?}"
    );

    let body = resp.text().await.expect("body");
    if web::spa_is_embedded() {
        assert!(body.contains("id=\"root\""), "SPA shell served at /");
    } else {
        assert!(body.contains("Radio-Scout"), "backend fallback served at /");
    }
}

/// Deep links to client-side routes fall back to the SPA shell so the router can
/// take over — a hard refresh on `/talkgroups` must not 404.
#[tokio::test]
async fn spa_fallback_serves_shell_for_client_routes() {
    let app = TestApp::spawn().await;

    let resp = app.get("/talkgroups").await;

    assert_eq!(resp.status(), 200);
    // Always serves an HTML document (built shell or the backend fallback), so
    // the router can take over — asserted even before the SPA is built.
    let content_type = header_of(&resp, "content-type").unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "got {content_type:?}"
    );
    if web::spa_is_embedded() {
        let body = resp.text().await.expect("body");
        assert!(body.contains("id=\"root\""), "client route -> SPA shell");
    }
}

/// The SPA fallback must not shadow the API/health namespace: unknown `/api/*`
/// stays a clean 404 rather than returning the HTML shell.
#[tokio::test]
async fn api_namespace_is_not_shadowed_by_spa_fallback() {
    let app = TestApp::spawn().await;

    let unknown = app.get("/api/does-not-exist").await;
    assert_eq!(unknown.status(), 404, "unknown /api route must 404");
    let body = unknown.text().await.unwrap_or_default();
    assert!(
        !body.contains("id=\"root\""),
        "must not serve the SPA for /api/*"
    );

    assert_eq!(app.get("/api/call/999999/audio").await.status(), 404);
}

/// Content-hashed assets are served with a long-lived immutable cache header.
#[tokio::test]
async fn serves_hashed_asset_with_immutable_cache() {
    if !web::spa_is_embedded() {
        return; // nothing to serve until the SPA is built
    }
    let app = TestApp::spawn().await;

    let index = app.get("/").await.text().await.expect("index body");
    let asset = index
        .split('"')
        .find(|token| token.starts_with("/assets/"))
        .expect("an /assets/ reference in index.html");

    let resp = app.get(asset).await;
    assert_eq!(resp.status(), 200, "asset {asset} served");

    let cache = header_of(&resp, "cache-control").unwrap_or_default();
    assert!(
        cache.contains("immutable"),
        "hashed asset cached; got {cache:?}"
    );
}
