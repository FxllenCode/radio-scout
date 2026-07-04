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
