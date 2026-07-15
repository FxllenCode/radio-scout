# Spec — Radio-Scout v1

Status: ready-for-agent · Scope: v1 (the core listening product) · Decisions: see `docs/adr/0001`–`0009`, `CONTEXT.md`, `docs/design/brief.md`, `docs/research/`.

## Problem Statement

People who monitor trunked-radio systems (public-safety, aviation, rail, events) currently rely on rdio-scanner to ingest audio from their SDR recorders and listen through a scanner-style web app. It works, but it has real pain: audio is stuffed into the database and streamed as JSON byte arrays over a (now proprietary) WebSocket, the UI is dated, and — most painfully — **it does not work correctly in the background on a phone, especially iOS**: audio stops, and there are no working lock-screen controls. Listeners who want to keep monitoring while their phone is in a pocket or the app is backgrounded are forced onto separate native apps. There is no single, self-hosted, "install one thing and it just works" product that is fast on cheap hardware (a Raspberry Pi), sounds good, and behaves like a first-class mobile app.

## Solution

Radio-Scout is a single self-contained binary that ingests calls from existing recorders **with zero migration effort** (it speaks rdio-scanner's ingest API, so an existing Trunk Recorder or SDRTrunk points at it with a one-line URL change), stores audio in real object storage, and serves a beautiful, dark, mobile-first PWA that plays through the OS media layer — so **background audio and lock-screen controls actually work, including on iOS**. It runs great on a Raspberry Pi, needs no external services out of the box (SQLite + filesystem), and scales up cleanly (Postgres + Garage object storage) when wanted.

A **Call** flows: recorder → rdio-compatible ingest → dedup + auto-populate → audio to object store, metadata to DB → pushed over WebSocket to subscribed listeners → played via HTML5 `<audio>` + Media Session, with the archive searchable and replayable.

## User Stories

### Ingest & recorder compatibility
1. As an operator running Trunk Recorder, I want to point its existing rdioscanner uploader at Radio-Scout by changing only the URL, so that I migrate with no new plugin.
2. As an operator running SDRTrunk, I want its existing rdio-scanner broadcaster to work against Radio-Scout unchanged, so that my setup keeps working.
3. As an operator, I want Radio-Scout to accept both `POST /api/call-upload` (generic multipart) and `POST /api/trunk-recorder-call-upload` (Trunk Recorder's `.wav`+`.json`), so that either upload path works.
4. As an operator, I want the exact response strings my recorder expects (`Call imported successfully.`, `duplicate call rejected`, `incomplete call data: no talkgroup`) and HTTP 200 on success, so that my recorder's success/health/duplicate logic behaves correctly.
5. As an operator, I want each ingest authenticated by a per-system API key, so that only authorized recorders can add calls to a System.
6. As an operator, I want duplicate calls (same System + Talkgroup within a short configurable window) rejected, so that the same transmission isn't stored twice.
7. As an operator, I want unknown Systems/Talkgroups/Units created automatically from incoming calls (**auto-populate**) using the labels my recorder already sends, so that the archive is usable with zero manual configuration.
8. As an operator, I want a per-System blacklist of Talkgroups I never want ingested, so that noise/encrypted/private channels are dropped.

### Live feed & playback
9. As a listener, I want incoming Calls for my selected Talkgroups to play automatically (**live feed**), so that I can monitor activity hands-free.
10. As a listener, I want a **listening queue** so that Calls arriving during playback play in order rather than being lost.
11. As a listener, I want **Hold System** and **Hold Talkgroup**, so that I can temporarily focus on the current System or Talkgroup and then restore my selection.
12. As a listener, I want **Skip** so that I can stop a boring/encrypted Call and jump to the next queued one.
13. As a listener, I want **Replay** (current, previous, and back through the last five), so that I can re-hear something I missed.
14. As a listener, I want **Avoid** with an optional timed mode (30/60/120 min auto-reactivate), so that I can mute a chatty Talkgroup temporarily.
15. As a listener, I want **Pause** that suspends playback without losing the queue, so that I can step away.
16. As a listener, I want a scanner **display** showing System label, Talkgroup tag/label/name, frequency, TGID, Unit ID, and a live waveform, updating as the Call plays, so that I know what I'm hearing.
17. As a listener, I want an **LED/status indicator** colored by System/Talkgroup, so that I can tell at a glance which service is talking.
18. As a listener, I want **patched** Talkgroups to reach me if I'm subscribed to any Talkgroup in the patch, so that I don't miss cross-patched traffic.

### Selection
19. As a listener, I want a **Talkgroups** panel to choose which Systems/Talkgroups the live feed plays, so that I hear only what I care about.
20. As a listener, I want **Group** and **Tag** category toggles (three-state on/off/partial), so that I can enable/disable many Talkgroups at once.
21. As a listener, I want per-System all-on/all-off and a global all-on/all-off, so that I can quickly reshape my selection.
22. As a listener, I want my selection persisted locally (optionally namespaced), so that it survives reloads and I can run more than one independent scanner in one browser.
23. As a listener, I want only Calls for my selected Talkgroups pushed to me (server-side filtering), so that bandwidth and battery aren't wasted.

### Archive & search
24. As a listener, I want to **search** the archive by date range, System, Talkgroup, Group, and Tag with a sort order, so that I can find past Calls.
25. As a listener, I want **playback mode** (live feed off) that plays sequentially through filtered archive results with pagination, so that I can catch up on history.
26. As a listener, I want to play an archived Call while the live feed is on (interrupt, then resume the queue), so that I can check something without losing my feed.
27. As a listener, I want to **download** an individual Call's audio, so that I can keep or share it.

### Mobile / PWA / background (the headline)
28. As a mobile listener, I want to install Radio-Scout to my home screen as a PWA, so that it behaves like a native app.
29. As a mobile listener, I want audio to keep playing when the app is backgrounded or the screen is locked, so that I can keep monitoring with my phone away.
30. As a mobile listener, I want working lock-screen / Bluetooth / CarPlay controls and metadata (play/pause/next/prev, System·Talkgroup, artwork) via Media Session, so that I can control playback without unlocking.
31. As a mobile listener, I want playback to survive short quiet gaps between Calls (keep-alive / Managed Media Source), so that a lull doesn't kill my background feed.
32. As a mobile listener, I want an opt-in **Web Push** notification when there's new activity on a watched Talkgroup while the app is fully suspended, coalesced so it doesn't storm me, so that I can tap to jump back in.

### Audio quality (differentiator, opt-in)
33. As a listener, I want an optional per-Call audio-enhancement pipeline (loudness normalization, voice band-pass, noise suppression), so that scanner audio is clearer and levels are consistent between Talkgroups.
34. As an operator, I want enhancement off by default and enable-able per instance (and, on busy systems, per System/Talkgroup), so that it never overwhelms my Pi.

### Configuration & operation
35. As an operator, I want a single self-contained binary with an embedded UI that runs with zero configuration on first launch, so that install is one step.
36. As an operator, I want global settings in a TOML file + CLI flags (port, base_dir, storage backend, DB, retention, enhancement), so that I can configure without a UI.
37. As an operator, I want to bulk-curate Talkgroups (labels, Groups, Tags, LED colors) via **CSV import**, so that I can tidy my archive without a full admin UI.
38. As an operator, I want a password-gated admin surface, so that configuration isn't open.
39. As an operator, I want audio in filesystem storage by default and S3-compatible (Garage) storage as a config option, so that I can start simple and scale to real object storage.
40. As an operator, I want SQLite by default and Postgres as a config option, so that I can run zero-config locally or host the DB on a separate machine.
41. As an operator, I want time-based retention plus an optional total-size cap that prune both audio and metadata together, so that my Pi's disk stays bounded.
42. As an operator, I want prebuilt binaries for my platform (incl. Raspberry Pi arm64), a `curl | sh` installer, a `service install` command, and a Docker image, so that deployment is easy.
43. As an operator exposing the instance publicly, I want to front it with a reverse proxy for TLS/auth, so that I control access (listening is open in v1).

## Implementation Decisions

**Architecture (see ADRs):** Rust · Axum · Tokio backend; React+TS+Vite+Tailwind+shadcn frontend embedded in the binary; Redux Toolkit + RTK Query for client state; SQLite/Postgres via SeaORM; audio in object storage via the `object_store` abstraction; live feed over raw WebSocket.

**Domain model** (per `CONTEXT.md`): `System` 1—* `Talkgroup`, `System` 1—* `Site`, `System` 1—* `Unit`; `Talkgroup` *—* `Group`; `Talkgroup` *—1 `Tag`; `Call` *—1 `Talkgroup`/`System` with child `CallFrequency`, `CallUnit`, `CallPatch`. Every network-facing entity has both an internal **Id** and an external **Ref**; ingest and display resolve by Ref/Label, storage joins by Id.

**Ingest (ADR-0001):** two rdio-compatible endpoints; multipart + Trunk Recorder JSON meta parsers; API-key auth (hashed) scoped per System; duplicate detection (~500 ms window); auto-populate (recorder labels, default `Unknown` Group / `Untagged` Tag, lowest-free-Ref for new Systems); per-System blacklist. Ingest is a serialized pipeline: resolve → dedup → auto-populate → **write audio object, then insert DB row** → emit to live feed.

**Storage (ADR-0002):** `object_store`-backed blob store; filesystem default under `base_dir/audio/<sharded>`, S3/Garage opt-in. Audio served via `GET /api/call/:id/audio` with HTTP range; **S3 backend may issue short-lived presigned URLs after an access-scope check** to avoid proxying at scale. Prune deletes **DB row then object**; periodic orphan-GC.

**Database (ADR-0003):** SeaORM entities + migrations; SQLite default, Postgres opt-in; both exercised in CI. Validate the archive-search query (cascading filters + Group/patch aggregation) on both dialects early — it's the highest dialect-divergence risk.

**Live feed (ADR-0004):** one WebSocket endpoint; compact JSON `{t, …}` messages; per-connection subscription matrix (`systemRef→talkgroupRef→bool`) + access scope; server pushes a Call only to connections whose matrix + scope match (honoring patches); reconnect + heartbeat implemented directly; audio never rides the socket. Hold/avoid/queue/replay/history are client state.

**Client audio (ADR-0005):** single reused HTML5 `<audio>` element + Media Session; **never WebAudio**. Queue advances via Media Session while resident; background gaps bridged by (prototype-selected) keep-alive / Managed Media Source, else graceful degrade; **Web Push** (VAPID, coalesced per-Talkgroup) for suspended-state activity. Service worker handles PWA caching + push→notification only. Next-Call audio is prefetched for gapless playback.

**Audio enhancement (ADR-0006):** opt-in, off by default, cargo-feature-gated (default binary stays pure-Rust). Pipeline: symphonia decode → rubato 48k → nnnoiseless denoise → biquad band-pass → optional fundsp dynamics → ebur128 two-pass loudnorm → AAC-LC/M4A (fdk-aac) [Opus-in-Ogg option]. Runs in a **bounded work-queue with backpressure**; must never delay live ingest. Denoise benefit on digital audio to be validated; AAC muxing + patent posture to be resolved (fallback: system ffmpeg for mux only).

**Auth & security (ADR-0008):** admin via httpOnly+Secure+SameSite **session cookie** (server-side sessions), CSRF-protected writes, brute-force lockout; per-System hashed API keys for ingest; open listening in v1; TLS via reverse proxy.

**Config & curation:** TOML + CLI for globals; auto-populate for entities; Talkgroup **CSV import** (ref,label,group,tag,led,…) for bulk curation; no per-entity in-app editing in v1.

**Packaging (ADR-0007):** single binary, `rust-embed` frontend, zero-config first run (creates `base_dir` + SQLite + fs store); GitHub Releases per OS/arch (incl. linux-arm64), `curl|sh` installer, `service install` subcommand, multi-arch Docker.

**API surface (v1):** `POST /api/call-upload`, `POST /api/trunk-recorder-call-upload`, `GET /api/call/:id/audio` (range/presigned), `WS /api/live`, admin endpoints (login/logout/config/CSV-import/logs), `GET /healthz`, Web Push subscribe endpoint.

## Testing Decisions

Per ADR-0009, tests assert **external, observable behavior**, not implementation details.

- **Primary seam — the integration harness:** bring up the real Axum app in-process against a temp SQLite DB + temp filesystem store; drive it over its **actual HTTP + WebSocket boundary** (POST synthetic Calls, connect a WS client, submit search); assert on DB rows, stored objects, WS pushes, and response bodies. This is the highest single seam and covers ingest → dedup → auto-populate → store → live-feed fanout → search end to end.
- **Recorder-compatibility golden suite:** real captured Trunk Recorder and SDRTrunk multipart payloads as fixtures; assert parse correctness **and** the exact load-bearing response strings + status codes. This is the automated guarantee behind the drop-in-replacement promise.
- **Dual-DB:** the integration suite runs against both SQLite and Postgres (Postgres via testcontainers) in CI; the archive-search/aggregation query is a required dual-dialect case.
- **Unit tests:** parsers, filename-mask, dedup window, access-scope/matrix matching, patch resolution, retention (age + size-cap + orphan-GC ordering), enhancement DSP params.
- **Frontend:** Vitest + React Testing Library for store reducers (queue/selection/playback) and components; Playwright for critical flows (subscribe → receive push → play; archive search → playback; install/Media-Session metadata on WebKit).
- **iOS background audio:** validated manually on a real device — no CI substitute; keep-alive/MMS/Web-Push behavior is prototype-gated.
- **Merge gates:** `cargo fmt`, `clippy -D warnings`, all backend tests (both DBs), frontend tests green; red-green-refactor.

## Out of Scope (v1)

Deferred to **v2:** full admin CRUD web UI (Systems/Talkgroups/Groups/Tags/Units/API-keys); multi-user **access codes** (per-listener PINs, scopes, expiry, connection limits); **dirwatch** ingest + filename-mask + dsdplus/sdr-trunk/trunk-recorder dir formats; **alerts** + keypad beeps (client tone synthesis); native richer ingest API + first-party recorder plugins; built-in Let's Encrypt autocert; light theme; per-System/Talkgroup retention overrides.

Deferred to **later:** downstream forwarding to other instances; broadcast **delayer**; multiple Radio-Scout app instances sharing one DB; hosting the legacy rdio-scanner app.

## Further Notes

**Must-prototype / validate before "done":**
- iOS background gap-bridging (keep-alive vs Managed Media Source vs background-only continuous stream) — on real devices; the continuous-stream option is the most robust but the most Pi-expensive, so prefer client-side mechanisms.
- Pure-Rust AAC-in-M4A muxing (fallback: operator's system ffmpeg for muxing only) and AAC patent/licensing review before distributing an AAC encoder.
- RNNoise denoise quality on already-vocoder-decoded digital (P25/DMR) audio — lead the "edge" with loudness normalization (proven) over denoise (hypothesis).
- SeaORM covering the archive-search aggregation on both dialects without hand-branching.

**Sequencing suggestion (walking skeleton first):** ingest one rdio-compatible Call → store to filesystem → push over WebSocket → play via `<audio>` + Media Session in the PWA. That single vertical slice exercises the primary seam and every core subsystem; everything else layers onto it.

**Research/design references:** `docs/research/audio-pipeline.md`, `docs/research/ios-background-audio.md`, `docs/design/brief.md`.

**Publishing:** published to GitHub Issues (`origin` → github.com/FxllenCode/radio-scout). This spec is the tracker/epic issue **#24** (labelled `ready-for-agent` + `epic`), split into the v1 ticket set **#1–#23** — linked as native sub-issues of #24 with blocking edges expressing their dependency order.
