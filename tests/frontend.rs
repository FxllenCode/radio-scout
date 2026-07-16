//! Frontend-serving integration tests (ticket #2): the embedded SPA + API + WS
//! share one origin (ADR-0007). These drive the real app over HTTP and adapt to
//! whether `client/dist` has been built — so they pass in CI both before and
//! after the frontend build step.

use std::sync::Arc;

use radio_scout::{AppState, BlobStore, InMemoryCallRepository, build_app, web};

async fn spawn_app() -> (String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audio = Arc::new(BlobStore::filesystem(tmp.path().join("audio")).expect("blob store"));
    let calls = Arc::new(InMemoryCallRepository::new());
    let app = build_app(AppState::new(audio, calls));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("127.0.0.1:{}", addr.port()), tmp)
}

/// `/` returns HTML — the built SPA shell when embedded, else the backend page.
#[tokio::test]
async fn serves_frontend_at_root() {
    let (addr, _tmp) = spawn_app().await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .send()
        .await
        .expect("get /");

    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
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
    let (addr, _tmp) = spawn_app().await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/talkgroups"))
        .send()
        .await
        .expect("get client route");

    assert_eq!(resp.status().as_u16(), 200);
    // Always serves an HTML document (built shell or the backend fallback), so
    // the router can take over — asserted even before the SPA is built.
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
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
    let (addr, _tmp) = spawn_app().await;
    let client = reqwest::Client::new();

    let unknown = client
        .get(format!("http://{addr}/api/does-not-exist"))
        .send()
        .await
        .expect("get unknown api");
    assert_eq!(
        unknown.status().as_u16(),
        404,
        "unknown /api route must 404"
    );
    let body = unknown.text().await.unwrap_or_default();
    assert!(
        !body.contains("id=\"root\""),
        "must not serve the SPA for /api/*"
    );

    let audio = client
        .get(format!("http://{addr}/api/call/999999/audio"))
        .send()
        .await
        .expect("get missing audio");
    assert_eq!(audio.status().as_u16(), 404);
}

/// Content-hashed assets are served with a long-lived immutable cache header.
#[tokio::test]
async fn serves_hashed_asset_with_immutable_cache() {
    if !web::spa_is_embedded() {
        return; // nothing to serve until the SPA is built
    }
    let (addr, _tmp) = spawn_app().await;
    let client = reqwest::Client::new();

    let index = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .expect("get index")
        .text()
        .await
        .expect("index body");

    let asset = index
        .split('"')
        .find(|token| token.starts_with("/assets/"))
        .expect("an /assets/ reference in index.html");

    let resp = client
        .get(format!("http://{addr}{asset}"))
        .send()
        .await
        .expect("get asset");
    assert_eq!(resp.status().as_u16(), 200, "asset {asset} served");

    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        cache.contains("immutable"),
        "hashed asset cached; got {cache:?}"
    );
}
