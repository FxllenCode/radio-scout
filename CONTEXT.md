# Radio-Scout

Radio-Scout ingests **Calls** from **Recorders** and distributes them to **Listeners** through a scanner-style web app. This glossary is the project's ubiquitous language — use these terms exactly, in code and in conversation.

**"Scanner" is an adjective here, never a noun.** "Scanner audio", "a scanner-style app" — fine. The nouns it used to stand in for each have their own word, because it was carrying four meanings at once: a running Radio-Scout is an **Instance**, a Listener's independent setup is a **Profile**, the software that feeds us Calls is a **Recorder**, and the physical radio hardware is out of scope for this glossary entirely.

## Language

### People

**Operator**:
The person who runs an **Instance** — installs it, points **Recorders** at it, and decides retention, storage and enhancement policy. Holds the **admin password**; the only person who needs one.
_Avoid_: admin, user, host, owner.

**Listener**:
The person who listens through the web app. Needs no account and no **Session**: their whole state — **Selection**, **Hold**, **Avoid**, **Profile** — lives in their own browser. One person is often both a Listener and the **Operator**; the terms name the role, not the human.
_Avoid_: user, client, subscriber, viewer.

### Core entities

**Call**:
A single recorded radio transmission (or conversation) — audio plus its metadata (when, which talkgroup/system, frequency, units heard). The atomic unit Radio-Scout stores and plays.

**A *transmission* is the radio event; a Call is the row.** Usually one each, but not always: a **Patch** or a multi-site System makes several **Copies** of one transmission arrive, and they become one Call. So "transmission" is the right word for what was on the air and never a synonym for the thing stored — which is what the avoid-list below means.
_Avoid_: recording, clip, transmission (for the Call itself), audio file.

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

**A Unit names itself.** Either alias a **Call** carries for a radio is enough to roster one — the alias an **Operator**'s recorder had configured, else the **OTA alias** the radio broadcast — and the configured one wins where both are there. A Unit that already has a name keeps it (**Auto-populate** fills unknowns, it never rewrites curation); a Unit with *no* name takes the first one offered, which is what an apparatus created for nothing but the **Range** it owns needs. A **Call** is shown under the first radio heard on it, resolved to the Unit that owns that Ref — so a fleet's portable reads as its apparatus.
_Avoid_: radio, source, subscriber.

**Site**:
A physical tower/receiver site within a system that a call was heard on. Discovered from traffic the way a **Talkgroup** is — never gated on **Auto-populate**, because a tower is the System's own infrastructure rather than a channel that could clutter a panel.

**A Ref identifies a Site and a name names it, and a Recorder may know either.** The rdio dialect's `site` is a number with no name behind it; **Mining** finds a name with no number beside it, because SDRTrunk sends no `site` field at all. Where only a name arrives, a Ref is **minted** — the lowest free one in that System, the same answer #8 gives a System that Trunk Recorder identified by name alone — and it is this Instance's own numbering rather than anything the radio network assigned. Which is why a Listener is shown the name wherever there is one.

**Duration**:
How long a **Call**'s transmission lasted. From the **Recorder**'s own metadata when it sent any, otherwise read from the audio's container header at **Ingest** — never by decoding. Every Call carries one except where neither could say, and "unknown" is distinct from "zero": an unmeasured Call matches no length filter.
_Avoid_: length (the recorder's word — TR's `call_length` — reserve it for the wire field), runtime, playtime.

**Emergency**:
The bit a radio sets on a transmission when its emergency button is pressed. A property of a **Call**, and separately of each **Unit** heard within one — the Call says somebody keyed it, the per-source flag says which radio did. Absent means no, never unknown.

**A mark is all it is.** An Emergency is shown, filtered and searched on, and **no Listener is told**: Radio-Scout does not notify ([ADR-0014](docs/adr/0014-no-notifications.md)). "Alert" was the word for what an emergency used to produce, and it is no longer a word this project uses. A **Webhook** an Operator configured may carry one to an address *they* chose, which is that Operator arranging their own inbox rather than this Instance waking anybody.
_Avoid_: alert, notification, panic, priority (the recorder's own unrelated field).

**Mark**:
What a **Recorder** or this Instance's own signal processing proved about a transmission, kept on the **Call**. There are two: the **Emergency** bit (#42) and a **Tone profile** match (#55). A closed vocabulary rather than a free-form tag — a Mark is something *proved*, where a **Group** or a **Tag** is something an Operator decided.

Marks are shown, filtered and searched on, and are the only thing a **Webhook** fires on. Nothing derived from speech is or will be one ([ADR-0013](docs/adr/0013-no-transcription.md)).
_Avoid_: flag (fine in prose about one bit, wrong for the set), alert, trigger, event.

**Tone profile**:
The per-talkgroup definition of a paging tone sequence that tone-out detection matches against a call's audio — an **ordered sequence** of tones rather than a fixed A/B pair, so one shape spells a single long group tone, Quick Call II's two, and an A-B-then-group run of three. Signal processing, not speech recognition — transcription is banned ([ADR-0013](docs/adr/0013-no-transcription.md)). A match **marks** the Call, the way an **Emergency** does, and like an Emergency it is shown rather than delivered. The mark records **which profile fired**, because an Operator with twelve stations on one dispatch channel is asking who was paged rather than whether somebody was.
_Avoid_: tone set, page definition.

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
The live feed switched off by the **Listener** — a hard off, and not a pause: the playing call stops, the **listening queue** clears, and the connection closes, because nothing is being listened to. Persists until switched back on; rejoining starts from now, never backfilling the silence. Nothing reaches the Listener in the meantime — Radio-Scout does not notify ([ADR-0014](docs/adr/0014-no-notifications.md)) — so switching it back on is the only way back in.
_Avoid_: offline (the network's state, not the listener's choice), disabled, standby.

**Feed down**:
The **live feed** not delivering because the connection isn't there — through no choice of the **Listener's**. The counterpart to **Feed off**, and the distinction is the whole point: Feed off is a decision and persists; Feed down is a condition and clears itself when the connection returns. A Listener is told which, because "nothing is happening" and "nothing is reaching you" call for different reactions.
_Avoid_: offline, disconnected, dropped, stale.

**Playback mode**:
The mode where the listener plays archived calls from the searchable history instead of the live feed. Mutually exclusive with live feed.
_Avoid_: archive mode, replay mode.

**Run**:
The ordered set of archived **Calls** a **Listener** is walking, and where they are within it — which **Call** is playing, what follows, and which page of the **Archive** has to be on hand for it. One concept with an ordering: walking search results newest-first, playing forward in time from a row, and a **DVR** across a time range are the same **Run** configured differently. A Run knows which search it belongs to, so a search that changes ends it rather than silently walking the wrong results — at the *boundary*, not mid-Call: what is loaded plays out, and what stops is rolling onto a page of a search the Listener has not seen. Identity is the search **compared structurally**, so a filter object rebuilt from the URL or cleared to `undefined` is the same Run.

A Run carries **its own window** into the Archive, which is not the window on screen. The Listener browsing ahead with the paging buttons moves the screen; the Run keeps walking, keeps naming the page after its own, and does not drag the visible list along behind it (#89).
_Avoid_: playlist, queue (the **listening queue** is a different set), walk, session — *for a Run*. The word is spoken for twice over and neither is this: the **Operator's** admin login, and the **Listener's** sitting since they opened the app, which is what the **Session log** records.

**Listening queue**:
The ordered set of not-yet-played live calls waiting to play. Its depth is the `Q` count in the display, and tapping that count is how a **Listener** sees into it — playing one now, letting one go, or giving up the backlog for the newest **Call** there is. Bounded — and it truncates in the same order it plays: lowest **Priority** first, then stalest. A queue that plays by one order and drops by another can starve the one talkgroup the **Listener** said mattered. Every queued Call carries the **ordinal it arrived at**, because arrival order is not recoverable from a queue that has been re-ordered — a Call demoted out of the Priority band has to fall back among Calls it once outranked, and nothing about where it currently sits can say which of them spoke first.
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
One named, independent **Listener** setup within a single browser — its own **Selection**, **Avoid** list, **Hold** state and Talkgroups-panel arrangement (**Pin**s, which Systems are folded away, the row order). Two Profiles behave as two entirely separate radios in the same browser: a "truck" Profile and a "desk" Profile share nothing. Spelled `namespace` in the client's persistence layer, which is the mechanism rather than the concept.
_Avoid_: namespace (in prose), workspace, preset, scanner.

**Priority**:
A **Listener's** per-talkgroup preference that makes its calls jump the **listening queue** instead of waiting their turn. Queue order, not selection — a priority talkgroup still has to be selected to be heard. It applies to what is *already waiting*, not only to what arrives next: a Listener reaches for it precisely when they are forty Calls behind, so marking a talkgroup re-orders the queue in hand. Part of a **Profile**, unlike a **Pin**, which cannot change what plays.
_Avoid_: preempt (SDRTrunk's stronger notion — interrupting the playing call — which this is not), favorite.

**Pin**:
Keeping a talkgroup at the top of the Talkgroups panel — in a section of its own, above every System, and the pinned row stays in its System as well. A panel-ordering affordance only; pins change nothing about what plays.
_Avoid_: favorite, star (a **Star** marks a Call).

**Catch-up**:
Draining the **listening queue** faster than real time — **Quiet spans** skipped, playback rate raised — until the feed is live again. A **Listener's** action on Calls they already have; distinct from a **Backfill**, which is how they got them. Ends when the queue is empty, because the Call playing then is the newest there is and hurrying through the present is not catching up.
_Avoid_: fast-forward, smart speed (a product's trademark), time compression, backfill.

**Quiet span**:
A stretch of one **Call** where nobody is talking, measured against how loud *that Call's* speech is and reported only when it is long enough to be worth skipping. What **Catch-up** jumps over. Found by a **Worker** reading the stored audio once, never on the ingest path and never by the browser — a client with one `<audio>` element and no WebAudio ([ADR-0005](docs/adr/0005-client-audio-media-session-background.md)) cannot look at a sample. A Call with none is the ordinary case, not a failure: somebody saying one thing has no hole in it, and Catch-up then raises the rate and trims nothing.
_Avoid_: silence (the keep-alive loop is the client's word for that, and a quiet span is not digital silence — it is a level relative to the speech beside it), dead air, gap (a **Backfill** has one, meaning something a Listener never got), VAD, speech detection (nothing here asks what was said — [ADR-0013](docs/adr/0013-no-transcription.md)).

**Session log**:
Every **Call** a **Listener** heard since they opened the app, newest first, replayable and reachable for a **Hold**, an **Avoid** or a download. Client-side and unpersisted — *this session* is what it says — and deeper than the RECENT list on the live screen, which is a five-deep replay control rather than a record. Each Call appears once, in the order it was **first** heard: a replay is hearing it again, not a new thing happening, and a list that reordered itself under "what was that ten minutes ago" would not answer the question.
_Avoid_: history (RECENT's five), **Backfill** (Calls the Listener did *not* hear), **Run** (archived Calls being walked), transcript. The *session* here is the Listener's sitting, never the **Operator's** admin login — the two share a word and nothing else, and only this one is a thing a Listener sees.

**Backfill**:
The **Calls** a **Listener** missed while **Feed down**, sent on reconnect so the **live feed** resumes without a hole. Ordered by when a Call was *emitted*, never by when it was stored — a **Delay**ed Call is stored early and emitted late, so a cursor over storage order would skip it silently. Bounded — past the bound the Listener is told their history has a gap only archive search can fill, because a silent truncation is indistinguishable from having missed nothing.
_Avoid_: catch-up (the Listener draining a queue, not the server refilling one), replay, resync, history.

**Emission**:
A **Call**'s place in the order Calls went out on the **live feed** — what a **Backfill** is read in and what a **Listener**'s cursor names. Deliberately distinct from the Call's identifier, which is the order rows were *stored*: the two coincide until something holds a Call back, and a **Delay** is exactly that. A Call that has been stored but not yet emitted has no emission at all, which is the honest reading — nobody has heard it.
_Avoid_: sequence number, offset, cursor (a Listener *holds* a cursor; its value is an emission), call id.

**Activity**:
How much traffic a stretch of time held, counted bucket by bucket. Read under exactly the filters a search was made with, so the **density ribbon** drawn over a page of results describes *those* results and its bars add up to their total — and so a chart of one channel is the same read with a **Talkgroup** filter set. Two pictures of it: the ribbon, which is when it *was* busy and is scrubbable, and the hour-by-day heatmap, which is when it *usually* is. The bucketing is the server's; the fold into an hour of the day is the browser's, because only the browser knows what timezone the **Listener** is in.
_Avoid_: volume, traffic (fine in prose, wrong for the measurement), stats, metrics (a **Metric** is what an Operator scrapes).

**Listener count**:
How many **Listeners** were connected at once, sampled onto an interval and kept as a series. A count and an instant and nothing else: no address, no session, no per-**Talkgroup** breakdown, because on a quiet channel that would be a record of *who* was listening ([ADR-0011](docs/adr/0011-observability-logging-policy.md) rule 5). Each sample is the **peak** since the one before it rather than a reading taken at the tick, so somebody who arrived and left between two ticks is still somebody who was there. The **Operator's**, not the Listener's — an open Archive does not make how many people listen to an Instance public.
_Avoid_: audience, traffic, users, sessions, analytics.

**DVR**:
The archive surface that plays one talkgroup (or a **Selection**) gaplessly across a time range, scrubbable on a call-density timeline. Oldest-first by construction — a DVR that plays backwards is a search result, not a DVR.

It is a **Run**, configured differently, and what it configures differently is that **its position is a time**. Scrubbing the **Activity** ribbon on the search screen moves the window of *results* and deliberately leaves the Run alone; scrubbing a DVR re-anchors the Run itself, because "rewind the county to 2am" names an instant rather than a row number. Its scope is always a **Selection** — a single talkgroup is a one-entry matrix — so a channel reached only through a **Patch** is one it plays, which the archive's own talkgroup filter is not.

**Catch-up**'s two levers apply inside one and the *state* deliberately does not: a DVR ends at the end of a range where Catch-up ends when the **listening queue** empties, so a Listener catching up cannot find a DVR already hurrying and a DVR cannot speed up the live feed it hands back to.
_Avoid_: time machine, rewind mode, tape, playlist (the word for what a DVR plays is a **Run**, and the "playlist" of #63 is the ordered Calls themselves, never a media format).

**Station stream**:
A continuous audio stream of a **Selection** — calls in order, silence-filled — for players that can't run the app (smart speakers, stream URLs, car radios).
_Avoid_: radio mode, icecast feed (the mechanism), broadcast.

### Ingest & distribution

**Recorder**:
The software that receives radio and uploads **Calls** to an **Instance** — Trunk Recorder or SDRTrunk. Authenticates with an **API key**. Both already speak the rdio-scanner upload dialect and need nothing installed to work, because that dialect is the compatibility contract. Trunk Recorder can do better than it, though, and there are two shipped ways to: `radio-scout-upload.sh` (#43) is a shell script for TR's own `uploadScript` hook that posts the recorder's whole `.json`, and it is the recommended TR setup because it needs no rebuild; `plugins/trunk-recorder/` (#44) is a first-party TR plugin sending the same thing from inside the recorder's process, which costs a TR rebuild and buys TR's own retry-with-backoff on a failed upload — something the script deliberately cannot have, since a non-zero exit from `uploadScript` skips every other plugin on the recorder.
_Avoid_: source, uploader, feeder, scanner.

**Ingest**:
Accepting a **Call** from a **Recorder** into Radio-Scout (via the HTTP upload API or, later, directory watching).
_Avoid_: upload, import (except in user-facing recorder docs).

**Admission**:
What **Ingest** decided about one **Call** — stored, **replaced** by a better **Copy** of the same transmission, duplicate, dropped for a named reason, or refused for an unauthorized **API key**. A *value*, not a response: **Ingest** resolves what the database knows, decides purely, then performs. The HTTP endpoints render an Admission into the rdio wire strings; **Dirwatch** logs one with no HTTP in reach. Its reason is a single closed vocabulary — the machine-readable slug and the recorder-facing detail derive from the same value, never two strings that can drift.
_Avoid_: disposition (the narrower auto-populate/blacklist decision *inside* an Admission), verdict, result, outcome.

**Candidate**:
A **Call** already stored on the same **System**, near enough in time that an arriving one has to be compared against it before an **Admission** can be decided. Rows rather than a count, because the decision has to name *which* Call a duplicate was of — and because keeping the better of two copies means comparing them. Scoped to the System rather than to one **Talkgroup**, since a **Patch** puts one transmission on channels that are genuinely different.
_Avoid_: match, neighbour, dupe.

**Copy**:
One **Recorder**'s upload of a transmission that Radio-Scout may already have. A Patch re-broadcast and a multi-site duplicate are two copies of one transmission, not two **Calls** — and which of them a **Listener** ends up hearing is decided by comparing them: fewer decode errors, then longer **Duration**. Deliberately a word for the *upload*, where **Call** is the row it becomes: N copies arrive and one Call exists.
_Avoid_: version, variant, instance (an **Instance** is a running Radio-Scout), dupe. (It collides with Rust's `Copy` trait, which is why it stays a prose term: the code names the two sides `Arriving` and `Candidate` instead.)

**Replacement**:
A later-arriving **Copy** that turns out to be better taking the stored **Call**'s place — its audio and everything the Recorder said about the transmission — **under the same Call id**. Not a new Call and not an edit to one: the id, the **Talkgroup**, the instant and the position in the **Archive** are what a **listening queue**, a **Run** and an open page are all keyed on, so they stay. Nothing is published to the **live feed** for one, because the Listener already has that Call and a second frame would play it twice. Bounded: a Call stops being replaceable once its dedup window has closed, which is what lets its audio URL be promised immutable.
_Avoid_: update, overwrite, upgrade. (*Swap* is fine for the **mechanism** — it is what **Enhancement** does to an object too — but a Replacement is the whole act, not the write.)

**Curation**:
What an **Operator** edits about the entities themselves — a **Talkgroup**'s label, LED and blacklisting, a **Unit**'s name, which **Group**s a channel belongs to, which **API key** a **Recorder** holds. Distinct from **Configuration**, which is the machine: ports, storage, retention and the credentials live in `radio-scout.toml` and the environment, and nothing curated is ever written there.

**Curation always wins over discovery.** **Auto-populate** fills blanks and **Mining** fills blanks; a name an Operator wrote down is never overwritten by either. And curation is per row: one entity, one request, so two browsers editing different channels cannot undo each other.
_Avoid_: admin (the *surface* curation happens on), configuration (the machine's), editing, management.

**Auto-populate**:
Automatically creating an unknown system/talkgroup/unit the first time a call for it is ingested, so the archive is usable with zero manual configuration.
_Avoid_: auto-create, discovery.

**Mining**:
Reading what a **Recorder** wrote *inside* a **Call**'s audio, and folding it into the **Archive**. SDRTrunk buries an ID3 tag in every MP3 it uploads carrying the radio alias and the tower name an **Operator** configured — none of which its upload dialect has a field for, and it has no plugin mechanism to teach. So the facts arrive already; nothing had ever looked.

Mining **fills and never overwrites**: the wire is the Recorder speaking now and the container is a snapshot it wrote earlier, so where both answer the live one is believed, and a curated name survives (the **Auto-populate** rule, one layer up). A name is applied only to a radio the Call actually heard — the tag names one radio at one moment, and hanging it on whichever **Unit** the Call happens to list would put an apparatus's name on a different apparatus.

It happens twice: at **Ingest**, in the same pass that reads a Call's **Duration**, because the live-feed frame goes out there and nothing republishes one; and as a **Sweep** over the Archive that was already there. It is always a **read** — mining never rewrites an audio object, which is what keeps a stored Call's bytes immutable.
_Avoid_: tag mining (**Tag** is a Talkgroup's service label), scraping, extraction, transcription (which is banned outright, [ADR-0013](docs/adr/0013-no-transcription.md), and is about *speech* — this is about a file header).

**Downstream**:
Another instance this **Instance** forwards matching **Calls** to, speaking the rdio upload dialect, scoped per System/Talkgroup. Forwarding only — *receiving* a peer's downstream is just **Ingest** with an API key.

**A peer's outage costs delay, not Calls.** A matching Call is written to a durable queue inside the same transaction that stores it, so "the Call exists" and "the Call is owed to this peer" are one fact a crash cannot separate; the queue drains **in order, per peer**, one attempt at a time, and survives a restart of either end. The scope is a **Selection** — the live feed's own — so a **Patch** reaches a peer subscribed to the channel it was patched onto.
_Avoid_: relay, mirror, federation, upstream.

**Sink**:
Somewhere an **Instance** delivers **Calls** to, over a durable queue it drains in order. There are two — a **Downstream** and a **Webhook** — and the word exists because everything *about the queue* is the same for both: a delivery row written inside the transaction that stores the Call, one attempt in flight per sink, head-first draining, an exponential backoff, and one question deciding a retry (*will these same bytes ever be accepted?*). What differs is the errand: one POSTs a Call's audio in the rdio dialect, the other POSTs a marked Call's facts as JSON.

So a failure reads `sink-refused (404)` whichever it was, and a log line carries `sink=downstream` or `sink=webhook` beside it. Deliberately **not** *peer*, which this glossary spends on a Downstream alone: a Discord channel is not a peer of anything.
_Avoid_: peer (a **Downstream** specifically), target, destination, subscriber, endpoint (the far end's own word for its URL, not ours for the relationship).

**Webhook**:
An **Operator**-configured URL that receives the **Calls** they asked to hear about — one carrying an **Emergency**, or a **Tone profile** match — as JSON, optionally Discord-shaped. The automation escape hatch, and a sibling of **Downstream** rather than of anything listener-facing: an Operator wiring up their own inbox is a different act from this Instance waking a **Listener**, which it does not do ([ADR-0014](docs/adr/0014-no-notifications.md)). Delivery is retried and never blocks anything; the URL is a secret and is never logged.

**A Webhook fires on a Mark *and* a scope**, and both halves are the point: an Emergency on a channel this Webhook was never given is somebody else's Emergency, and a Webhook asking for no Mark at all fires for nothing, which is the safe direction. There is deliberately no "every Call" trigger — that is what a **Downstream** is for, and a county's routine traffic posted into a chat room would exceed the far end's rate limit within a minute.

**The URL *is* the credential**, which is the one way this differs from a Downstream (whose URL is public and whose key is the secret). So it is never returned, never logged, and — alone among curated entities — **never exported** in the configuration document, because a backup that carries it stops being a file an Operator can email. What the admin listing shows instead is the URL's *host*.
_Avoid_: integration, callback, alert, notification.

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

**Worker**:
A background task an **Instance** owns and can account for. There are nine — the **Retention** sweeper, the **enhancement** worker, the **Mining** backfill, the **Downstream** sender, the **Webhook** sender, the tone-out detector, the **Quiet span** scanner, the **listener sampler**, and the operator log writer — and every one has the same envelope: started exactly once, stoppable, joinable, and readable as a **depth** (work admitted and not yet settled) plus a count of what it has finished. The loops themselves differ and are meant to: a ticker, a bounded queue, a broadcast subscription and a batching drain are not one shape. Work is owed from where it is *handed over*, never from where it is picked up — which is what makes "this Instance has settled" a fact an **Operator** can be shown and a test can wait on.

What one *unit* of that work is belongs to the Worker, and is not always one item: the Downstream sender's is "I have caught up with what was handed to me", because a delivery waiting out a retry is owed by nobody — and a Worker that stayed non-idle through a peer's outage would make "this Instance has settled" unanswerable for as long as the outage lasted. The **Webhook** sender is the same Worker shape drained by the same code (`crate::delivery`): the two differ in what one delivery *is*, not in how a queue is drained.
_Avoid_: job, task, background thread, daemon (a **Service** is the operating system's).

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
The policy that bounds the **Archive**: an age window (in days) plus an optional cap on total stored audio, overridable per System/Talkgroup (unset inherits). Expressed as configuration; enforced by sweeps.

A **Star** buys a Call a *longer age window* where the Operator has configured one (#66) — not an exemption, and never one from the **size cap**, because a cap a Listener can defeat is not a cap and this is the one policy defeasible by somebody holding no credential at all. **Event** members are not exempt either, and #67 built what this entry planned: they are frozen by *copying* their audio at curation time, so the sweep stays one pass over one archive and never has to know an Event exists. The sweep therefore still has exactly one exemption, and it is the Star's window — what the copies get instead is to be **counted by the size cap and never taken by it**, so `max_size_gb` keeps describing the disk.
_Avoid_: expiry, TTL, cleanup.

**Share link**:
An expiring public URL for one **Call**, minted by a **Listener** and opening a page with that Call
on it and nothing else — no search, no catalog, no other audio. What it exists for is that sharing a
moment should not mean sharing the **Instance**.

**One live link per Call, and that is the abuse bound rather than a convenience.** Minting needs no
credential, because a Listener holds none — so a table keyed on anything but the Call could be
filled by anybody who can POST in a loop. Keyed on the Call it is bounded by the Archive, which
**Retention** already bounds. Sharing a Call that is already shared therefore hands back *the link
already in circulation* with its window pushed out, and two Listeners sharing one Call share one
link.

**The link is the credential**, the way a **Webhook**'s URL is: never returned by the admin listing,
never logged — which is why the token rides a URL's *query string*, the one part of a request that
has never been written down (ADR-0011 rule 2). Revoking deletes the row, so that URL is dead for
good; sharing the Call again mints a different one, which is exactly what revoking a leaked URL
should mean. An expiry is a promise about the link that was handed out, so an expired one is
replaced rather than resurrected, and a recipient is told which of "expired" and "never existed"
they have hit.

Deliberately not an **Event** (a curated collection frozen against Retention) and not an **Access
code** (a scoped credential for the whole Instance): a Share link is one Call, for a while, to
whoever holds it. The contrast with an Event is sharp on every axis and each difference is the same
difference — who is publishing: a Listener mints this one and an Operator curates that one, so this
one expires and is bounded by the Calls table where that one is a toggle and bounded by the gate.
_Avoid_: public link (fine in prose, wrong for the entity), permalink (it expires), token (the
secret *inside* one), embed (#75's iframe page is a different thing).

**Export**:
A range of the **Archive** taken away as a file — the **Calls** a search matched, in one of two
shapes. A **zip** holds each Call as its own file plus a `manifest.json` describing all of them; a
**stitch** holds all of them end to end as one playable WAV. Oldest first in both, because an
incident has one useful order and a stitch has one legal one.

**The filters are the search's own**, read by the same parser, so an Export is *what was on screen*
— including a **Selection**, which is how the **DVR**'s scope exports without the exporter knowing
what a DVR is.

**A stitch declares its timeline before it reads any audio.** A WAV states its length in its first
44 bytes, so the length is summed from what the Archive already measured (`duration_ms`) and each
Call is then *fitted* into the room that sum reserved for it — padded with silence where it fell
short, trimmed where it ran over. That is what buys a single decode pass, an exact
`Content-Length`, and a memory cost of one Call. Two consequences: a Call whose length was never
measured is **not in a stitch at all** (the kerchunk filter's own rule — a threshold cannot be
tested against an unknown), and neither is an **Encrypted Call**; and an object that has gone
missing since the pre-pass becomes silence of exactly its declared length, because a valid header
over a short body does not fail, it plays as every later Call being the wrong one.

Deliberately not a **Sweep**, not a **Backfill** and not a **Downstream**: nothing is stored, queued
or forwarded — an Export is one request, streamed, that leaves the Instance exactly as it found it.
_Avoid_: download (one Call's audio, spec US 27), archive (the zip is one; **Archive** is the
Instance's whole holding), bundle, dump.

**Star**:
A mark any **Listener** may leave on a **Call** — filterable in search, and kept past the **Retention** window as far as the **Operator** allows.

**It is the Instance's mark, not a browser's** (#66, reversing this entry's original "per-browser"). Starring takes no credential, because a Listener holds none — and the two ways of making it personal both cost something this project has already refused twice: a `call_stars(call_id, starrer)` table is a per-browser record of what somebody kept, which is the listening history ADR-0011 rule 5 exists to stop an Instance accumulating; and it is a table whose rows are Calls × browsers rather than bounded by the Calls table, which is the **Share link**'s abuse bound inverted. So a Star is one nullable column on the Call, anybody can set or clear it, and "starred" means *somebody here thought this mattered*. The shared shortlist is what that buys, and on the county instance this was designed against it is a handful of people.

**A column and not a child table**, which is the same decision seen from the other side: `call_tones` and `share_links` each had to be remembered in the delete that **Retention** runs, and forgetting one is invisible until the **Sweep** meets its oldest marked Call and fails there forever. A column goes with its row.

**What it is worth is the Operator's** — `[retention] starred_days`, absent by default, so out of the box a Star is a bookmark. A *window* rather than a switch, because that is what bounds the two ways this could go wrong on its own: a Listener who stars a thousand Calls, and a Star nobody will ever come back for. What this Instance will do rides on the catalog beside `sharing`, so the control can say it.
_Avoid_: favorite, bookmark, like.

**Event**:
A named, curated collection of **Calls** — an incident assembled by hand — frozen against **Retention**, shareable by link, exportable as audio. The one thing in the **Archive** that is meant to outlive it.

**A member is a snapshot, not a pointer** (#67). Freezing *copies* the Call's audio under a key of its own and stores the Call's own wire document beside it, so an Event is readable and playable when nothing it was made from survives — and **Retention**'s sweep stays one pass over one archive, which is what the copy buys. The row carries no foreign key to the Call, which is the **Share link**'s rule exactly inverted: a share link is a child that goes *with* its Call, so the prune must name it; a frozen member must *survive* that same prune, so it must not appear there and must carry nothing that would make it fail.

**It is the Operator's, and that is a departure from the story it came from.** Spec US 38 is a **Listener**'s, and every other Listener-facing mark here takes no credential — but a **Star** is bounded by a window the Operator sets and by the size cap that outranks it, and a Share link is bounded by the Calls table. Freezing is bounded by *nothing*: it spends disk permanently and no policy here can reclaim it. So curating one takes the admin session; a Listener still receives, plays and exports a shared Event.

**Frozen bytes are counted by the size cap and never taken by it.** They are stored audio on the same disk, so a cap blind to them would stop being true the moment an incident was curated. When Events alone exceed the cap the sweep empties the Archive around them and then stops, saying so — the visible failure rather than the quiet one.

**The link does not expire**, alone among this project's public links, because neither half of a Share link's reasoning applies: the Operator is the one publishing, and an Event is the durable thing by definition. What replaces the expiry is a **toggle**, and turning it off is a *revoke* — the token is cleared, so that URL is dead for good and sharing again mints a different one. It is the only revoke there is here.
_Avoid_: incident (the real-world happening, not the collection), playlist, compilation.

**Sweep**:
One pass of a periodic policy over the **Archive**, run at startup and on an interval. There are two: **Retention**'s — age out, then enforce the size cap, then reclaim orphans — and **Mining**'s, which reads the Calls nothing has looked inside yet. Both are bounded per pass and resume where they stopped, because an Instance that restarts more often than its interval must still make progress.

Deliberately not a **Backfill**: that is the Listener's missed Calls, replayed on reconnect, and the two have nothing in common but the direction of travel.
_Avoid_: job, cron, scheduler run, backfill.

**Prune**:
Removing a call from the archive because retention says so: its metadata row first, then its audio object.
_Avoid_: delete, purge, evict (reserve _delete_ for a single row or object).

**Orphan**:
A stored audio object no call row points at — the residue of an ingest that failed after writing its audio, or of a prune interrupted between the row and the object. Reclaimed by **orphan-GC**, which spares anything written inside the grace period so it can't race an in-flight ingest.
_Avoid_: dangling blob, garbage.
