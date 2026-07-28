# Audio lives in object storage, served through our own range endpoint

## Context

rdio-scanner stores call audio as BLOBs in the database and ships it to the browser as a JSON integer array over the WebSocket, which the client decodes with the WebAudio `AudioContext`. This bloats the database (audio is ~99% of data volume) and — because iOS suspends `AudioContext` in the background and won't attach lock-screen transport controls to it — is a primary cause of rdio-scanner's broken iOS background audio. Fixing iOS background audio is a headline requirement for Radio-Scout.

## Decision

Audio is stored as objects behind a single **S3-compatible storage interface** (the Rust `object_store` crate), never in the database. The database holds only metadata, including the object key.

- **Default backend (zero-config): local filesystem** under `base_dir`, so "one binary, just works" and backup is still "copy one folder."
- **Opt-in backend: S3-compatible object storage**, with **Garage** as the first-class recommendation (Rust, self-hostable, Pi-friendly, optionally multi-node). MinIO/AWS/etc. also work via the same interface.

Audio is served through Radio-Scout's own `GET /api/call/:id/audio` endpoint **with HTTP range support** by default. For the **S3/Garage backend**, the server may instead issue a **short-lived presigned URL** *after* an access-scope check, letting the client fetch audio directly from the object store and relieving the app of proxying bandwidth for remote-store, many-listener deployments; the filesystem/local backend always proxies. Either way the client plays via an HTML5 `<audio>` element + the Media Session API, **not** WebAudio, and never needs to know the backend.

**Write/delete ordering (consistency):** ingest writes the audio object **then** inserts the DB row (a row always has its audio); pruning deletes the DB row **then** the object (an archive row never points at missing audio). A periodic **orphan-GC** sweep removes any object with no row. This prevents both dangling rows (playback 404s) and orphaned blobs.

**Orphan-GC needs a write grace period** (#10). The write ordering above means an object legitimately has no row for the span between the two writes, so an unconditional "no row → delete" sweep would race in-flight ingests and delete a live Call's audio. GC therefore only reclaims objects whose last write is older than a configurable grace period (default 1 h, comfortably longer than any ingest). Retention also records each Call's `audio_size` at ingest, so the optional total-size cap is one `SUM()` rather than a stat per object — which on a remote S3/Garage store would be a network round-trip each, every sweep.

## Amendment (#39, 2026-07-28): the S3 backend retries a blip, not an outage

The decision above adopted `object_store` and said nothing about how it should behave when the store does not answer, so it ran on the crate's defaults: **10 retries over a 180 s ceiling**, with a randomized backoff climbing to 15 s a sleep. That is a policy written for a fleet talking to AWS, and it is the wrong one at both ends of this project's range.

On a Pi it is a **worker slot held for minutes over a single Call** while the Garage box is down — enhancement (#20) runs a bounded queue, so one Call sitting in a backoff schedule is capacity the rest of the queue does not get. And because each sleep is a *random draw* rather than a fixed step, the time to surface a dead store is a variable whose tail runs past a minute, so "how long until this fails" had no answer anyone could design against. That is what made the unreachable-store tests intermittent rather than simply slow (`tests/enhance.rs`, `tests/archive.rs`).

So `BlobStore::s3` now passes an explicit `RetryConfig` (`blob::retry_policy`): **4 retries, a 5 s ceiling, backoff 100 ms → 1 s**. A store answering `503` while it sheds load is still ridden out; a store that is actually down surfaces as an error in **about a second and a half**, and the layer above decides what that means — the enhancement worker settles the Call as `skipped` and takes the next one, ingest answers the recorder with a failure the recorder retries on its own schedule. Neither outcome is improved by waiting three minutes for it, and a store that is *restarting* takes longer to come back than any retry schedule worth having would wait.

The bound is on **retrying**, not on a request. `retry_timeout` gates whether a further attempt is scheduled; it does not abort one in flight, so a store that accepts a connection and then stalls is bounded by `ClientOptions`' own 30 s request timeout instead. That default is left alone deliberately: the request body is a Call's audio, and a tighter timeout would start failing real uploads over a slow link — a worse trade than the one it would buy.

It is a constant rather than a `[storage.s3]` setting: no operator has needed a different number, and a knob costs a TOML key, a `RADIO_SCOUT_*` spelling, validation and a template line. Promoting it later is cheap; un-shipping a setting is not.

## Consequences

- The database stays small regardless of archive size, which is what makes SQLite viable as the default (see [ADR-0003](0003-database-sqlite-postgres.md)).
- Storage backend is a config flag, not an architecture fork; switching filesystem↔Garage requires no code change.
- Serving through our own endpoint keeps access-control/scoping centralized; the presigned-URL path preserves scoping by checking access *before* issuing a time-limited URL. Presigned direct-fetch is a **v1 option for the S3 backend** to keep the app from becoming an audio-proxy bottleneck at scale.
- Real URLs + `<audio>` + Media Session is the mechanism that makes iOS background playback and lock-screen controls work.
