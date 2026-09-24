# Curating an Event is an Operator's act

*Decided while grilling #67 (2026-09-11), before the first test. Written down as an ADR by #110 (2026-09-24), because it departs from the spec story it implements.*

## Context

Spec US 38 is a **Listener**'s story: collect the Calls of an incident into a named **Event** that outlives **Retention**. An Event outlives Retention because it **freezes** its members: it copies each Call's audio under a key of its own at curation time, which is what keeps the retention sweep one pass over one archive (see `src/event/mod.rs`).

A Listener holds no credential (ADR-0008). Everything else a Listener can write without one is bounded:

- a **Share link** is one per Call, bounded by the Calls table, which Retention bounds (#64);
- a **Star** is worth only the window an Operator sets (#66, ADR-0016).

Freezing is bounded by **nothing**. It spends disk permanently, and no policy here can reclaim it, which is the feature working as intended.

## Decision

**Creating, filling, renaming and deleting an Event is admin-gated.** Curation lives under `crate::curate`, where the gate is a property of the route prefix. A Listener still **receives, plays and exports** a shared Event: its page (`src/event/page.rs`) and its downloads are open, borrowing `[share]` and `[export]` for their switches.

An unauthenticated POST that commits a Pi's SD card for good is exactly what #66 refused to build. Making US 38 a Listener's story would need a bound, and none exists that keeps the feature useful.

## Consequences

- On the client, multi-select and "Add to Event" live on the Search screen and are **absent**, not disabled, for a Listener.
- Frozen bytes count against `[retention] max_size_gb` and are never taken by it. When the frozen audio alone exceeds the cap, the sweep stops and reports it once. That is an Operator's decision to make, which is one more reason the act belongs to one.
- Reopening this ADR needs a bound on how much a stranger can freeze, not only a credential.
