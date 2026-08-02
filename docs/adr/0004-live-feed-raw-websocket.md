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
- `{"t":"sub","sel":{sysRef:{tgRef|"*":bool}},"all":bool,"since":callId?}` — replace the subscription matrix, optionally with a reconnect catch-up cursor. See **the matrix** below for how `sel`, `"*"`, and `all` resolve.

**The matrix (#11 refinement).** `sel` resolves most-specific-first: an explicit `sel[system][talkgroup]` wins, then the System's wildcard `sel[system]["*"]`, then `all`. Two client behaviors need that ordering, and rdio only sidesteps both by shipping its entire Talkgroup config to the client and enumerating every id:

- **Hold System** (spec US 11) narrows to the System that's talking, but the client only knows the Talkgroups it has *heard* — so it sends `sel[system]["*"] = true` rather than a list it can't complete.
- **Avoid** (spec US 14) mutes one Talkgroup out of a selection that starts all-on, so `sel[system][talkgroup] = false` has to be an *exception* to `all` (or to a hold), not something they overrule.

Exclusions alone still count as "all off" — a matrix of nothing but `false` selects nothing, and the fanout skips patch resolution for it. Both refinements are backward compatible: a client that sends neither wildcards nor `false` entries behaves exactly as before.

**Matching** mirrors rdio's `IsEnabled` (a Call reaches a subscriber of its own **or** any patched Talkgroup, same-System), and adds an **access-scope** gate: delivery requires *both* the subscription matrix and the connection's access scope to admit the (System, Talkgroup). v1 listening is open, so every connection's scope is `All`; the restricted scope (`[{system, talkgroups}]`) is the v2 access-code seam ([ADR-0008](0008-security-posture.md)), built and unit-tested now.

**Heartbeat + reconnect.** The server pings on an interval and **reaps a connection one unanswered ping later** — a ping goes out on the first tick after activity, and if no pong (or any frame) arrives by the next tick the peer is dropped (~two missed intervals). rdio has no heartbeat of its own and leaves half-open connections lingering. Per-connection state is stateless-until-subscribed, so a reconnecting client just re-sends its matrix.

**Reconnect catch-up** — a bounded refinement of "history is client-side." rdio drops any Call that arrives while a listener is briefly disconnected (backgrounded tab, network blip) — the exact mobile pain Radio-Scout exists to fix. On (re)subscribe the client sends the last Call id it saw as `since`; the server takes the newest `CATCHUP_MAX_CALLS` Calls with `id > since`, filters them through the same matrix+scope logic as the live path, and sends the survivors oldest-first (flagged `catchup`) before resuming live. The cap is applied **before** filtering (newest-N-by-id across all Systems), so this is a **best-effort recent slice**, not a completeness guarantee: for a *brief* reconnect the gap is well under the cap and every matching missed Call is delivered; for a *long* gap the client gets the recent slice and, since it always holds `since`, can archive-search (#13) the remainder itself. This is deliberately **not** a server-side history browser — Hold/avoid/queue/replay/full history stay client state, and completeness for large gaps is the client's archive-search job, not the live socket's. Delivery is **at-least-once** — a Call ingested in the narrow window between connect and the catch-up query can arrive both via catch-up and live — so the client dedups by (unique, monotonic) Call id, which it already does to drive replay/history. Server-side id dedup is deliberately avoided: concurrent ingests can broadcast out of id order, so a high-water mark would wrongly drop Calls.

## Consequences

- Fanout is initially "iterate connected clients, check each matrix" — fine at our scale (low hundreds of listeners). If scale ever demands, add an internal `(systemRef,talkgroupRef) → subscribers` index — a data-structure change on the same transport, not a protocol change.
- The optimization the user wanted ("only receive selected talkgroups") is delivered by this server-side filtering, independent of any Socket.IO room feature.
- Catch-up adds a bounded DB read on (re)subscribe (only when a `since` cursor is present). N+1 `stored_call` builds over a capped candidate set are acceptable at v1 Pi scale; a batched builder is a later optimization if it ever shows up in a profile.
- This is a **foreground** transport. It cannot fix delivery to a suspended/backgrounded iOS tab; background behavior is handled separately in the PWA/Media Session design ([ADR-0005](0005-client-audio-media-session-background.md)) — Web Push covers the fully-suspended case, catch-up covers the brief-reconnect case.

## Amendment (#86, 2026-08-02): the Backfill is batched, and the profile it was waiting on was arithmetic

The consequence above deferred this explicitly: "N+1 `stored_call` builds over a capped candidate set are acceptable at v1 Pi scale; a batched builder is a later optimization if it ever shows up in a profile." Two things about that turned out to be wrong, and #84's architecture review named the second.

**The batched builder already existed.** `repo::stored_calls` was written for `GET /api/calls` (#13) and sat directly beside the single-Call form the Backfill was looping. So this was never a builder to write and weigh — it was a call site pointed at the wrong one of two functions in the same module, and the "optimization" was deleting a loop. The cost of *not* doing it was six hundred-odd round-trips per reconnect; the cost of doing it was smaller than the cost of writing this paragraph.

**It could not show up in a profile, because nothing profiles a Pi with Postgres over a network.** The bound is a hundred Calls and each one cost seven statements — one to re-read the row the loop already held, six to denormalize it. On the SQLite-on-a-Mac loop everything is developed on, seven hundred local statements are milliseconds and invisible. The deployment that pays for it is the one that is never measured: a Raspberry Pi with a Postgres over a socket, on every network blip and every phone unlock. "If it ever shows up in a profile" was a deferral to a profile nobody was ever going to run, which is a decision not to do it.

What replaced the guess is an assertion. The suite counts the statements an Instance issues (the #97 statement seam, counting rather than refusing) and holds a Backfill of twenty Calls to costing exactly what a Backfill of two does — so this cannot regress quietly the way it arrived. Two sizes rather than a pinned number, because what has to hold is that the cost does not grow per Call, and a pinned constant would make a legitimately added query read as this regression.

Nothing observable changed: the same bound, the same ordering, the same truncation flag, the same at-least-once delivery, the same per-connection filtering. The one behavioural difference is a failure that used to be silent — a Call whose view failed to build was dropped without a line, which is the shape of bug an operator can only ever report as "some calls go missing sometimes." It now logs, like the query above it (ADR-0011 rule 3).

The single-Call denormalizer is gone entirely. Both its callers — this one and ingest's — already held the row it re-fetched by id, so what it really offered was the opportunity to write the loop.
