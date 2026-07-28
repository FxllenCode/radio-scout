# Radio-Scout — Development Workflow

**The definitive guide to how work gets planned, designed, and built in this repo using the agent skills.**

If you only remember one sentence: **decide with `/wayfinder` and `/grill-*`, build with `/implement` and `/tdd`, and everything flows through GitHub issues.** The rest of this doc is the detail.

> Not sure which skill fits a situation? Run **`/ask-matt`** — it's a live router over these same skills. This file is the written, Radio-Scout-specific version of what it tells you.

---

## 1. The one-minute mental model

Every skill lives in one of two phases:

| Phase | Question it answers | Output | Skills |
|-------|--------------------|--------|--------|
| **Plan / Decide** | *What should we build, and what do we need to decide first?* | Decisions, specs, tickets — **not code** | `/grill-with-docs`, `/grill-me`, `/domain-modeling`, `/research`, `/prototype`, `/wayfinder`, `/to-spec`, `/to-tickets`, `/triage` |
| **Build / Execute** | *Build this well-specified thing.* | Code, committed, reviewed | `/implement`, `/tdd`, `/code-review`, `/diagnosing-bugs` |

The whole point of the planning phase is to reach a state where **nothing is left to decide** — then a build skill turns a spec/ticket into code. Confusion almost always comes from reaching for a build skill while decisions are still open, or a planning skill on something already fully specified.

**The test:** *Is there still something to decide?* Yes → a planning skill. No, it's spec'd → `/implement`.

---

## 2. Cheat sheet — "I want to… → do this"

| I want to… | Run | Notes |
|------------|-----|-------|
| Work the current build (Radio-Scout **v1**, right now) | **`/wayfinder 25`** | See §4. One ticket per session. |
| Build one specific, already-spec'd ticket | `/implement <#>` | Only if working *outside* the map (see §4). |
| Sharpen a new feature idea into a plan | `/grill-with-docs` | Interview + writes `CONTEXT.md`/ADRs as it goes. |
| Sharpen an idea with **no repo** attached | `/grill-me` | Stateless; saves nothing. |
| Plan something too big for one session | `/wayfinder` | Charts a decision map; see §5. |
| Turn a finished discussion into a spec | `/to-spec` | No interview — just synthesis → GitHub issue. |
| Split a spec into buildable tickets | `/to-tickets` | Tracer-bullet slices with blocking edges. |
| Build a behaviour test-first | `/tdd` | The red→green loop; `/implement` uses it internally. |
| Review a branch/PR before merge | `/code-review` | Two axes: Standards + Spec. |
| Chase down a hard/intermittent bug | `/diagnosing-bugs` | Builds a tight repro loop first, then fixes. |
| Answer a design Q with throwaway code | `/prototype` | Logic (state machine) or UI variants. |
| Look up docs/API facts from primary sources | `/research` | Background agent → cited `.md` in repo. |
| Process incoming issues/bug reports | `/triage` | Only for issues **you didn't create**. |
| Pin down / fix domain vocabulary | `/domain-modeling` | Keeps `CONTEXT.md` a clean glossary. |
| Design a module's shape (deep modules) | `/codebase-design` | Vocabulary: module, interface, depth, seam. |
| Survey the codebase for refactor targets | `/improve-codebase-architecture` | Produces an HTML report of candidates. |
| Carry context into a fresh session | `/handoff` | Writes a handoff doc; then open a new session. |
| Resolve a merge/rebase conflict | `/resolving-merge-conflicts` | |

---

## 3. The main pipeline: idea → ship

This is the canonical route a **new** piece of work travels. Keep steps 1–3 in **one unbroken context window** — don't `/compact` until after `/to-tickets` — so the grilling, spec, and tickets all build on the same thinking.

```mermaid
flowchart TD
    A[Loose idea] --> B{Too big for<br/>one session?}
    B -->|Yes, foggy| W[/wayfinder<br/>chart a decision map/]
    W --> C
    B -->|No| C[/grill-with-docs<br/>sharpen by interview/]
    C --> D{Question needs a<br/>runnable answer?}
    D -->|Yes| P[/prototype or /research<br/>then fold the answer back/]
    P --> C
    D -->|No| E{Multi-session<br/>build?}
    E -->|Yes| F[/to-spec → /to-tickets/]
    F --> G[/implement per ticket<br/>clear context between each/]
    E -->|No| G
    G --> H[/implement drives /tdd internally<br/>then /code-review, then commit]
```

**Key facts about this pipeline:**

- **`/grill-with-docs`** is the front door for anything with a codebase. It runs the `/grilling` interview *and* the `/domain-modeling` discipline, leaving a paper trail in `CONTEXT.md` and `docs/adr/`.
- **`/to-spec`** synthesises the conversation into a spec and publishes it as a GitHub issue (labelled `ready-for-agent`). No re-interview.
- **`/to-tickets`** breaks a spec into **tracer-bullet** tickets — each a *vertical* slice through every layer (schema → API → UI → tests), sized to one fresh context window, with explicit **blocking edges**.
- **`/implement`** builds one ticket by driving **`/tdd`** (red→green, one slice at a time), then closes out with **`/code-review`** before committing. **Clear context between tickets.**
- Reach for **`/tdd`** or **`/code-review`** on their own anytime you want just that piece.

### Context hygiene

Steps 1–3 stay in one window; each `/implement` starts **fresh** from its ticket. The limit is the *smart zone* (~120k tokens where the model still reasons sharply). If a planning session approaches it before `/to-tickets`, don't push on degraded — `/handoff` and continue in a new thread. (`/handoff` **forks** to a new session; the built-in `/compact` **continues** the same one — use `/compact` only at clean breaks between phases.)

---

## 4. Radio-Scout **right now**: working map #25

This is the part that resolves most day-to-day confusion.

Radio-Scout v1 is already **past the planning phase**. Every v1 design decision is recorded in **ADRs 0001–0009**, `CONTEXT.md`, and the spec **[#24 "Spec: Radio-Scout v1"]**. The build is charted as the **#1–#23 DAG** of tracer-bullet tickets under epic #24, and tracked by **[#25 "Wayfinder map: Implement Radio-Scout v1"]** (labelled `wayfinder:map`).

Map #25 is a deliberate **execution map** — an explicit override of wayfinder's usual "plan, don't do" default. Its own Notes say so:

> *This map carries execution: its tickets are the build tickets #1–#23; work them in frontier order, test-first, one meaningful slice per session.*

So the entire daily loop is one command:

```
/wayfinder 25
```

That single invocation runs a **manager → builder handoff**:

1. **Claims** the next frontier ticket (assigns it to you, so parallel sessions skip it) — *wayfinder, the manager*.
2. **Hands the build to `/implement`**, which owns the full code discipline: drives `/tdd` red-green (native Rust `cargo test` + Vitest) → runs `/code-review` (Standards + Spec) on the diff → commits — *implement, the builder*.
3. **Closes** the issue and appends a line to the map's *Decisions-so-far* — *wayfinder again*.

#### How the map delegates to `/implement`

wayfinder doesn't itself know how to build — it resolves each ticket by *"invoking the skills the map's Notes name."* That "skills to consult" line in map #25's Notes is effectively a **config file** for the build step:

- Map #25's Notes name **`/implement`** as the build executor. That's what guarantees the full **TDD → code-review → commit** ritual runs every session — because the discipline lives inside `/implement`'s own instructions, not in a loose "build with TDD" note.
- Earlier the Notes named only `/tdd`, so `/code-review` was silently skipped (some tickets got reviewed, some didn't). Naming `/implement` fixed it in one place.

Rule of thumb: **wayfinder manages the map; `/implement` builds the code.** A skill is a *playbook the agent follows*, not compiled code — so what wayfinder does on each ticket is exactly what its Notes tell it to delegate to.

**To target a specific ticket** instead of the next frontier one:

```
/wayfinder 25 work #4
```

### The rules while working the map

- **One ticket per session.** wayfinder's hard rule: *never resolve more than one ticket per session* (except `research` tickets). So two tickets = two sessions.
- **Frontier only.** A ticket is takeable when it's open, **unblocked** (all blocking tickets closed), and **unassigned**. GitHub's native issue dependencies render this in the UI.
- **Parallel is fine.** Two unblocked tickets can run in two separate sessions at once — each claims its ticket first, so they don't collide.

### Current frontier (as of this writing)

The open, unblocked, unassigned tickets ready to take:

| # | Title | Area |
|---|-------|------|
| **#3** | Domain model + SeaORM entities + migrations (SQLite + Postgres) | storage |
| **#4** | Blob storage: object_store (filesystem / S3-Garage) + range serving | storage |
| **#19** | Admin auth: cookie session + CSRF + brute-force lockout | config |

Everything #5–#23 is blocked behind these (and each other) until their blockers close. Run `gh issue list --state open` or open the map to see the live frontier.

### `/wayfinder 25` vs `/implement #3` — which?

Both build the ticket. The difference is bookkeeping:

- **`/wayfinder 25`** *(recommended for map work)* — keeps the map authoritative: claims the ticket, updates *Decisions-so-far*, coordinates parallel sessions. Use this to work v1.
- **`/implement #3`** — builds + reviews + commits, but **skips** the map bookkeeping (no claim, no map update). Use it only when you're deliberately building something *not* being tracked on the map.

For v1, drive with `/wayfinder 25` so the map never goes stale.

---

## 5. On-ramps — starting situations that generate work

Not everything starts as a clean idea. These three skills are entry points that feed into the pipeline above.

- **A huge, foggy effort** (greenfield, or a feature too big to hold in one session) → **`/wayfinder`**. It charts a **shared map of decision tickets** on GitHub and resolves them **one at a time**, producing *decisions, not deliverables*, until the way is clear. This is how map #25 itself was created. When a *planning* map clears, it hands off to `/to-spec` → `/to-tickets` → `/implement` — it doesn't build. (Map #25 is the exception: it was deliberately made an *execution* map.) Use wayfinder **only** for genuinely session-spanning fog — never a well-scoped feature; `/grill-with-docs` is for ideas you *can* hold in one session.

- **Incoming issues/bug reports piling up** → **`/triage`**. Moves issues through a state machine (`needs-triage` → `needs-info` / `ready-for-agent` / `ready-for-human` / `wontfix`) and writes agent-ready briefs that `/implement` later picks up. **Only for issues you didn't create** — tickets `/to-tickets` produced are already agent-ready; don't triage them.

- **Something's broken** → **`/diagnosing-bugs`**. For the hard ones — the resistant bug, the intermittent flake, the regression between two known-good states. It refuses to theorise until it has a **tight feedback loop** (one command that already goes *red* on this exact bug), then fixes with a regression test. Its post-mortem hands off to `/improve-codebase-architecture` when the real finding is "there was no good seam to lock this bug down."

---

## 6. The vocabulary layers (used *by* the skills above)

Two model-invoked references run *beneath* the others — reach for them directly when the **words**, not the process, are the problem:

- **`/domain-modeling`** — sharpen the project's *domain* language: challenge a fuzzy term, resolve an overloaded word, record a hard-to-reverse decision as an ADR. Keeps `CONTEXT.md` a clean glossary (glossary only — no implementation details). `/grill-with-docs` drives this automatically.
- **`/codebase-design`** — the **deep-module** vocabulary (*module, interface, depth, seam, adapter, leverage, locality*) for designing a module's *shape*: lots of behaviour behind a small interface at a clean seam. `/tdd` and `/improve-codebase-architecture` both speak it.

---

## 7. Full skill reference

| Skill | Phase | One-liner | Invocation |
|-------|-------|-----------|------------|
| `/wayfinder` | Plan | Chart a decision map for session-spanning work | user only |
| `/grill-with-docs` | Plan | Relentless interview + writes ADRs/glossary | user only |
| `/grill-me` | Plan | Relentless interview, no repo, saves nothing | user only |
| `/grilling` | Plan | The raw interview primitive | model |
| `/domain-modeling` | Plan | Build/sharpen domain glossary + ADRs | model |
| `/research` | Plan | Background agent → cited primary-source `.md` | model |
| `/prototype` | Plan | Throwaway code to answer one design question | model |
| `/to-spec` | Plan | Synthesise conversation → spec issue | user only |
| `/to-tickets` | Plan | Split spec → tracer-bullet tickets | user only |
| `/triage` | Plan | Move incoming issues through triage states | user only |
| `/implement` | Build | Build a ticket via `/tdd`, then `/code-review`, commit | user only |
| `/tdd` | Build | Red→green loop; tests at agreed seams | model |
| `/code-review` | Build | Two-axis review (Standards + Spec) of a diff | model |
| `/diagnosing-bugs` | Build | Repro-loop-first bug diagnosis | model |
| `/codebase-design` | Design | Deep-module vocabulary | model |
| `/improve-codebase-architecture` | Design | Survey for deepening opportunities (HTML report) | user only |
| `/handoff` | Meta | Compact a session into a handoff doc | user only |
| `/resolving-merge-conflicts` | Meta | Resolve an in-progress merge/rebase | model |
| `/ask-matt` | Meta | Live router — "which skill fits?" | user only |
| `/setup-matt-pocock-skills` | Meta | One-time repo config (already done) | user only |
| `/teach` | Meta | Learn a concept over multiple sessions | user only |
| `/writing-great-skills` | Meta | Reference for authoring skills | user only |

*"user only" = you must type it; it won't fire on its own and other skills can't reach it. "model" = other skills can invoke it (and you can type it too).*

---

## 8. The rules that bind every session

These are Radio-Scout hard constraints (from `CLAUDE.md` and the ADRs) that apply no matter which skill is running:

1. **TDD is mandatory.** Every change goes red→green at a pre-agreed **seam**. Prefer **native tests** as the loop — Rust `cargo test` + the in-process HTTP/WS harness, and Vitest + React Testing Library. Reserve **Playwright** for browser-level flows native tests can't reach (Media Session / lock-screen, PWA install, iOS/WebKit background audio).
2. **Use the domain language.** `CONTEXT.md` is the ubiquitous glossary (Call, System, Talkgroup, Ref vs Id, …). Use those terms exactly, in code and prose. Respect the ADRs in `docs/adr/` for any area you touch — don't re-litigate settled decisions.
3. **GitHub issues are the tracker.** Everything is a `gh` issue (see `docs/agents/issue-tracker.md`). Specs, tickets, the wayfinder map, and its dependency edges all live there. Triage labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`.
4. **Vertical slices, not horizontal.** Both `/to-tickets` and `/tdd` insist on tracer-bullet slices — one narrow-but-complete path through every layer — never "all the tests, then all the code."
5. **Performance & simple install are first-class.** The app must run well on a Raspberry Pi 5, and install in one command. Every design decision is weighed against this.

---

## 9. Config & setup

The repo is already configured (`/setup-matt-pocock-skills` has been run). The generated config lives in:

- `docs/agents/issue-tracker.md` — GitHub via `gh`, plus the wayfinding operations (map, child tickets, blocking edges, frontier query).
- `docs/agents/triage-labels.md` — the canonical label vocabulary.
- `docs/agents/domain.md` — where `CONTEXT.md` and ADRs live and how skills read them.

Re-run `/setup-matt-pocock-skills` only to switch trackers or start over. Edit those `docs/agents/*.md` files directly for smaller tweaks.

---

### TL;DR

- **Building v1 today?** → `/wayfinder 25` (or `... work #<n>`). One ticket per session. That's the loop.
- **New idea?** → `/grill-with-docs` → (`/to-spec` → `/to-tickets` if multi-session) → `/implement`.
- **Too big to hold in one session?** → `/wayfinder`.
- **Incoming issue?** → `/triage`. **Broken?** → `/diagnosing-bugs`.
- **Lost?** → `/ask-matt`.
