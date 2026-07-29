# No transcription — speech-to-text is permanently out of scope

## Context

Transcription is the single most-demanded capability in this product's space. It is the top-voted idea on rdio-scanner's own tracker (asked for accessibility first — "for people who are hearing impaired" — and for keyword search second), an entire DIY ecosystem exists to bolt Whisper onto Trunk Recorder (trunk-transcribe/CrimeIsDown, RadioTranscriber, half a dozen faster-whisper dashboards), Broadcastify is building a paywalled transcription product, and OpenScanner — the closest active competitor — ships built-in Whisper with transcript search. The 2026-07 v2 research surfaced all of this, and transcription's derivatives (keyword alerts, transcript search, transcript-derived geocoding/maps) ranked at the top of the community-demand list.

The maintainer weighed exactly that evidence during the v2 grilling session and banned it anyway.

## Decision

**Radio-Scout does not transcribe audio. Ever.** No speech-to-text in any form: no local model, no cloud API, no plugin hook, no sidecar contract, no "experimental" flag. No feature may depend on a transcript existing — which rules out keyword alerts, transcript search, captions, and transcript-derived geolocation, not just the transcriber itself.

This is a maintainer decision (2026-07-29), made in full knowledge of the demand above — so its being popular is not grounds to reopen it. It is also consistent with, but stronger than, the v2 compute posture (strictly on-box, Raspberry Pi included): the ban is absolute, not a feasibility judgment that better models could later overturn.

Non-speech audio DSP is a separate question and explicitly not covered: the enhancement pipeline (ADR-0006) and tone-out detection (two-tone/Quick Call pattern matching) analyze signal, not speech, and remain in scope.

## Consequences

- Alerting and search are built on metadata and DSP only: talkgroup/unit activity, the recorder-supplied emergency and encrypted flags, tone-out detection, duration/unit/site filters. They are designed to be complete on those terms, never as placeholders awaiting transcripts.
- The accessibility ask that motivated the original community request (captions for hearing-impaired listeners) goes deliberately unserved; competitors offer it and Radio-Scout does not. This is the real cost of the decision and is accepted, not overlooked.
- Competitive differentiation shifts to what the ban leaves untouched: listening UX, alerting on recorder metadata, archive/sharing tooling, and operator observability.
- Contributors and agents must not re-propose transcription, scope features that assume it, or add seams "in case it changes." The rule is mirrored in `CLAUDE.md`'s hard constraints; this ADR records why.
