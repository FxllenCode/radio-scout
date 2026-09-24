# Tone detection is a Worker of its own, not part of Enhancement

*Decided while building #55 (2026-08-21). Written down as an ADR by #110 (2026-09-24), because #55 said to put it in "the off-path audio worker".*

## Context

A **Tone profile** marks a Call when a page-out is found in its audio (#55, spec US 20). Detection needs the audio decoded and transformed, so it belongs off the ingest path. The ticket named the Enhancement worker as the place.

**`[enhancement] mode` is `off` by default.** A detector folded into that worker would never run on a stock install. An Operator would configure a profile, see no page-outs, and have no way to tell a pager that is not being watched from one that has not gone off.

## Decision

**The tone detector (`src/tone/worker.rs`) is a separate Worker.** Its gate is whether an enabled profile exists (`Tones::armed`, one cached bit), not whether Enhancement is on.

It shares `crate::enhance::decode`, so a page is looked for in the same audio that gets levelled. It keeps its own queue, state column (`calls.tone`) and restart behaviour.

## Consequences

- **An Instance running both pays one extra decode per Call.** That is the cheaper half: detection decodes and transforms, where Enhancement decodes, resamples twice, filters, measures loudness and re-encodes.
- A Call on a Talkgroup with no profile is settled *without its object ever being read*, so on most Instances the Worker costs nearly nothing.
- The same reasoning later applied to **Quiet spans** (#59, `src/quiet/`). That is a third Worker, on by default, for the same reason: a feature that ships on cannot hang off one that ships off.
