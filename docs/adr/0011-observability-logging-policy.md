# Observability: structured logging, and what we deliberately don't record

## Context

Radio-Scout had no logging. Ten `println!`/`eprintln!` calls, no levels, no timestamps, no request visibility, no dependency.

The cost showed up the first time a real recorder was pointed at it (2026-07-25). A database created before #8 was missing `systems.auto_populate`, so **every ingest returned HTTP 500** — and the raw SQL error travelled to the recorder while the server said nothing at all. Diagnosing it took an hour of `netstat`, `lsof` and guesswork to answer a question the server should have answered in one line: *is anything reaching us?* Trunk Recorder's own log was the only evidence either side produced, and its build printed an empty error body.

Three failures in one incident, each structural:

1. **No request visibility.** "Nothing is arriving" and "the server isn't running" looked identical from outside.
2. **Errors went to the client and nowhere else.** The one machine that could have recorded the cause discarded it.
3. **No levels.** Nothing to turn up when chasing a problem, nothing to turn down on a Pi.

rdio-scanner is the reference. `server/log.go` writes every event to stdout *and* a `logs` table (level/message/timestamp), prunes it by age, and serves a searchable admin Logs page — which is genuinely useful to an operator with no shell access, and which we should have. It is also worth improving on: the insert is `fmt.Sprintf`-interpolated SQL (a log message containing an apostrophe corrupts it), and it records **every listener's IP address and access-code ident at info level**, on instances that are frequently public.

## Decision

**`tracing` + `tracing-subscriber`, to stdout, with a strict policy about what may and may not be recorded.**

`tracing` over `log` because spans survive across await points — an ingest's lines carry their System and Talkgroup without threading context by hand — and because a second sink (the operator log surface below) becomes a `Layer` rather than a bespoke logger.

**stdout only.** Every way we ship captures it: systemd/journald and Docker both do (#23), and a foreground run puts it on the terminal. We inherit rotation, retention and `journalctl -u radio-scout -f` instead of shipping a file rotator onto a disk-constrained Pi that retention (#10) is already rationing. Colour is on only when stdout is a terminal — everything that stores our output stores it verbatim, and escape sequences in a pasted excerpt help nobody.

**`RUST_LOG` selects level and target**, defaulting to `info,sqlx::query=warn,sea_orm_migration=warn` when it is unset, blank or unparseable (a typo must not boot a silent scanner). The two demotions are deliberate, not taste: sqlx logs **every statement** at INFO, which on a scanner taking a Call a second is a log line per query — protocol detail, so DEBUG per rule 7 — and sea-orm's migration lines are formatted sentences we replace with structured ones. `RUST_LOG=debug` gives both back.

**Migrations say what they did.** `db::migrate` applies pending migrations one at a time and logs each name at INFO *after* it succeeds, so a fresh database lists what it built and an upgraded one names only what changed; an already-current schema says so at DEBUG. This is the gap that made the 2026-07-25 incident so slow to diagnose — the migration that would have fixed it had never run, and nothing said so either way.

### The rules

These are **hard rules**, not preferences. The first is enforced by the compiler; the rest are enforced in review and by the tests named beside them.

1. **`println!`, `eprintln!` and `dbg!` are denied** in library and binary code (`[lints.clippy]`). Application output goes through `tracing`, always. The single exception is `examples/` — a CLI tool whose stdout *is* its product — which carries an explicit `#![allow]` and a comment saying why. (Cargo applies package lints to every target, so the one test that announces being skipped carries a statement-level allow too; build scripts are never linted by clippy and need none.)
2. **A secret is never logged, at any level, in any form.** Not API keys, access codes, admin passwords or their hashes; not truncated, not prefixed. A key is identified in logs by its database id or its label, resolved *after* lookup.

   This bites hardest where the secret is *ours*: first run used to generate an ingest key and `println!` it. A credential still has to reach the operator, so **it goes to a file and the log line carries the path** — `startup::provision_ingest_key` writes the generated key into the `.env` it would otherwise have been read from (created `0600`, existing settings and line endings preserved, an existing assignment replaced rather than duplicated since dotenvy takes the first). Better than the banner it replaces, which was unrecoverable once the scrollback scrolled. With no env file to have read it from, it is written beside the database in `base_dir` instead of into the working directory, which under systemd or Docker is routinely `/` or read-only. If the write fails anyway, **no key is registered** — a credential only the server ever saw would lock the operator out, and an empty database is what makes the next boot try again.
3. **Every rejected ingest logs at WARN with a machine-readable `reason`** — `invalid-api-key`, `duplicate`, `blacklisted`, `no-talkgroup`, `not-populated`. A Call that does not become a row must leave a line saying why. This is the rule the 2026-07-25 incident bought.
4. **Every 5xx logs at ERROR with the cause and a correlation ref**, and the response body carries **only** that ref — never the cause. Internals do not travel to clients or into pasted logs. The rdio-compatible strings for *known* outcomes (`Call imported successfully.`, `duplicate call rejected`, `Incomplete call data: no talkgroup`) are a wire contract and stay byte-identical.
5. **Listener IP addresses never appear above DEBUG.** Recorder IPs may appear at INFO on ingest routes — that is the operator's own infrastructure and the diagnostic that matters. A public instance must not accumulate a record of who listened and when, least of all in a database once the log surface lands.
6. **Messages are static strings; the variable part is structured fields.** `warn!(reason = "blacklisted", %system_ref, "ingest dropped")`, never a formatted sentence. This is what makes a log searchable, a DB sink sane, and rdio's quoting bug impossible.
7. **Levels mean something specific:** ERROR = an operator must act. WARN = something was rejected or dropped. INFO = a notable normal event (startup, ingest outcome, one line per request). DEBUG = per-asset and per-range requests, listener IPs, protocol detail. TRACE = wire-level dumps.
8. **Nothing logs unguarded inside a hot loop.** Per-Call is fine; per-range-request is DEBUG; per-sample is never.

### The request log

One line per HTTP request at INFO — method, path, status, duration — with the chatty classes demoted to DEBUG: embedded SPA assets, `/api/call/{id}/audio` range requests (a media element issues several per Call), and the `/healthz` liveness probe, which a packaged deployment (#23) hits every few seconds and which at INFO is thousands of lines a day saying nothing happened. A malformed, 404ing or rejected request still appears, which is exactly what was missing.

The level is the **louder of the route's class and its outcome**: a 4xx escalates to WARN and a 5xx to ERROR whatever the class, so a probe that fails or a range request that 416s is never quiet. `/healthz`'s demotion is the one place this deviates from #28 as written (which put everything outside the two named classes at INFO); it is the same reasoning the ticket gives for the other two, applied to the chattiest route we ship, and rule 8 asks for it.

Three details that are policy, not formatting:

- **The path, never the whole URI.** Access codes are a query parameter in rdio-scanner's world and ADR-0008 keeps that shape, so logging the query string would make rule 2 conditional on which routes happen to exist today.
- **The client address is the TCP peer's**, never `X-Forwarded-For` (see below). A recorder's address rides an ingest line at any level; a listener's rides only a line that is already DEBUG, which in practice means the audio and asset requests it makes, and only when someone turned the verbosity up to look. Note this lands *stricter* than #28 asked for: the archive and live-feed routes rest at INFO, so a listener's address on those is unrecorded at **every** verbosity rather than merely hidden at the default. Nothing needs it there, and the alternative — emitting a second, DEBUG-only line per request just to carry an address — is the hot-loop rule 8 forbids.
- **Every line carries a 16-hex-character request id**, generated per request — never taken from an inbound header — and carried as a span field, so the lines a handler emits while serving the request carry it too. The response echoes it in `x-request-id`, so an operator holding a client's failure can grep the server. The 5xx correlation ref hangs off the same id (#29, below).

The live feed is a socket, not a request: it logs its upgrade (the 101), then one line when a listener arrives and one when it leaves, carrying the connection's lifetime and the upgrade's request id. Never one per frame (rule 8).

### Rejections, failures, and the correlation ref

The shape rules 3 and 4 take in the code (#29):

**One funnel per outcome.** Every ingest path that declines to store a Call goes through `ingest::rejected`, which writes `WARN … ingest rejected reason=<slug>` and hands back the response the recorder gets; every handler that fails returns a `failure::ServerError` naming the stage. The two differ in how much they guarantee, and it is worth being honest about which: the rejection funnel is a **convention** — a new rejection path that builds its own response skips the line, and only review catches that — while the 5xx redaction is **structural**, applied by the request middleware to any server-error status whatever produced it, including one nobody wrote a cause for (which logs `server error with no recorded cause` — loudly, because a 500 nobody wrote down is the 2026-07-25 failure mode itself).

**The vocabulary is one string, not two.** The rejection slugs are the five rule 3 names plus the malformed-body family — `malformed-multipart-body`, `could-not-read-field`, `could-not-read-audio`, `no-audio`, `no-meta`, `invalid-meta`. For the rdio 417 family the slug *is* the wire detail with its dashes spelled as spaces (`no-talkgroup` ⇄ `Incomplete call data: no talkgroup`), so the string an operator greps and the string a recorder branches on cannot drift apart. Stages are slugs in the same shape (`dedup`, `store-call`, `search-calls`), rendered bare rather than quoted for the same reason the request line renders its path bare.

**The client gets `internal error (ref: <request id>)` and nothing else**, at every 5xx, from every route — the ref being #28's request id, which the response also carries as `x-request-id`. The cause travels to the log as `cause=`, never to the body. This costs the operator who only reads their recorder's log the detail they used to get; the ref buys it back, and it is the difference between "the server knows" and "the recorder knows and the server doesn't".

**One upload is one span** (`ingest{system_ref, talkgroup_ref, call_id}`), nested inside the request span, with the Call id recorded onto it after the row exists — so a rejection line carries the System and Talkgroup without repeating them, and a stored Call's line carries the id it became. Like the request span it exists at ERROR level: the lines that need its context most are WARN rejections, which an operator may be watching with everything else turned down. The span starts where an upload first *has* an identity, so the malformed-body rejections that precede it carry the request id alone — which is all that is known about them. A `ServerError` captures the span it was built in and the middleware re-enters it to write the line, so a 500 that happens mid-pipeline is reported with the upload's context rather than with the request id alone.

The live feed's own subsystem lines: the subscribe at DEBUG (protocol detail — a listener re-subscribes on every Talkgroup toggle, and the line carries the *shape* of the selection, never its contents), a lagging listener and a reaped half-open connection at WARN (a listener that stopped hearing things is a symptom, and rdio leaves both silent), and one DEBUG line per reconnect catch-up carrying `sent` and `truncated` — never one per backfilled Call.

### The operator log surface (later)

A `Layer` writing to a `logs` table, retention in days, and a searchable Settings → Logs view behind admin auth (#19). rdio parity, with the console remaining first-class rather than an afterthought, parameterised inserts rather than interpolated SQL, and rule 5 applying to what gets stored.

## Considered and rejected

- **`log` + `env_logger`** — two small crates and no learning curve, but no spans (every message re-states its own context), no structured fields, no `tower-http` integration, and the DB sink becomes a hand-rolled `log::Log`.
- **A rotating file sink** (`tracing-appender`) — duplicates what journald and Docker already do for every deployment we ship, and adds a second disk-space policy to a device where retention is already rationing the disk.
- **JSON output now** — trivial to add later (tracing-subscriber ships the formatter); nobody is shipping logs to an aggregator yet.
- **rdio's DB-first logging** — putting a database write in the path of every log line on a Pi, and losing the console as the primary surface. Inverted here: console first, DB as an additional sink.
- **Trusting `X-Forwarded-For` for the client address** — rdio-scanner does, unconditionally (`main.go:265`), and it is what makes the address useful behind nginx or Docker's bridge, where the peer is the proxy. But the header is attacker-controlled: on a public instance anyone could forge a recorder's address into the operator's log, which corrupts the one field the request log exists to make trustworthy. Honouring it needs an explicit list of proxies that may be believed, which belongs with the real configuration (#17), not with an implicit "looks like a private address" heuristic. Until then a reverse-proxied deployment reads every client as its proxy — a known, visible limitation rather than a quiet lie.
- **Reusing an inbound `X-Request-Id`** — nice for tracing through a proxy, and the same problem: a value a client chooses, landing in every line about the request.
- **Keeping error detail in the response body** — convenient for an operator who only reads their recorder's log, but it means internals travel to clients by design, and the correlation ref recovers the convenience without the leak.
- **Keeping the first-run key banner** behind a narrow allow (or logging the generated key once at INFO) — it reads as harmless, but it makes rule 2 conditional on the day, and startup output is exactly what gets pasted into an issue and, once the log surface lands, written to a database. A file the operator can `cat` is both safer and more recoverable.
- **Writing the generated key to its own file** under the base dir rather than into `.env` — no rewriting of a file we don't own, but it invents a second place a credential can live when `.env` is already *the* documented one, and the next boot would not pin it. (`base_dir/.env` is only the fallback for a boot that found no env file at all.)

## Consequences

- Three crates and some compile time on a Pi; `tracing`'s cost when a level is disabled is a static check.
- Every future subsystem owes instrumentation as it is written, not afterwards — the `println!` escape hatch is gone by lint.
- The operator log surface inherits rule 5, so what the console declines to record cannot appear in the database either.
- `RUST_LOG` is the control surface until #17 lands a `[log]` section that sets the same thing from configuration.
- Log lines are assertable, so they are asserted: tests capture the subscriber's real output and read it back (`src/testing.rs`). That harness has to install a permissive global subscriber first — `tracing` caches per callsite whether anyone is interested, and a thread-local capture otherwise loses events to a callsite another test reached first.
- A first run whose env file cannot be written comes up with **no** ingest key and an ERROR saying so, where it used to come up with one nobody could read. With `base_dir` as the fallback target that needs a writable `base_dir` to fail, which is already fatal for a scanner that has to store audio — but a deployment that manages to hit it must set `RADIO_SCOUT_API_KEY`, which #17's config makes the normal path anyway.
