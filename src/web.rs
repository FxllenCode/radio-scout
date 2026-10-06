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
///
/// **The one handler that still returns a `Response`** (#92), deliberately. Its
/// 404 is a *routing* answer — "that namespace is reserved" — not a refusal with
/// a reason an operator would ever grep for, and routing a `Reason` through here
/// would cover only half the 404s an unrouted URL can get: axum's own fallback
/// for everything outside `/api` never reaches this function at all. A
/// vocabulary that covers half a case is worse than one that declines it.
pub async fn spa_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Reserved server namespaces never fall through to the SPA. Everything real
    // lives under `/api/*` (covered generically); `healthz` and the share
    // surface (#64) are the top-level exceptions. A new top-level route outside
    // `/api` must be added here too, or the SPA shell would shadow it.
    //
    // `s/` and not `s`: the share **page** is `/s` itself and is an explicit
    // route, which wins over this fallback; what this reserves is everything
    // *under* it, so `/s/anything` is a clean 404 rather than the whole app
    // served at a share URL. The trailing slash also keeps `/search`,
    // `/session` and `/settings` — three real client-side routes — out of it.
    //
    // `embed/` for the same reason (#75): `/embed` is the embed page's route,
    // and the app served at `/embed/anything` would be the one surface on this
    // Instance that looks like the embed and is not framable.
    if path == "healthz"
        || path == "api"
        || path.starts_with("api/")
        || path.starts_with("s/")
        || path.starts_with("embed/")
    {
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

/// The **Embed** page (#75): `embed.html`, the second entry the client build
/// emits — a few kilobytes of its own rather than the whole app, because it is
/// loaded by every reader of somebody else's homepage.
///
/// One static document whatever the token: whether the embed exists is the
/// feed's answer, and the page draws it. Without a client build there is no
/// page to serve, so this says so in a sentence — still a page, still framable,
/// so a host framing an Instance whose binary was built without its UI shows
/// that sentence instead of a browser error.
pub fn embed_page() -> Response {
    page_or_sentence(Assets::get("embed.html").map(|page| page.data.into_owned()))
}

/// The page if this binary was built with one, else the sentence — a function
/// of the asset rather than of the build, so both answers are testable in a
/// tree where only one of them can be served.
fn page_or_sentence(page: Option<Vec<u8>>) -> Response {
    match page {
        Some(page) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response(),
        None => Html(EMBED_FALLBACK_HTML).into_response(),
    }
}

/// What the embed route serves from a binary built without the client.
const EMBED_FALLBACK_HTML: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="robots" content="noindex"><title>Radio-Scout</title></head>
<body><p>This scanner's player is not built into this copy of Radio-Scout.</p></body>
</html>
"#;

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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// The embed route always answers with a page (#75): the built one, or —
    /// from a binary built without the client — a sentence saying so, which a
    /// host's frame shows instead of a browser error.
    #[rstest]
    #[case::built(Some(b"<main id=\"embed\"></main>".to_vec()), "id=\"embed\"")]
    #[case::not_built(None, "not built into this copy")]
    #[tokio::test]
    async fn the_embed_route_answers_with_a_page_built_or_not(
        #[case] page: Option<Vec<u8>>,
        #[case] says: &str,
    ) {
        let response = page_or_sentence(page);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        assert!(String::from_utf8_lossy(&body).contains(says));
    }

    /// Every extension the SPA build emits, pinned to the type a browser needs
    /// to see (#83).
    ///
    /// Nothing else in the suite can reach most of these arms: the integration
    /// tests serve whatever `client/dist` happens to contain, so an arm for a
    /// file this build didn't emit — or any arm at all in a checkout where the
    /// SPA was never built — runs unasserted. A stylesheet handed over as
    /// `application/octet-stream` is a blank page in a browser, and a worker or
    /// manifest with the wrong type is silently ignored, so these are the
    /// values, not merely "some string".
    #[rstest]
    #[case::html("index.html", "text/html; charset=utf-8")]
    #[case::js("assets/index-D4f9.js", "text/javascript; charset=utf-8")]
    #[case::mjs("assets/worker.mjs", "text/javascript; charset=utf-8")]
    #[case::css("assets/index-B1c2.css", "text/css; charset=utf-8")]
    #[case::svg("favicon.svg", "image/svg+xml")]
    #[case::json("assets/data.json", "application/json")]
    #[case::webmanifest("manifest.webmanifest", "application/manifest+json")]
    #[case::woff2("assets/geist.woff2", "font/woff2")]
    #[case::woff("assets/geist.woff", "font/woff")]
    #[case::png("icon-192.png", "image/png")]
    #[case::ico("favicon.ico", "image/x-icon")]
    #[case::source_map("assets/index-D4f9.js.map", "application/json")]
    #[case::txt("robots.txt", "text/plain; charset=utf-8")]
    fn content_type_is_pinned_per_extension(#[case] path: &str, #[case] expected: &str) {
        assert_eq!(content_type_for(path), expected, "for {path}");
    }

    /// An extension we don't model, and a file with none at all, both fall back
    /// to the byte-stream type rather than guessing.
    #[rstest]
    #[case::unmodelled("archive.tar.gz", "application/octet-stream")]
    #[case::no_extension("LICENSE", "application/octet-stream")]
    fn unknown_extensions_fall_back_to_octet_stream(#[case] path: &str, #[case] expected: &str) {
        assert_eq!(content_type_for(path), expected, "for {path}");
    }
}
