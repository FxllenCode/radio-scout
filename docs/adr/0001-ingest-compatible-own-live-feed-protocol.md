# Backward-compatible ingest API, but our own live-feed protocol

## Context

Radio-Scout replaces rdio-scanner. rdio-scanner's license (API_ACCESS_POLICY.md, effective 2026-01-03) splits its API: the **HTTP REST ingest API is fully open (GPL)**, but the **WebSocket API is proprietary** and explicitly prohibits reverse-engineering, replicating, or redistributing it. Recorders (Trunk Recorder, SDRTrunk) already push calls to rdio-scanner's ingest endpoints.

## Decision

Radio-Scout implements an ingest surface that is **byte-compatible with rdio-scanner's open HTTP ingest API** — `POST /api/call-upload` and `POST /api/trunk-recorder-call-upload`, including the load-bearing response strings clients depend on (SDRTrunk requires HTTP 200 + `Call imported successfully.`, health-checks on `incomplete call data: no talkgroup`, and drops-without-retry on `duplicate call rejected`) and duplicate detection. Existing recorder setups migrate to Radio-Scout with only a URL change.

We design our **own** real-time live-feed protocol from scratch. We do **not** reverse-engineer or replicate rdio-scanner's proprietary WebSocket API. A richer *native* ingest API plus first-party Trunk Recorder / SDRTrunk plugins come in a later phase (see [ADR-0002](0002-audio-object-storage.md) for how audio is delivered).

## Consequences

- Real-world testing can start immediately against the maintainer's existing Trunk Recorder on the Pi — no new plugin to install.
- Our client and live-feed protocol are unconstrained by rdio-scanner's design, which is what lets us fix the iOS background-audio problem (see ADR-0002).
- We must faithfully reproduce the exact ingest response strings and field-parsing quirks (Unix-seconds timestamps, TR's `sources` array vs SDRTrunk's singular `source`, `talkgroupTag`/`talkgroupName` vs `talkerAlias`, etc.) for drop-in compatibility.

## Amendment (#96, 2026-08-02): the promises keep, the shape changes

This decision's two operative promises — **duplicate detection**, and the ordering that keeps the archive consistent with the object store (ADR-0002: "ingest writes the audio object **then** inserts the DB row") — were both carried by the shape of one long function. #96 makes ingest **resolve, then decide purely, then perform**, and answer with an **Admission** ([`CONTEXT.md`](../../CONTEXT.md)) rather than an HTTP response. Both promises survive, differently held:

- **Duplicate detection is now a decision over rows, not a `COUNT` over a window.** One query reads the Calls already stored on the resolved channel inside `±dedup_window_ms`; a pure function decides which — if any — the arriving Call duplicates, and names it. Same query count, same inclusive edges, same rdio-compatible `duplicate call rejected`. What changes is that the decision is a value: the window's near misses are property-tested at both edges, including the out-of-order backfill case a recorder produces when it catches up after a blip — the case a surviving mutant lived in. #46's keep-best needs the candidate rows anyway, to compare the copies.
- **The write-before-row ordering is now a property of the types.** The recorder's facts (`NewCall`) no longer carry the object key or the byte length; those are `blob::StoredAudio`, which only a completed write produces, and `repo::insert_call` takes one. A row naming an object nobody wrote is no longer expressible, where before it was prevented by a comment. The orphan invariant is unchanged: a failed insert after a successful write still leaves an object for #10's grace-period GC to reclaim, and an **Encrypted Call** still stores no object at all (`None`, spec US 9).
- **The wire contract is unchanged and now pinned as one artifact.** Every Admission's status and body — including the two rejections that answer `200 Call imported successfully.` so a recorder never retries — is snapshot-tested together, because a diff there is a diff every recorder in the field would see.

The reason ingest stopped returning a response is #72: **Dirwatch** ingests Calls with no HTTP request to answer, and would otherwise have had to re-implement this pipeline. It also moves ADR-0011 rule 3's guarantee: an Admission is written down where it is *decided*, so a Call refused with nobody waiting for an answer still leaves its line.
