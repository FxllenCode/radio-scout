# Live feed over raw WebSocket with server-side filtering

## Context

Radio-Scout needs a real-time channel to push call metadata to listeners and receive their subscription and auth messages. rdio-scanner's WebSocket API is proprietary, so we design our own ([ADR-0001](0001-ingest-compatible-own-live-feed-protocol.md)). Two facts shrink the problem: audio is fetched over HTTP, not the socket ([ADR-0002](0002-audio-object-storage.md)), so the channel carries only small JSON; and per-connection server state is minimal — just the subscription matrix (`system→talkgroup→bool`) plus the access scope. Hold, avoid, queue, replay, and history are all client-side.

## Decision

Use a **raw WebSocket** via Axum, with a compact JSON message protocol, over a single bidirectional connection on the same HTTP port. The client persists its selection in LocalStorage and sends the subscription matrix on connect and on every change; the server stores it per connection. When a call is ingested, the server pushes it **only** to connections whose subscription matrix **and** access scope match, honoring patches (a call reaches subscribers of any patched talkgroup). Reconnect and heartbeat are implemented directly.

## Considered and rejected

- **Socket.IO (`socketioxide`)** — its "rooms" are only an indexing optimization, and they don't cleanly express our access-scope and patch filter dimensions. Not worth the engine.io overhead and heavier client dependency for modern-browser-only clients.
- **SSE + HTTP POST** — robust and proxy-friendly, but two mechanisms where one bidirectional connection suffices.
- **WebTransport (HTTP/3/QUIC)** — its strengths (lossy datagrams, massive multiplexing) don't apply to our tiny-reliable-message workload; iOS Safari support is bleeding-edge/uncertain (our #1 platform); and it mandates QUIC/UDP/TLS infrastructure that fights the "simple install" goal.

## Consequences

- Fanout is initially "iterate connected clients, check each matrix" — fine at our scale (low hundreds of listeners). If scale ever demands, add an internal `(systemRef,talkgroupRef) → subscribers` index — a data-structure change on the same transport, not a protocol change.
- The optimization the user wanted ("only receive selected talkgroups") is delivered by this server-side filtering, independent of any Socket.IO room feature.
- This is a **foreground** transport. It cannot fix delivery to a suspended/backgrounded iOS tab; background behavior is handled separately in the PWA/Media Session design.
