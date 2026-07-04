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
