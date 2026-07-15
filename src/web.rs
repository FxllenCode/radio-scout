//! Minimal proof-of-playback client for the walking skeleton (ticket #1).
//!
//! This is a deliberately throwaway, dependency-free page served inline at `/`
//! so the skeleton actually *walks*: run the binary, POST a call, and hear it
//! play in the browser through a single reused HTML5 `<audio>` element with
//! Media Session metadata (ADR-0005). Ticket #2 replaces this route with the
//! real React/Vite/Tailwind PWA embedded via `rust-embed`, and #14/#15 own the
//! production client-audio, prefetch, and background/PWA behavior.

use axum::response::Html;

/// `GET /` — the skeleton client page.
pub async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>Radio-Scout (skeleton)</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body {
    margin: 0; min-height: 100vh; font: 15px/1.5 system-ui, sans-serif;
    background: #0b0f14; color: #e6edf3; padding: env(safe-area-inset-top) 1rem 2rem;
  }
  header { display: flex; align-items: center; gap: .6rem; padding: 1rem 0; }
  .dot { width: .8rem; height: .8rem; border-radius: 50%; background: #f85149; transition: background .2s; }
  .dot.on { background: #3fb950; }
  h1 { font-size: 1.1rem; margin: 0; font-weight: 600; letter-spacing: .02em; }
  .card { background: #11161d; border: 1px solid #222b35; border-radius: 12px; padding: 1rem 1.1rem; margin: .8rem 0; }
  .tg { font-size: 1.35rem; font-weight: 700; }
  .sys { color: #8b98a5; font-size: .9rem; }
  .meta { color: #6e7b8a; font-size: .8rem; margin-top: .35rem; }
  button {
    appearance: none; border: 0; border-radius: 10px; padding: .8rem 1rem; font-size: 1rem;
    font-weight: 600; background: #2f81f7; color: #fff; width: 100%; cursor: pointer;
  }
  audio { width: 100%; margin-top: .8rem; }
  .q { color: #8b98a5; font-size: .8rem; }
  ul { list-style: none; padding: 0; margin: .5rem 0 0; }
  li { padding: .4rem 0; border-top: 1px solid #1b232c; font-size: .85rem; color: #8b98a5; }
</style>
</head>
<body>
<header><span id="dot" class="dot"></span><h1>Radio-Scout <span class="sys">skeleton</span></h1></header>

<button id="start">Start listening</button>

<div class="card">
  <div id="tg" class="tg">Waiting for calls…</div>
  <div id="sys" class="sys"></div>
  <div id="meta" class="meta"></div>
  <audio id="player" controls></audio>
  <div class="q">Queue: <span id="qlen">0</span></div>
</div>

<div class="card">
  <div class="sys">Recent</div>
  <ul id="log"></ul>
</div>

<script>
(function () {
  const player = document.getElementById('player');
  const dot = document.getElementById('dot');
  const qlenEl = document.getElementById('qlen');
  const logEl = document.getElementById('log');
  const queue = [];
  let started = false, playing = false, ws;

  function label(c) { return c.talkgroupTag || c.talkgroupLabel || ('TG ' + c.talkgroupRef); }
  function system(c) { return c.systemLabel || ('System ' + c.systemRef); }

  function connect() {
    ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/api/live');
    ws.onopen = () => { dot.classList.add('on'); ws.send(JSON.stringify({ t: 'sub', all: true })); };
    ws.onclose = () => { dot.classList.remove('on'); setTimeout(connect, 1000); };
    ws.onmessage = (ev) => {
      let msg; try { msg = JSON.parse(ev.data); } catch (e) { return; }
      if (msg.t === 'call') enqueue(msg.call);
    };
  }

  function enqueue(call) {
    queue.push(call);
    qlenEl.textContent = queue.length;
    const li = document.createElement('li');
    li.textContent = label(call) + ' · ' + system(call);
    logEl.prepend(li);
    if (started && !playing) playNext();
  }

  function playNext() {
    const call = queue.shift();
    qlenEl.textContent = queue.length;
    if (!call) { playing = false; return; }
    playing = true;
    document.getElementById('tg').textContent = label(call);
    document.getElementById('sys').textContent = system(call);
    document.getElementById('meta').textContent =
      [call.frequency && (call.frequency / 1e6).toFixed(4) + ' MHz',
       'TGID ' + call.talkgroupRef,
       call.source && 'Unit ' + call.source].filter(Boolean).join(' · ');
    player.src = call.audioUrl;
    player.play().catch(() => {});
    if ('mediaSession' in navigator) {
      navigator.mediaSession.metadata = new MediaMetadata({
        title: label(call), artist: system(call), album: 'Radio-Scout',
      });
    }
  }

  player.onended = () => playNext();

  if ('mediaSession' in navigator) {
    navigator.mediaSession.setActionHandler('play', () => player.play());
    navigator.mediaSession.setActionHandler('pause', () => player.pause());
    navigator.mediaSession.setActionHandler('nexttrack', () => { player.pause(); playNext(); });
  }

  document.getElementById('start').onclick = () => {
    started = true;
    document.getElementById('start').style.display = 'none';
    if (!playing) playNext();
  };

  connect();
})();
</script>
</body>
</html>
"#;
