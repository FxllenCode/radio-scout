# Audio lives in object storage, served through our own range endpoint

## Context

rdio-scanner stores call audio as BLOBs in the database and ships it to the browser as a JSON integer array over the WebSocket, which the client decodes with the WebAudio `AudioContext`. This bloats the database (audio is ~99% of data volume) and — because iOS suspends `AudioContext` in the background and won't attach lock-screen transport controls to it — is a primary cause of rdio-scanner's broken iOS background audio. Fixing iOS background audio is a headline requirement for Radio-Scout.

## Decision

Audio is stored as objects behind a single **S3-compatible storage interface** (the Rust `object_store` crate), never in the database. The database holds only metadata, including the object key.

- **Default backend (zero-config): local filesystem** under `base_dir`, so "one binary, just works" and backup is still "copy one folder."
- **Opt-in backend: S3-compatible object storage**, with **Garage** as the first-class recommendation (Rust, self-hostable, Pi-friendly, optionally multi-node). MinIO/AWS/etc. also work via the same interface.

Audio is served through Radio-Scout's own `GET /api/call/:id/audio` endpoint **with HTTP range support** by default. For the **S3/Garage backend**, the server may instead issue a **short-lived presigned URL** *after* an access-scope check, letting the client fetch audio directly from the object store and relieving the app of proxying bandwidth for remote-store, many-listener deployments; the filesystem/local backend always proxies. Either way the client plays via an HTML5 `<audio>` element + the Media Session API, **not** WebAudio, and never needs to know the backend.

**Write/delete ordering (consistency):** ingest writes the audio object **then** inserts the DB row (a row always has its audio); pruning deletes the DB row **then** the object (an archive row never points at missing audio). A periodic **orphan-GC** sweep removes any object with no row. This prevents both dangling rows (playback 404s) and orphaned blobs.

## Consequences

- The database stays small regardless of archive size, which is what makes SQLite viable as the default (see [ADR-0003](0003-database-sqlite-postgres.md)).
- Storage backend is a config flag, not an architecture fork; switching filesystem↔Garage requires no code change.
- Serving through our own endpoint keeps access-control/scoping centralized; the presigned-URL path preserves scoping by checking access *before* issuing a time-limited URL. Presigned direct-fetch is a **v1 option for the S3 backend** to keep the app from becoming an audio-proxy bottleneck at scale.
- Real URLs + `<audio>` + Media Session is the mechanism that makes iOS background playback and lock-screen controls work.
