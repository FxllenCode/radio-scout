# Radio-Scout

Radio-Scout ingests **Calls** from **Recorders** and distributes them to **Listeners** through a scanner-style web app. This glossary is the project's ubiquitous language — use these terms exactly, in code and in conversation.

**"Scanner" is an adjective here, never a noun.** "Scanner audio", "a scanner-style app" — fine. The nouns it used to stand in for each have their own word, because it was carrying four meanings at once: a running Radio-Scout is an **Instance**, a Listener's independent setup is a **Profile**, the software that feeds us Calls is a **Recorder**, and the physical radio hardware is out of scope for this glossary entirely.

## Language

### People

**Operator**:
The person who runs an **Instance** — installs it, points **Recorders** at it, and decides retention, storage and enhancement policy. Holds the **admin password**; the only person who needs one.
_Avoid_: admin, user, host, owner.

**Listener**:
The person who listens through the web app. Needs no account and no **Session**: their whole state — **Selection**, **Hold**, **Avoid**, **Profile**, **push subscription** — lives in their own browser. One person is often both a Listener and the **Operator**; the terms name the role, not the human.
_Avoid_: user, client, subscriber, viewer.

### Core entities

**Call**:
A single recorded radio transmission (or conversation) — audio plus its metadata (when, which talkgroup/system, frequency, units heard). The atomic unit Radio-Scout stores and plays.
_Avoid_: recording, clip, transmission, audio file.

**System**:
A radio network Radio-Scout receives calls from (e.g. a P25 trunked system). Owns talkgroups, sites, and units.
_Avoid_: network, agency.

**Talkgroup**:
A logical channel within a system that calls are addressed to (e.g. "Fire Dispatch"). Listeners subscribe at talkgroup granularity.
_Avoid_: channel, TG (in prose), frequency.

**Group**:
A cross-system category that clusters talkgroups by purpose (e.g. "Fire", "Law") for bulk selection. A talkgroup may belong to several groups.
_Avoid_: category (reserve "category" for the UI concept spanning groups + tags).

**Tag**:
A single service label on a talkgroup (e.g. "Fire Dispatch", "EMS"). A talkgroup has exactly one tag; a group may contain many.
_Avoid_: label, type.

**Unit**:
A single radio (identified by a radio ID) heard transmitting within a system. May carry a human alias.
_Avoid_: radio, source, subscriber.

**Site**:
A physical tower/receiver site within a system that a call was heard on. Discovered from traffic the way a **Talkgroup** is — never gated on **Auto-populate**, because a tower is the System's own infrastructure rather than a channel that could clutter a panel.

**Duration**:
How long a **Call**'s transmission lasted. From the **Recorder**'s own metadata when it sent any, otherwise read from the audio's container header at **Ingest** — never by decoding. Every Call carries one except where neither could say, and "unknown" is distinct from "zero": an unmeasured Call matches no length filter.
_Avoid_: length (the recorder's word — TR's `call_length` — reserve it for the wire field), runtime, playtime.

**Emergency**:
The bit a radio sets on a transmission when its emergency button is pressed. A property of a **Call**, and separately of each **Unit** heard within one — the Call says somebody keyed it, the per-source flag says which radio did. Absent means no, never unknown.
_Avoid_: alert (an **Alert** is what an emergency may *produce*), panic, priority (the recorder's own unrelated field).

**Encrypted Call**:
A **Call** on a talkgroup whose traffic is encrypted: stored as a flagged, metadata-only row with **no audio object at all**, because what a recorder captures there is the vocoder's noise rather than speech. It reaches **Listeners** so the activity is visible, carries no audio URL, and never enters the **Listening queue**.
_Avoid_: blocked call, private call, secure call.

**OTA alias**:
The name a radio broadcast about *itself*, as opposed to the alias an **Operator** configured for it — Trunk Recorder's `tag_ota`, SDRTrunk's `talkerAlias`. Kept beside the configured label rather than replacing it: when the two disagree, the disagreement is the information.
_Avoid_: talker alias (one vendor's word), radio name, over-the-air tag.

**Patch**:
A temporary, console-made union of talkgroups whose traffic reaches any listener subscribed to a member. A property calls carry, not an entity of its own — patches churn (some systems mint a fresh TGID per patch event), so Radio-Scout deduplicates and routes patched traffic rather than modelling patches as subscribable things.
**A patch's members are Talkgroups, only ever Talkgroups.** A console can patch individual radios into the union too, and SDRTrunk appends those radio IDs behind the talkgroups in the same flat `patches` array with nothing marking the boundary — so a ref is a member only when its System has a Talkgroup for it, and one it doesn't recognize is dropped rather than guessed at (#81, rdio-scanner's own rule).
_Avoid_: supergroup, simulselect, regroup (each is one vendor's word).

### Identity

**Ref**:
The external, radio-network-assigned numeric identifier that recorders send (`systemRef`, `talkgroupRef`, `unitRef`, `siteRef`). Stable across instances; the thing humans and recorders reference.
_Avoid_: external id, radio id (in code identifiers).

**Id**:
Radio-Scout's internal database primary key for an entity. Never sent by recorders; never shown to users. **Ref and Id are distinct** — conflating them breaks joins.

**Member Ref**:
One of the external Refs a **Talkgroup** or **Unit** answers to. Every entity has exactly one **primary Ref** (the one displayed and exported) and may own additional member Refs — a patch-minted dynamic TGID, a per-site duplicate, a second radio carried by the same apparatus — resolved to the owning entity at ingest, so the archive and the panel see one channel where the radio network sees several numbers.
_Avoid_: alias (SDRTrunk's word for the owning entity, not the id), secondary ref, merged id.

**Range**:
A contiguous span of Refs owned as member Refs (`unitFrom..unitTo`). Mostly a Unit affair — fleets number their radios in blocks.

### Listening experience

**Live feed**:
The mode where incoming calls play automatically as they arrive, filtered to the listener's selected systems/talkgroups.
_Avoid_: live mode, streaming.

**Feed off**:
The live feed switched off by the **Listener** — a hard off, and not a pause: the playing call stops, the **listening queue** clears, the connection closes, and **Web Push** (if subscribed) takes over, because nothing is being listened to. Persists until switched back on; rejoining starts from now, never backfilling the silence.
_Avoid_: offline (the network's state, not the listener's choice), disabled, standby.

**Feed down**:
The **live feed** not delivering because the connection isn't there — through no choice of the **Listener's**. The counterpart to **Feed off**, and the distinction is the whole point: Feed off is a decision and persists; Feed down is a condition and clears itself when the connection returns. A Listener is told which, because "nothing is happening" and "nothing is reaching you" call for different reactions.
_Avoid_: offline, disconnected, dropped, stale.

**Playback mode**:
The mode where the listener plays archived calls from the searchable history instead of the live feed. Mutually exclusive with live feed.
_Avoid_: archive mode, replay mode.

**Run**:
The ordered set of archived **Calls** a **Listener** is walking, and where they are within it — which **Call** is playing, what follows, and which page of the **Archive** has to be on hand for it. One concept with an ordering: walking search results newest-first, playing forward in time from a row, and a **DVR** across a time range are the same **Run** configured differently. A Run knows which search it belongs to, so a search that changes ends it rather than silently walking the wrong results.
_Avoid_: session (the **Operator's** admin login), playlist, queue (the **listening queue** is a different set), walk.

**Listening queue**:
The ordered set of not-yet-played live calls waiting to play. Its depth is the `Q` count in the display. Bounded — and it truncates in the same order it plays: lowest **Priority** first, then stalest. A queue that plays by one order and drops by another can starve the one talkgroup the **Listener** said mattered.
_Avoid_: buffer, backlog.

**Hold**:
Temporarily narrowing the live feed to only the current call's system (hold system) or only its talkgroup (hold talkgroup), then restoring the prior selection when released.

**Avoid**:
Muting a talkgroup in the live feed, optionally for a fixed duration (e.g. 30/60/120 minutes) after which it re-activates automatically.
_Avoid_: mute, block, ignore.

**Selection**:
The **Listener's** chosen set of active systems/talkgroups/groups that the live feed plays. Persisted per browser, under a **Profile**.
_Avoid_: subscription, filter.

**Profile**:
One named, independent **Listener** setup within a single browser — its own **Selection**, **Avoid** list and **Hold** state. Two Profiles behave as two entirely separate radios in the same browser: a "truck" Profile and a "desk" Profile share nothing. Spelled `namespace` in the client's persistence layer, which is the mechanism rather than the concept.
_Avoid_: namespace (in prose), workspace, preset, scanner.

**Push subscription**:
One browser's registration for **Web Push** notifications — the push service endpoint it is reachable at, the keys that make a message readable only by that device, and the **Selection** it wants to be woken for. The delivery half, distinct from the Selection itself: a listener has one Selection and zero or one push subscription per browser. Identified in logs by its **Id**, never by its endpoint (a stable per-device identifier).
_Avoid_: notification subscription, device token, registration.

**Coalescing**:
The rule that bounds notifications: at most one per **talkgroup** per push subscription per configured window, each carrying a count of the calls it stands for. A busy system must never storm a phone, and nothing is silently dropped for it.
_Avoid_: throttling, rate limiting, batching, debouncing.

**Priority**:
A **Listener's** per-talkgroup preference that makes its calls jump the **listening queue** instead of waiting their turn. Queue order, not selection — a priority talkgroup still has to be selected to be heard.
_Avoid_: preempt (SDRTrunk's stronger notion — interrupting the playing call — which this is not), favorite.

**Pin**:
Keeping a talkgroup at the top of the Talkgroups panel. A panel-ordering affordance only; pins change nothing about what plays.
_Avoid_: favorite, star (a **Star** marks a Call).

**Catch-up**:
Draining the **listening queue** faster than real time — silence trimmed, playback rate raised — until the feed is live again. A **Listener's** action on Calls they already have; distinct from a **Backfill**, which is how they got them.
_Avoid_: fast-forward, smart speed (a product's trademark), time compression, backfill.

**Backfill**:
The **Calls** a **Listener** missed while **Feed down**, sent on reconnect so the **live feed** resumes without a hole. Ordered by when a Call was *emitted*, never by when it was stored — a **Delay**ed Call is stored early and emitted late, so a cursor over storage order would skip it silently. Bounded — past the bound the Listener is told their history has a gap only archive search can fill, because a silent truncation is indistinguishable from having missed nothing.
_Avoid_: catch-up (the Listener draining a queue, not the server refilling one), replay, resync, history.

**DVR**:
The archive surface that plays one talkgroup (or a **Selection**) gaplessly across a time range, scrubbable on a call-density timeline. Oldest-first by construction — a DVR that plays backwards is a search result, not a DVR.
_Avoid_: time machine, rewind mode, tape.

**Station stream**:
A continuous audio stream of a **Selection** — calls in order, silence-filled — for players that can't run the app (smart speakers, stream URLs, car radios).
_Avoid_: radio mode, icecast feed (the mechanism), broadcast.

### Alerting

**Alert**:
A notification fired by something a call's *metadata or signal* proves — an emergency flag, a **tone profile** match — delivered by **Web Push** and **Webhooks**. Never fired by speech content: transcription is banned ([ADR-0013](docs/adr/0013-no-transcription.md)).
_Avoid_: notification (the delivery, not the occurrence), alarm.

**Tone profile**:
The per-talkgroup definition of a paging tone sequence (two-tone/Quick Call) that tone-out detection matches against a call's audio. Signal processing, not speech recognition.
_Avoid_: tone set, page definition.

**Webhook**:
An **Operator**-configured URL that receives **Alert** payloads (optionally Discord-shaped). The automation escape hatch; delivery is retried and never blocks anything.
_Avoid_: integration, callback.

### Ingest & distribution

**Recorder**:
The software that receives radio and uploads **Calls** to an **Instance** — Trunk Recorder or SDRTrunk. Authenticates with an **API key**. Radio-Scout ships no plugin for either: both already speak the rdio-scanner upload dialect, and that dialect is the compatibility contract.
_Avoid_: source, uploader, feeder, scanner.

**Ingest**:
Accepting a **Call** from a **Recorder** into Radio-Scout (via the HTTP upload API or, later, directory watching).
_Avoid_: upload, import (except in user-facing recorder docs).

**Admission**:
What **Ingest** decided about one **Call** — stored, duplicate, dropped for a named reason, or refused for an unauthorized **API key**. A *value*, not a response: **Ingest** resolves what the database knows, decides purely, then performs. The HTTP endpoints render an Admission into the rdio wire strings; **Dirwatch** logs one with no HTTP in reach. Its reason is a single closed vocabulary — the machine-readable slug and the recorder-facing detail derive from the same value, never two strings that can drift.
_Avoid_: disposition (the narrower auto-populate/blacklist decision *inside* an Admission), verdict, result, outcome.

**Auto-populate**:
Automatically creating an unknown system/talkgroup/unit the first time a call for it is ingested, so the archive is usable with zero manual configuration.
_Avoid_: auto-create, discovery.

**Downstream**:
Another instance this **Instance** forwards matching **Calls** to, speaking the rdio upload dialect, scoped per System/Talkgroup. Forwarding only — *receiving* a peer's downstream is just **Ingest** with an API key.
_Avoid_: relay, mirror, federation, upstream.

**Dirwatch**:
Ingesting **Calls** from a watched directory instead of an HTTP upload — recorder drop folders, DSDPlus, filename masks.
_Avoid_: file ingest, folder watch, hot folder.

**Delay**:
Per-System/Talkgroup policy that publishes a **Call** to **Listeners** only after a configured interval — stored on arrival, emitted late, flagged as delayed, surviving restarts. Officer-safety policy, not a buffer.
_Avoid_: delayer (rdio's noun for the mechanism), embargo, hold-back.

**Access code**:
A listener-facing PIN that grants scoped viewing access to specific systems/talkgroups (with optional expiry and concurrent-connection limits). Distinct from an **API key**.
_Avoid_: password, passcode.

**API key**:
A recorder-facing secret that authorizes ingesting calls into specific systems. Distinct from an **access code**.

**Admin password**:
The single **Operator**-facing secret that opens the admin surface — everything under `/api/admin/`, which configures the **Instance**. Distinct from both an **access code** (**Listener**-facing, scoped) and an **API key** (**Recorder**-facing). There is exactly one; it lives in the environment (`RADIO_SCOUT_ADMIN_PASSWORD`), not the database.
_Avoid_: admin key, admin token.

**Session**:
The server-side record that an operator has proved they know the **admin password**, referred to by an opaque id in an httpOnly cookie. Ends when it is logged out, when it goes unused for its idle window, when its absolute lifetime runs out, or when the process restarts. Unqualified "session" always means this one — a listener needs none.
_Avoid_: token, login, JWT.

**CSRF token**:
The secret bound to a **session** that a state-changing admin request must echo back in `X-CSRF-Token`, proving the request came from this origin's own page rather than from another site trading on the cookie.
_Avoid_: nonce, anti-forgery token.

**Lockout**:
The refusal to check any password from an address that has spent its budget of failed logins, until a cooldown measured from its last attempt has passed. Per address, and never shared: one address's failures neither spend nor restore another's.
_Avoid_: ban, throttle, rate limit.

### Audio quality

**Enhancement**:
Reprocessing a stored call's audio to make it clearer and consistently loud — noise suppression, voice band-pass, loudness normalization — replacing the audio object the call points at. Opt-in and off by default, scoped per instance, system or talkgroup. Never happens on the ingest path: a recorder's upload is answered before any of it starts.
_Avoid_: conversion (rdio-scanner's word, for the narrower act of changing format), transcoding, processing, normalization (one stage of enhancement, not the whole of it).

**Passthrough**:
Keeping a call's audio exactly as the recorder sent it. The default, and what a call keeps whenever enhancement is off, out of scope for it, or unable to run.
_Avoid_: raw, as-is, unconverted.

**Enhancement queue**:
The calls waiting to be enhanced. Server-side work, and distinct from the **listening queue**, which is what a listener is about to hear — the two are never the same set. Bounded: a call that cannot be admitted keeps its **passthrough** audio rather than waiting.
_Avoid_: work queue, job queue, backlog, pipeline.

### Deployment

**Instance**:
One running Radio-Scout: a process, its **Archive**, its configuration and its **admin password**. The unit an **Operator** installs, upgrades and points **Recorders** at. Two Instances share nothing unless they are given the same database and object store.
_Avoid_: scanner, server, deployment, node, site (Site is a tower).

**Service**:
The operating system's registration that runs Radio-Scout at boot and restarts it if it dies — a systemd unit, a launchd daemon, or a Windows scheduled task. Installed, removed and controlled by `radio-scout service …`. Distinct from the running process: uninstalling the service leaves the binary, and stopping the process leaves the service.
_Avoid_: daemon, unit, task (each is one platform's word for it), autostart.

**Target**:
One platform a release is built for, named by its Rust triple (`aarch64-unknown-linux-musl`). The thing an **asset** name and the installer's machine detection have to agree about.
_Avoid_: platform, architecture, arch (each is only half of one).

**Asset**:
One file published with a release: an archive holding the binary for a single **target**, or the `SHA256SUMS` covering all of them. What `install.sh` downloads and verifies.
_Avoid_: artifact (reserve that for CI build outputs, which are not published), download, package.

### Storage & retention

**Archive**:
Every **Call** an **Instance** currently holds — what a **Listener** searches and replays in **playback mode**, and what **Retention** bounds. Metadata in the database, audio in the object store; "the Archive" means both halves together, never one of them.
_Avoid_: history, library, database, back catalogue.

**Retention**:
The policy that bounds the **Archive**: an age window (in days) plus an optional cap on total stored audio, overridable per System/Talkgroup (unset inherits). Expressed as configuration; enforced by sweeps. **Starred** calls and **Event** members are exempt.
_Avoid_: expiry, TTL, cleanup.

**Star**:
A **Listener's** per-browser mark on a **Call**, filterable in search and exempt from **Retention** where the **Operator** allows.
_Avoid_: favorite, bookmark, like.

**Event**:
A named, curated collection of **Calls** — an incident assembled by hand — frozen against **Retention**, shareable by link, exportable as audio. The one thing in the **Archive** that is meant to outlive it.
_Avoid_: incident (the real-world happening, not the collection), playlist, compilation.

**Sweep**:
One pass of the retention policy over the archive — age out, then enforce the size cap, then reclaim orphans. Runs at startup and on an interval.
_Avoid_: job, cron, scheduler run.

**Prune**:
Removing a call from the archive because retention says so: its metadata row first, then its audio object.
_Avoid_: delete, purge, evict (reserve _delete_ for a single row or object).

**Orphan**:
A stored audio object no call row points at — the residue of an ingest that failed after writing its audio, or of a prune interrupted between the row and the object. Reclaimed by **orphan-GC**, which spares anything written inside the grace period so it can't race an in-flight ingest.
_Avoid_: dangling blob, garbage.
