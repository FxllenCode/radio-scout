//! Serves the frontend on the same origin as the API + WebSocket (ADR-0007).
//!
//! The production React SPA (ticket #2) is built to `client/dist/` and embedded
//! into the binary with `rust-embed`. This module serves those assets and falls
//! back to the SPA shell (`index.html`) for client-side routes, so the single
//! binary hosts the whole app. When the SPA hasn't been built yet, it serves a
//! minimal backend-only page so `cargo run` still does something useful.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "client/dist/"]
struct Assets;

/// Whether the production SPA has been built into `client/dist/` and embedded.
pub fn spa_is_embedded() -> bool {
    Assets::get("index.html").is_some()
}

/// The app's catch-all: serve a built asset by path, otherwise the SPA shell for
/// client-side routes. Registered as the router `fallback`, so it only runs when
/// no explicit API/WS/health route matched — and it still refuses to answer for
/// the `api`/`healthz` namespaces, so an unknown `/api/*` stays a clean 404.
pub async fn spa_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Reserved server namespaces never fall through to the SPA. Everything real
    // lives under `/api/*` (covered generically); `healthz` is the one top-level
    // exception. A new top-level route outside `/api` must be added here too, or
    // the SPA shell would shadow it.
    if path == "healthz" || path == "api" || path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }

    if !path.is_empty()
        && let Some(asset) = Assets::get(path)
    {
        return asset_response(path, asset.data.into_owned());
    }

    match Assets::get("index.html") {
        Some(index) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            index.data.into_owned(),
        )
            .into_response(),
        None => Html(FALLBACK_HTML).into_response(),
    }
}

fn asset_response(path: &str, data: Vec<u8>) -> Response {
    let mut response = data.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for(path)),
    );
    // Vite emits content-hashed filenames under assets/ — safe to cache forever.
    if path.starts_with("assets/") {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    response
}

fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Shown at `/` only when the SPA hasn't been built. Proves the backend works
/// standalone: connect the live feed and play calls through an HTML5 `<audio>`
/// element with Media Session metadata.
const FALLBACK_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>Radio-Scout (backend)</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; min-height: 100vh; font: 15px/1.5 system-ui, sans-serif;
    background: #09090b; color: #fafafa; padding: 2rem 1rem; }
  .card { max-width: 32rem; margin: 0 auto; background: #18181b; border: 1px solid #27272a;
    border-radius: 12px; padding: 1.2rem; }
  code { background: #27272a; border-radius: 4px; padding: 1px 5px; font-family: ui-monospace, monospace; }
  audio { width: 100%; margin-top: .8rem; }
  .muted { color: #a1a1aa; font-size: .85rem; }
</style>
</head>
<body>
<div class="card">
  <h1>Radio-Scout</h1>
  <p class="muted">The UI isn't embedded yet. Build it with
    <code>cd client &amp;&amp; npm run build</code>, then rebuild the binary.</p>
  <p class="muted">Meanwhile the backend is live — this page plays incoming calls.</p>
  <div id="now">Waiting for the first call…</div>
  <audio id="player" controls></audio>
</div>
<script>
(function () {
  var player = document.getElementById('player');
  var queue = [], playing = false, ws;
  function connect() {
    ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/api/live');
    ws.onopen = function () { ws.send(JSON.stringify({ t: 'sub', all: true })); };
    ws.onclose = function () { setTimeout(connect, 1000); };
    ws.onmessage = function (ev) {
      try { var m = JSON.parse(ev.data); if (m.t === 'call') { queue.push(m.call); if (!playing) next(); } } catch (e) {}
    };
  }
  function next() {
    var c = queue.shift();
    if (!c) { playing = false; return; }
    playing = true;
    document.getElementById('now').textContent =
      (c.talkgroupTag || c.talkgroupLabel || ('TG ' + c.talkgroupRef)) + ' · ' + (c.systemLabel || ('System ' + c.systemRef));
    player.src = c.audioUrl;
    player.play().catch(function () {});
    if ('mediaSession' in navigator) {
      navigator.mediaSession.metadata = new MediaMetadata({
        title: c.talkgroupTag || ('TG ' + c.talkgroupRef),
        artist: c.systemLabel || ('System ' + c.systemRef), album: 'Radio-Scout' });
    }
  }
  player.onended = next;
  connect();
})();
</script>
</body>
</html>
"#;
