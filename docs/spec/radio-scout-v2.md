# Spec — Radio-Scout v2

Status: ready-for-agent · Scope: v2, one release (0.2.0) · Decisions: `docs/adr/0001`–`0013`, `CONTEXT.md`, the 2026-07 grilling sessions. v1.0 (the remaining open v1 tickets plus the SDRTrunk patches parsing fix) lands first; v2 starts from that baseline.

## Problem Statement

v1 built the core listening product and proved its founding bet on hardware — background audio with working lock-screen controls, including iOS. But the maintainer's real, public rdio-scanner instance still cannot cut over to it:

- Its **downstream peers would go dark** — the production instance both receives from and forwards to other rdio instances, and Radio-Scout can receive but not forward.
- Its **county-scale catalog overwhelms the Talkgroups panel** — hundreds of rows, no collapse, no sticky controls, the global switches at the bottom of the scroll.
- A migrating listener meets a UI that can show a **green "connected" light over a silently dead feed** (playback mode disables the live feed with no indication anywhere), forgets the last call between transmissions, and buries what's playing on three of four tabs.
- There is **no cap on listeners**, so a popular moment has no overload guardrail on a Pi.

Beyond the cutover, the product has real gaps its research phase documented from primary sources: the same patched transmission plays **three times** because recorders upload it once per member talkgroup and the dedup key can never see that; dynamic patch TGIDs **flood the panel with duplicate buttons** (rdio's issue #466, inherited by our auto-populate); **units are bare numbers** even though SDRTrunk sends `talkerAlias` on every upload and our own database stores unit rows nothing displays; the archive is **write-mostly** — searchable but unexplorable, unshareable, unlinkable; recorders emit **emergency flags, encrypted flags, durations, and signal health** that the ingest discards; and operators administer everything by CSV, TOML, and SSH. Meanwhile rdio-scanner is publicly abandoned, its community is actively seeking a maintained successor, and competitors are racing to claim the seat.

## Solution

v2 makes the cutover real and the instance worth visiting. One release, planned backwards from "the maintainer's public instance runs Radio-Scout in production":

Ingest grows up — the native Trunk Recorder endpoint parses everything TR knows (emergency, encrypted, priority, duration, site, signal health, over-the-air aliases), reachable from any stock TR via a shipped `uploadScript` and a first-party plugin; **channel merge** lets one Talkgroup own many member Refs (and Units own UIDs and Ranges) so patch churn and per-site duplicates collapse into one channel; **keep-best dedup** recognizes the same transmission arriving under different TGIDs and stores the best copy once. Listening grows teeth — a queue you can see, reorder by Priority, and drain with Catch-up; alerts fire on what the signal proves (emergency flags, tone-out matches — never speech, per ADR-0013) over Web Push and Webhooks. The Archive becomes a place — everything URL-addressable, calls shareable by expiring public links, ranges exportable, activity charted and scrubbable, and a **DVR** that replays any talkgroup's night gaplessly. Units become people and apparatus, not numbers. Operators get a full admin surface, a status page, Prometheus metrics, a live recorder dashboard, access codes, per-entity retention, built-in TLS, Dirwatch, the Delay policy, an embeddable player, and a Station stream. The client ships the full UI-critique sweep, so all of it is discoverable on a phone.

## User Stories

### Cutover
1. As an operator, I want Radio-Scout to forward matching Calls to my rdio-compatible Downstream peers (URL + key, scoped per System/Talkgroup), so that cutting my instance over doesn't cut my peers off.
2. As an operator, I want the Downstream sender to queue durably and retry with backoff through a peer outage, so that a peer's downtime costs delay, not Calls.
3. As an operator, I want a configurable cap on concurrent live-feed listeners with a clear "instance full" response, so that a busy moment degrades predictably on a Pi.
4. As an operator, I want to name and brand my Instance (title, header, push notifications), so that listeners know whose scanner they're hearing.

### Ingest enrichment & correctness
5. As an operator, I want the Trunk Recorder native endpoint to parse emergency, encrypted, priority, audio type, precise duration, per-frequency signal/error data, site, and over-the-air unit aliases, so that nothing my recorder knows is discarded.
6. As an operator, I want a shipped, documented `uploadScript` that posts TR's full `.json` + audio to the native endpoint, so that a stock Trunk Recorder sends everything with a one-line config change.
7. As an operator, I want a first-party Trunk Recorder plugin speaking the native contract, so that installs preferring a plugin over a shell hook have one.
8. As a listener, I want every Call to carry its duration — parsed from recorder metadata or the audio header — so that a one-second kerchunk and a forty-second dispatch are distinguishable everywhere.
9. As an operator, I want encrypted Calls stored as flagged metadata-only records (no audio), so that encrypted-channel activity is visible without pretending there's something to hear.
10. As a listener, I want the same transmission arriving multiple times — per-member patch re-broadcasts, multi-site copies — deduplicated to the best copy (fewest errors, longest audio), so that I hear each call exactly once.
11. As an operator, I want Site recorded and displayed on Calls from multi-site systems, so that simulcast coverage is legible.
12. As an operator, I want unit labels consumed from `talkerAlias` and TR's OTA tags at ingest, so that units name themselves with zero configuration.
13. As an operator, I want SDRTrunk's ID3 tags mined from already-stored audio, so that aliases and site data arrive retroactively for the archive I already have.
14. As an operator, I want Dirwatch ingest (Trunk Recorder, SDRTrunk, and DSDPlus directory formats with filename masks), so that file-drop recorders have an ingest path without HTTP.

### Channel merge
15. As an operator, I want a Talkgroup to own multiple member Refs, so that patch-minted dynamic TGIDs and per-site duplicates stop flooding the panel as separate channels.
16. As an operator, I want a Unit to own multiple UIDs and Ranges, so that an apparatus with a mobile and three portables is one Unit.
17. As an operator, I want merges curated in admin and CSV — including folding an already-auto-populated Ref into an existing Talkgroup, re-pointing its archived Calls — so that discovering churn after the fact is recoverable.
18. As a listener, I want selection, search, dedup, blacklists, and the archive to treat merged Refs as one channel, so that a merge is invisible everywhere except the curation screen.

### Alerts
19. As a listener, I want a push notification when a Call carries the emergency flag on a talkgroup I've selected, so that an emergency button press finds me.
20. As an operator, I want per-Talkgroup Tone profiles matched by tone-out detection (two-tone/Quick Call, pure DSP), so that a station's page-out triggers an Alert without any speech recognition.
21. As an operator, I want Alerts delivered to configured Webhooks (with a Discord-compatible payload option) as well as Web Push, so that my community's automations hear what my listeners hear.
22. As a listener, I want alert notifications to respect the existing rules — never pushed while I'm demonstrably listening, coalesced per talkgroup — so that alerting doesn't storm my phone.

### Listening
23. As a listener, I want Catch-up — queued Calls played with silence trimmed at raised speed until I'm live — so that a 40-call backlog takes minutes, not an hour.
24. As a listener, I want to tap the queue counter to see, play, drop, or jump past what's waiting, so that the queue is a tool instead of a number.
25. As a listener, I want an Undo snackbar after Avoid and a sheet listing active avoids with individual removal, so that a mis-tap doesn't silently mute a channel.
26. As a listener, I want my Avoids and Holds to survive a reload, so that a 120-minute avoid means 120 minutes.
27. As a listener, I want per-Talkgroup Priority so its Calls jump the listening queue, so that dispatch outranks tactical chatter when I'm behind.
28. As a listener, I want a session log of everything heard this session with replay and quick actions, so that "what was that ten minutes ago" has an answer.
29. As a listener, I want to Pin talkgroups to the top of the Talkgroups panel, so that my daily channels don't live behind a 400-row scroll.

### Archive, sharing & DVR
30. As a listener, I want search filters, the selected talkgroup set, and individual Calls all addressable by URL, so that any view is a bookmark and any find is a text message.
31. As a listener, I want date-range presets (last hour, today, yesterday, last 7 days) and a reset control, so that common searches cost one tap instead of eight.
32. As a listener, I want to mint an expiring public share link for a single Call with a clean preview card, so that sharing a moment doesn't require sharing the instance.
33. As a listener, I want to export a talkgroup + time range as a zip or a single stitched audio file, so that an incident can be kept or handed to someone.
34. As a listener, I want per-talkgroup activity charts clickable into the archive at that moment, so that "when was it busy" is a picture I can tap.
35. As a listener, I want a call-density timeline over search results with jump-to-date, so that time travel doesn't mean paging.
36. As a listener, I want duration and unit columns and a minimum-duration filter in search, so that kerchunks stop wasting my taps.
37. As a listener, I want to Star Calls and filter by starred, with starred Calls exempt from Retention where the operator allows, so that what mattered to me survives.
38. As a listener, I want Events — named, curated collections of Calls, frozen against Retention, shareable by link and exportable as audio — so that an incident outlives the retention window.
39. As a listener, I want the DVR: pick a talkgroup (or my Selection) and a time range, and scrub gaplessly through it oldest-first on a density timeline, so that "rewind the county to 2am last Friday" is one gesture.
40. As an operator, I want DVR playback served efficiently (playlist-stitched over existing audio objects), so that an hour of scrubbing doesn't cost the Pi an hour of transcoding.
41. As an operator, I want listener-count history recorded as counts (never identities), so that the analytics page can show peak listeners with timestamps.

### Units
42. As a listener, I want unit labels shown wherever a source appears — display, recent list, search, call detail — so that radios are names, not numbers.
43. As an operator, I want unit CSV import with Ranges, so that a fleet's numbering scheme loads in one paste.
44. As a listener, I want a per-Unit history view (talkgroups used, first/last heard, its Calls) and search-by-unit, so that "who said that, and where else" is answerable.

### Admin & operations
45. As an operator, I want full CRUD administration in the browser — Systems, Talkgroups (labels, LEDs, blacklists, merges), Groups, Tags, Units, API keys, Access codes, Downstreams, Dirwatch entries, Tone profiles, Webhooks, branding — so that running an instance never requires SSH.
46. As an operator, I want multi-select bulk assignment of Groups and Tags across talkgroup rows, so that categorizing a county doesn't take an afternoon.
47. As an operator, I want entity-configuration export and import as JSON, so that a curated setup is backupable and portable.
48. As an operator, I want an instance status page — per-System last-call and rate, queue depths, storage and retention headroom, listener count, downstream and webhook health — so that "is it healthy" is one glance.
49. As an operator, I want a Prometheus metrics endpoint, so that my existing Grafana watches Radio-Scout too.
50. As an operator, I want a live recorder dashboard fed by Trunk Recorder's status WebSocket (active calls, decode rates, recorder states, why-not-recorded), so that I can see what my SDRs are doing right now.
51. As an operator, I want per-frequency and per-SDR health surfaced (error and spike rates, frequency drift, signal levels), so that a dying dongle or antenna announces itself.

### Access & retention
52. As an operator, I want Access codes — scoped per System/Talkgroup, with optional expiry and connection limits — so that sensitive channels can be gated while listening stays open by default.
53. As an operator, I want per-System and per-Talkgroup Retention overrides (unset inherits the global policy), so that Fire keeps 90 days while everything else keeps 14.

### Platform & polish
54. As a listener, I want the full UI-critique sweep: truthful feed state everywhere (a mini-player on every tab, an explicit banner when the live feed is off), a persistent last-call card with live controls, patch provenance chips, a pre-permission push explainer, a working push deep link that plays the Call I was notified about, an install path in Settings, county-scale panel ergonomics (sticky controls, collapsible systems, virtualization, activity signals on rows), phone-grade touch targets, and screen-reader announcements of each call — so that the product's truth is visible on every screen.
55. As a listener, I want hotkeys for skip, pause, avoid, hold, and replay, so that desktop listening is button-driven — and Stream Deck users get transport control through the existing media-key support, documented.
56. As an operator, I want sixteen curated LED colors (still an enum, still test-pinned), so that color collides half as often on big systems.
57. As an operator, I want the identity option cluster — listener count display, 12/24-hour time, talkgroup sort, playback-goes-live, display dimmer, help link — so that the instance feels like mine.
58. As an operator, I want an operator-editable reference page (markdown, seeded with the genuinely standard content), so that my listeners have my county's codes, not a wrong national list.
59. As an operator, I want an embeddable player page (live + recent Calls for a chosen selection) any site can iframe, so that the fire department's homepage can carry the feed.
60. As a listener, I want a Station stream — a continuous audio URL of a Selection — so that a smart speaker or car radio can play the scanner without the app.
61. As an operator, I want built-in TLS with Let's Encrypt autocert as an option, so that a public instance can be one binary with no proxy in front.
62. As an operator, I want the Delay policy per System/Talkgroup — published late, flagged, restart-safe — so that officer-safety publication rules are enforceable.

## Implementation Decisions

**Release shape.** One release, 0.2.0; the definition of done is the production cutover. v1.0 precedes it and carries the SDRTrunk patches parsing fix. Sequencing inside the release: ingest enrichment, duration, and channel merge land early because keep-best dedup, alerts, DVR, and unit features consume them; the UI truth fixes land early because everything else demos through them.

**Channel merge.** Talkgroup and Unit each own an ordered set of member Refs (Units also Ranges); one is primary (displayed, exported, hash-colored). Resolution happens server-side at ingest, before the wire shape is built, so every client surface keeps its existing Ref-keyed algebra against primary Refs and needs no migration. Dedup, blacklists, selection matrices, search, facets, CSV upsert, and enhancement scope all operate on the canonical entity. Merging an existing auto-populated Talkgroup into another re-points its archived Calls transactionally and leaves a member Ref behind; unmerging restores it. CSV grows a member-refs column; admin gets merge/unmerge affordances.

**Keep-best dedup.** The duplicate test widens from (System, Talkgroup, ±window) to: same System, overlapping time window, and same canonical Talkgroup *or* overlapping patch membership. The winner is the better copy — fewer decode errors, longer duration — and a later-arriving better copy replaces the stored one under the stable Call identity, the enhancement pipeline's swap-under-stable-URL precedent. Patches remain per-Call properties (no persistent patch entity — a deliberate decision; patch churn makes persisted patches self-cluttering).

**Ingest enrichment.** The TR-native meta parser gains the full field set TR writes (emergency, encrypted, priority, audio type, start/stop/length, per-frequency and per-source detail including OTA tags, site). Duration comes from recorder metadata when present, else a cheap audio-header parse at ingest — it rides the wire on every Call. Encrypted Calls become flagged metadata-only rows. The generic endpoint additionally consumes `talkerAlias` and `site`. ID3 mining runs as an off-path backfill job over stored SDRTrunk audio. Dirwatch reuses the same pipeline behind a filesystem watcher with per-watch format and mask configuration.

**TR full-meta paths.** A shipped shell script for TR's `uploadScript` hook posts the `.json` + audio pair to the native endpoint (the documented path, used by the maintainer's own scanner). A first-party TR plugin speaking the same contract ships as a separate artifact with its own build; the wire contract it must satisfy is pinned on Radio-Scout's side by the golden suite, so plugin and script cannot drift from the parser.

**Downstream sender.** An rdio-dialect upload client with per-System/Talkgroup scoping, a durable on-disk queue, retry with backoff, and health surfaced on the status page. Improves on rdio's forwarder by surviving peer outages without loss. Receiving needs nothing: a peer's downstream is an uploader with an API key.

**Alerts.** Evaluated on the live-feed fanout path (the push precedent — never on ingest). Emergency alerts fire from the enriched flag; tone-out detection runs in the off-path audio worker against per-Talkgroup Tone profiles and marks the Call, which then alerts. Delivery reuses Web Push (existing suppression-while-listening and coalescing rules apply, with alert-class topics) and adds Webhooks: operator-configured URLs, JSON payload with an optional Discord-compatible shape, retried with backoff, never blocking. No speech-derived triggers of any kind (ADR-0013).

**Listening.** Priority is a queue-insert sort key. Catch-up raises playback rate and skips silence using per-Call silence maps produced by the audio worker (with a header-level fallback when absent); it engages from the queue sheet and disengages at live. Avoids and Holds join the persisted state alongside the Selection. The session log is client-side.

**Archive, sharing & DVR.** Search state, selection, and Call deep links become URL state. Share links are expiring signed URLs served by the binary with a minimal preview page. Range export streams a zip or a pure-Rust stitched file. Activity charts and the density timeline come from time-bucketed aggregate queries. The DVR is an oldest-first gapless run over a talkgroup-or-Selection + time range: served as a playlist with discontinuity markers over the existing per-Call audio objects (no transcoding, no concatenation on the serve path — the Pi serves what it already serves), scrubbable against the density timeline; the stitched-file exporter is the download path, not the playback path. Events freeze member Calls by copying their audio objects at curation time so Retention's sweep stays simple; Stars are per-browser marks whose retention exemption is operator policy.

**Units.** Labels ride the wire wherever a source appears. Unit CSV import accepts Refs and Ranges. The per-Unit view is an archive filter surface, not a new subsystem.

**Admin.** Full CRUD over every entity the spec names, behind the existing session/CSRF/lockout guard (all new routes under the same prefix layer, protected by construction). Bulk assignment is multi-select over talkgroup rows. Entity config exports/imports as JSON (entities only — the TOML remains the home of infrastructure configuration, and the admin password stays in the environment). Tone profiles, Webhooks, Downstreams, Dirwatch entries, Access codes, branding, and the identity cluster are managed here.

**Access codes.** Gate listening through the access-scope seam already present in the live feed, extended to archive search and audio serving. Codes carry System/Talkgroup scope, optional expiry, and a concurrent-connection limit. Open listening remains the default posture; an instance without codes behaves exactly as today.

**Observability.** The status page aggregates what the process already knows. The metrics endpoint exposes Prometheus text format with minimal dependencies. The recorder dashboard is a WebSocket endpoint TR's statusServer dials into, feeding a live admin view and the health charts; per-frequency history persists compactly. Listener counts are recorded as counts on an interval — never identities, per the logging policy.

**Retention overrides.** Nullable per-System and per-Talkgroup retention settings, NULL inherits (the established scope pattern). The sweep gains a per-entity pass and honors Star/Event exemptions.

**Platform.** Built-in TLS is optional rustls + ACME autocert; the reverse-proxy path remains first-class and documented. The Delay policy holds publication (store now, emit late, flagged, restart-safe) and composes with the live feed's catch-up. The embeddable player is a minimal standalone page with iframe-safe headers. The Station stream is a chunked continuous encode of a Selection with silence fill — one stream is bounded, cheap on a Pi, and its cost is documented. LED palette doubles to sixteen curated entries (enum and tests stay). The reference page renders operator-supplied markdown. Media-source work in the DVR player must respect ADR-0005's constraints (single audio element, no WebAudio; HLS via native support or MSE where allowed).

## Testing Decisions

Tests assert external, observable behavior at the highest existing seam — the rules and gates of ADR-0009/0010 (100% patch coverage, ratcheting floor, mutation testing) bind every ticket.

- **Primary seam: the integration harness.** Every server-side feature drives through the real HTTP/WS boundary via `common::TestApp`, which grows builder wiring and stub peers on the established stub-service precedent: a downstream peer that records what the sender delivers (asserting the rdio dialect it speaks), a webhook sink that records alert payloads, a fake TR statusServer client over the existing WS helpers, and a temp-directory dirwatch driver. The fault-injection seams apply as-is to the new workers (sender queue, tone-out worker, backfill job).
- **Golden suite for every wire contract:** enriched TR meta fixtures, the uploadScript payload, the plugin's output (fixtures on our side pin the contract the plugin must emit), dirwatch format fixtures, and the unchanged rdio response strings — insta-pinned as today.
- **Dual-dialect and real-S3** runs cover the new queries (aggregates, merges, retention passes) and the new object flows (Event freezing, exports, share links) exactly as v1's harness tests do.
- **DSP unit seam** (the enhancement pipeline's pattern): tone-out detection against audio fixtures — synthesized tone sequences, real page-outs, and near-miss negatives; silence-map extraction likewise.
- **Property-based and parametrized coverage** where the input space is adversarial: merge resolution (member Ref sets, ranges, overlaps), the widened dedup predicate, playlist generation, mask parsing for dirwatch.
- **Frontend:** Vitest + RTL with MSW at the network boundary for every screen change; Vitest Browser Mode (landing with v1.0) for the DVR/gapless player and Media Session wiring; Playwright only where the worker or a standalone page is involved (embed page, share-link preview, push deep-link open).
- **New seam, deliberately one:** built-in TLS tests against a local ACME directory stub in its own CI job — autocert cannot be honestly tested through the harness alone. The TR plugin's build gets a CI job compiling it; its behavior is already pinned by fixtures.
- **The iOS real-device manual gate re-runs** for the DVR player and catch-up work — any change to media-source behavior re-opens ADR-0005's checklist.

## Out of Scope

- **Transcription — banned, not deferred** (ADR-0013). No speech-to-text, and no feature that assumes transcripts exist.
- **CarPlay / Android Auto native shell** — documented wontfix for v2; revisit only on demonstrated post-cutover demand.
- **Alert-tones parity and keypad beeps** (rdio's nine assignable UI sounds) — backlog, demoted from cutover relevance.
- **A persistent, subscribable Patch Group entity** — keep-best dedup plus channel merge dissolve the problem; patches stay per-Call properties.
- **Smart Playback conversation grouping** — deferred; the DVR serves the intent.
- **Surge/activity alert rules, keyword rules, silent-system watchdog** — alerting in v2 is event-quality only (emergency, tone-out).
- **A Stream Deck plugin** — media-key support already serves it; a docs paragraph, no code.
- **MySQL/MariaDB, multi-instance shared DB, hosting the legacy rdio app, listener accounts/billing beyond Access codes** — unchanged non-goals.

## Further Notes

- The research grounding this spec (parity audit, UI critique, community demand, competitive landscape, recorder-data mining, and the community-asks verification) was produced in the 2026-07 sessions; its conclusions are distilled here rather than referenced.
- The first-party TR plugin is the release's only non-Rust artifact; everything else remains one static binary.
- Improve-don't-clone receipts worth keeping in commit messages: keep-best dedup vs. rdio's first-wins; the durable downstream queue vs. rdio's forwarder; channel merge (no product does archive-level merging); the DVR (no product has one); alerts on recorder metadata while competitors paywall speech-derived alerting.
- The cutover itself ends with an update to the migration guide reflecting what the migration actually required.
- Publishing: this spec is the v2 tracker epic, split into tickets with blocking edges expressing dependency order (ingest enrichment → merge → dedup → alerts/DVR; admin CRUD → codes/profiles/webhooks UIs; UI truth fixes early).
