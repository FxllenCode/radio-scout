# A Star is the Instance's mark, not a browser's

*Decided while building #66 (2026-09-03). Written down as an ADR by #110 (2026-09-24), because it goes against the ticket that asked for it.*

## Context

Spec US 37 and #66 asked for a **per-browser** Star: each Listener keeps their own shortlist of Calls, and an Operator can set it so starred Calls are kept past **Retention**.

A **Listener** holds no credential (ADR-0008), so "per browser" has to mean one of two things, and each costs something this project had already refused.

- **A `call_stars(call_id, starrer)` table** is a per-browser record of what somebody kept. That is the listening history ADR-0011 rule 5 exists to stop an Instance accumulating, and it is why #62 gave `listener_samples` three columns. Its rows are also Calls × browsers rather than bounded by the Calls table, which reverses #64's abuse bound (one Share link per Call, bounded by a table Retention already bounds).
- **Keeping the set in `localStorage` and sending it with each search** gives per-browser filtering. It still needs a server-side keep for Retention, and that drifts every time somebody clears their site data.

## Decision

**A Star is the Instance's.** Anybody sets or clears it, it takes no credential, and it records no starrer. Everybody's "Starred" filter is one shared shortlist.

- **It is a nullable column on `calls`, not a child table.** A column goes with its row, so `repo::delete_calls` has nothing to forget. #55 and #64 each added a child table that had to be remembered there, and forgetting one breaks the retention sweep for good. `stored_calls` reads the column for no extra statement.
- **What a Star is worth is the Operator's decision:** `[retention] starred_days`, absent by default, so a Star is a bookmark until they say otherwise. It is a longer age window, not a switch. That bounds both ways an unauthenticated keep goes wrong: a Listener who stars a thousand Calls, and a Star nobody comes back for. Boot refuses a window shorter than `days`. **The size cap outranks it**, because a cap a Listener can defeat is not a cap.
- **The catalog says what a Star is worth** (`starred: { kept, keptDays }`), so a control never means less than the Listener thinks it does.

## Consequences

- On a public county Instance the shortlist is shared: one Listener's Stars are visible to all of them. That cost is accepted, and it is what made the feature affordable.
- No per-browser starred filter is possible without reopening this ADR. Any design that does must answer rule 5 and the abuse bound first.
- The client keeps an in-session override map (`client/src/store/stars.ts`) so that a tap feels instant. It is deliberately not persisted, because the durable record is the Instance's.
- The mechanics are described in `src/star.rs` and `crate::retention`.
