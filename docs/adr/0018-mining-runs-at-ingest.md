# Mining runs at Ingest, not in an off-path Worker

*Decided while building #48 (2026-08-11). Written down as an ADR by #110 (2026-09-24), because #48's own acceptance criteria said the opposite.*

## Context

**Mining** reads what a Recorder wrote inside a Call's audio container: the ID3v2.4 tag SDRTrunk's `AudioSegmentRecorder.recordMP3` puts ahead of the MPEG frames. That tag carries the FROM radio's alias list (`TPE1`) and the site, decoder and frequency (`COMM`), none of which the rdio dialect sends. SDRTrunk cannot have a plugin (its broadcast formats are compiled-in classes), so Mining is its integration.

#48 said to do this in "the off-path audio worker", following the rule that nothing expensive runs on the ingest path. That rule was written about **Enhancement**, which decodes and re-encodes the audio.

## Decision

**Mining runs at Ingest, in the pass that already reads the audio for its Duration.** Three facts make the worker the wrong place for it:

1. **The live-feed frame is published at Ingest, and nothing republishes one** (#46). A name that lands afterwards never reaches the Listener who heard the Call live.
2. **Enhancement rewrites the stored object and destroys the tag.** A mining worker would race it and lose.
3. **The cost is a header read**, done in the pass `audio_meta::read` was already making. That is not the decode-and-encode the "never on the ingest path" rule was written about. A three-byte `starts_with(b"ID3")` gate keeps a container parse off every Trunk Recorder upload.

The Archive that existed before Mining is handled by a separate **Sweep** Worker (`src/mining/sweep.rs`). It only ever reads a stored object once, and never touches audio bytes.

## Consequences

- Ingest gains a small, gated parse. Its fill-never-overwrite rule, the `TCOM` gate and the Ref cross-check are described in `src/mining/mod.rs`.
- There are two writers, `mining::apply` for a Call being assembled and `repo::apply_mined` for an old one. `tests/mining.rs::both_paths_land_the_identical_call` holds them to the same result.
- A future container field worth reading at Ingest takes the same path. Anything that needs a **decode** still belongs in a Worker.
