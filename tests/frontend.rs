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
    // Asserted both ways round, because the two pages are not otherwise
    // distinguishable: the built shell's title carries "Radio-Scout" too, so a
    // `spa_is_embedded` that reported the wrong thing would still have satisfied
    // the fallback branch (#83). Whether the SPA is embedded decides which tests
    // in this file assert anything at all — a predicate that lies quietly
    // disables them.
    if web::spa_is_embedded() {
        assert!(body.contains("id=\"root\""), "SPA shell served at /");
    } else {
        assert!(body.contains("Radio-Scout"), "backend fallback served at /");
        assert!(
            !body.contains("id=\"root\""),
            "the backend fallback is not the SPA shell"
        );
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

    // The namespace root itself, with no trailing slash: `/api` matches neither
    // `api/` prefix nor any route, so it is the one path that reaches the guard
    // through its middle arm (#83). Serving the shell here would tell a client
    // whose base URL lost its path that the API is alive.
    let root = app.get("/api").await;
    assert_eq!(root.status(), 404, "/api is not a client-side route");
    assert!(
        !root.text().await.expect("body").contains("id=\"root\""),
        "must not serve the SPA for /api"
    );

    assert_eq!(app.get("/api/call/999999/audio").await.status(), 404);
}

/// The PWA's two entry points have to survive the trip through `rust-embed`
/// (#15): a manifest the browser will parse only with the right content type,
/// and a worker that must be served as JavaScript from the app root — that path
/// is its scope.
#[tokio::test]
async fn serves_the_pwa_manifest_and_service_worker() {
    if !web::spa_is_embedded() {
        return; // nothing to serve until the SPA is built
    }
    let app = TestApp::spawn().await;

    let manifest = app.get("/manifest.webmanifest").await;
    assert_eq!(manifest.status(), 200);
    assert_eq!(
        header_of(&manifest, "content-type"),
        Some("application/manifest+json"),
        "a manifest served as anything else is ignored by the browser"
    );
    let body = manifest.text().await.expect("manifest body");
    assert!(
        body.contains("\"display\":\"standalone\""),
        "standalone is what iOS requires for Web Push and background audio"
    );

    let worker = app.get("/sw.js").await;
    assert_eq!(worker.status(), 200);
    let content_type = header_of(&worker, "content-type").unwrap_or_default();
    assert!(
        content_type.starts_with("text/javascript"),
        "a worker served as anything else won't register; got {content_type:?}"
    );
    // A worker cached for a year is a deploy that never lands: it is the one
    // file the browser has to be able to re-fetch. Its own precache manifest
    // is what carries the hashes.
    let cache = header_of(&worker, "cache-control").unwrap_or_default();
    assert!(
        !cache.contains("immutable"),
        "the worker must stay refetchable; got {cache:?}"
    );
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
