# Radio-Scout

Radio-Scout ingests audio "calls" from software-defined-radio recorders and distributes them to listeners through a scanner-style web app. This glossary is the project's ubiquitous language — use these terms exactly, in code and in conversation.

## Language

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
A physical tower/receiver site within a system that a call was heard on.

### Identity

**Ref**:
The external, radio-network-assigned numeric identifier that recorders send (`systemRef`, `talkgroupRef`, `unitRef`, `siteRef`). Stable across instances; the thing humans and recorders reference.
_Avoid_: external id, radio id (in code identifiers).

**Id**:
Radio-Scout's internal database primary key for an entity. Never sent by recorders; never shown to users. **Ref and Id are distinct** — conflating them breaks joins.

### Listening experience

**Live feed**:
The mode where incoming calls play automatically as they arrive, filtered to the listener's selected systems/talkgroups.
_Avoid_: live mode, streaming.

**Playback mode**:
The mode where the listener plays archived calls from the searchable history instead of the live feed. Mutually exclusive with live feed.
_Avoid_: archive mode, replay mode.

**Listening queue**:
The ordered set of not-yet-played live calls waiting to play. Its depth is the `Q` count in the display.
_Avoid_: buffer, backlog.

**Hold**:
Temporarily narrowing the live feed to only the current call's system (hold system) or only its talkgroup (hold talkgroup), then restoring the prior selection when released.

**Avoid**:
Muting a talkgroup in the live feed, optionally for a fixed duration (e.g. 30/60/120 minutes) after which it re-activates automatically.
_Avoid_: mute, block, ignore.

**Selection**:
The listener's chosen set of active systems/talkgroups/groups that the live feed plays. Persisted per browser (optionally namespaced so one browser can run independent scanners).
_Avoid_: subscription, filter.

**Push subscription**:
One browser's registration for **Web Push** notifications — the push service endpoint it is reachable at, the keys that make a message readable only by that device, and the **Selection** it wants to be woken for. The delivery half, distinct from the Selection itself: a listener has one Selection and zero or one push subscription per browser. Identified in logs by its **Id**, never by its endpoint (a stable per-device identifier).
_Avoid_: notification subscription, device token, registration.

**Coalescing**:
The rule that bounds notifications: at most one per **talkgroup** per push subscription per configured window, each carrying a count of the calls it stands for. A busy system must never storm a phone, and nothing is silently dropped for it.
_Avoid_: throttling, rate limiting, batching, debouncing.

### Ingest & distribution

**Ingest**:
Accepting a call from a recorder into Radio-Scout (via the HTTP upload API or, later, directory watching).
_Avoid_: upload, import (except in user-facing recorder docs).

**Auto-populate**:
Automatically creating an unknown system/talkgroup/unit the first time a call for it is ingested, so the archive is usable with zero manual configuration.
_Avoid_: auto-create, discovery.

**Access code**:
A listener-facing PIN that grants scoped viewing access to specific systems/talkgroups (with optional expiry and concurrent-connection limits). Distinct from an **API key**.
_Avoid_: password, passcode.

**API key**:
A recorder-facing secret that authorizes ingesting calls into specific systems. Distinct from an **access code**.

**Admin password**:
The single operator-facing secret that opens the admin surface — everything under `/api/admin/`, which configures the scanner. Distinct from both an **access code** (listener-facing, scoped) and an **API key** (recorder-facing). There is exactly one; it lives in the environment (`RADIO_SCOUT_ADMIN_PASSWORD`), not the database.
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

### Storage & retention

**Retention**:
The policy that bounds the archive: an age window (in days) plus an optional cap on total stored audio. Expressed as configuration; enforced by sweeps.
_Avoid_: expiry, TTL, cleanup.

**Sweep**:
One pass of the retention policy over the archive — age out, then enforce the size cap, then reclaim orphans. Runs at startup and on an interval.
_Avoid_: job, cron, scheduler run.

**Prune**:
Removing a call from the archive because retention says so: its metadata row first, then its audio object.
_Avoid_: delete, purge, evict (reserve _delete_ for a single row or object).

**Orphan**:
A stored audio object no call row points at — the residue of an ingest that failed after writing its audio, or of a prune interrupted between the row and the object. Reclaimed by **orphan-GC**, which spares anything written inside the grace period so it can't race an in-flight ingest.
_Avoid_: dangling blob, garbage.
