# Live feed over raw WebSocket with server-side filtering

## Context

Radio-Scout needs a real-time channel to push call metadata to listeners and receive their subscription and auth messages. rdio-scanner's WebSocket API is proprietary, so we design our own ([ADR-0001](0001-ingest-compatible-own-live-feed-protocol.md)). Two facts shrink the problem: audio is fetched over HTTP, not the socket ([ADR-0002](0002-audio-object-storage.md)), so the channel carries only small JSON; and per-connection server state is minimal — just the subscription matrix (`system→talkgroup→bool`) plus the access scope. Hold, avoid, queue, replay, and history are all client-side.

## Decision

Use a **raw WebSocket** via Axum, with a compact JSON message protocol, over a single bidirectional connection on the same HTTP port. The client persists its selection in LocalStorage and sends the subscription matrix on connect and on every change; the server stores it per connection. When a call is ingested, the server pushes it **only** to connections whose subscription matrix **and** access scope match, honoring patches (a call reaches subscribers of any patched talkgroup). Reconnect and heartbeat are implemented directly.

## Considered and rejected

- **Socket.IO (`socketioxide`)** — its "rooms" are only an indexing optimization, and they don't cleanly express our access-scope and patch filter dimensions. Not worth the engine.io overhead and heavier client dependency for modern-browser-only clients.
- **SSE + HTTP POST** — robust and proxy-friendly, but two mechanisms where one bidirectional connection suffices.
- **WebTransport (HTTP/3/QUIC)** — its strengths (lossy datagrams, massive multiplexing) don't apply to our tiny-reliable-message workload; iOS Safari support is bleeding-edge/uncertain (our #1 platform); and it mandates QUIC/UDP/TLS infrastructure that fights the "simple install" goal.

## Protocol and #9 refinements (improvements over rdio, not a clone)

The concrete wire protocol (all `{t, …}` JSON; audio never rides the socket). rdio's live feed is the reference for *what*, not the ceiling for *how well* — #9 deliberately fixes several of its weaknesses:

**Server → client**
- `{"t":"hello","protocol":N,"heartbeatMs":M}` — sent on connect. rdio has no such handshake; announcing the protocol version and heartbeat cadence lets the client negotiate and time its own reconnect logic.
- `{"t":"subscribed"}` — ack that a `sub` is applied (so a subscribe can't race an ingest).
- `{"t":"call","call":{…}}` — a live Call; `{"t":"call","call":{…},"catchup":true}` — a replayed one (see catch-up below). The Call view carries `patches[]` so the client can show cross-patched traffic.
- `{"t":"lagged","skipped":N}` — a slow client fell behind the fanout; it's told how many Calls it missed so it can refetch from the archive (#13). rdio silently drops them.

**Client → server**
- `{"t":"sub","sel":{sysRef:{tgRef:bool}},"all":bool,"since":callId?}` — replace the subscription matrix, optionally with a reconnect catch-up cursor.

**Matching** mirrors rdio's `IsEnabled` (a Call reaches a subscriber of its own **or** any patched Talkgroup, same-System), and adds an **access-scope** gate: delivery requires *both* the subscription matrix and the connection's access scope to admit the (System, Talkgroup). v1 listening is open, so every connection's scope is `All`; the restricted scope (`[{system, talkgroups}]`) is the v2 access-code seam ([ADR-0008](0008-security-posture.md)), built and unit-tested now.

**Heartbeat + reconnect.** The server pings on an interval and **reaps a connection one unanswered ping later** — a ping goes out on the first tick after activity, and if no pong (or any frame) arrives by the next tick the peer is dropped (~two missed intervals). rdio has no heartbeat of its own and leaves half-open connections lingering. Per-connection state is stateless-until-subscribed, so a reconnecting client just re-sends its matrix.

**Reconnect catch-up** — a bounded refinement of "history is client-side." rdio drops any Call that arrives while a listener is briefly disconnected (backgrounded tab, network blip) — the exact mobile pain Radio-Scout exists to fix. On (re)subscribe the client sends the last Call id it saw as `since`; the server takes the newest `CATCHUP_MAX_CALLS` Calls with `id > since`, filters them through the same matrix+scope logic as the live path, and sends the survivors oldest-first (flagged `catchup`) before resuming live. The cap is applied **before** filtering (newest-N-by-id across all Systems), so this is a **best-effort recent slice**, not a completeness guarantee: for a *brief* reconnect the gap is well under the cap and every matching missed Call is delivered; for a *long* gap the client gets the recent slice and, since it always holds `since`, can archive-search (#13) the remainder itself. This is deliberately **not** a server-side history browser — Hold/avoid/queue/replay/full history stay client state, and completeness for large gaps is the client's archive-search job, not the live socket's. Delivery is **at-least-once** — a Call ingested in the narrow window between connect and the catch-up query can arrive both via catch-up and live — so the client dedups by (unique, monotonic) Call id, which it already does to drive replay/history. Server-side id dedup is deliberately avoided: concurrent ingests can broadcast out of id order, so a high-water mark would wrongly drop Calls.

## Consequences

- Fanout is initially "iterate connected clients, check each matrix" — fine at our scale (low hundreds of listeners). If scale ever demands, add an internal `(systemRef,talkgroupRef) → subscribers` index — a data-structure change on the same transport, not a protocol change.
- The optimization the user wanted ("only receive selected talkgroups") is delivered by this server-side filtering, independent of any Socket.IO room feature.
- Catch-up adds a bounded DB read on (re)subscribe (only when a `since` cursor is present). N+1 `stored_call` builds over a capped candidate set are acceptable at v1 Pi scale; a batched builder is a later optimization if it ever shows up in a profile.
- This is a **foreground** transport. It cannot fix delivery to a suspended/backgrounded iOS tab; background behavior is handled separately in the PWA/Media Session design ([ADR-0005](0005-client-audio-media-session-background.md)) — Web Push covers the fully-suspended case, catch-up covers the brief-reconnect case.
