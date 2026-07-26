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

**stdout only.** Every way we ship captures it: systemd/journald and Docker both do (#23), and a foreground run puts it on the terminal. We inherit rotation, retention and `journalctl -u radio-scout -f` instead of shipping a file rotator onto a disk-constrained Pi that retention (#10) is already rationing.

### The rules

These are **hard rules**, not preferences. The first is enforced by the compiler; the rest are enforced in review and by the tests named beside them.

1. **`println!`, `eprintln!` and `dbg!` are denied** in library and binary code (`[lints.clippy]`). Application output goes through `tracing`, always. The single exception is `examples/` — a CLI tool whose stdout *is* its product — which carries an explicit `#![allow]` and a comment saying why.
2. **A secret is never logged, at any level, in any form.** Not API keys, access codes, admin passwords or their hashes; not truncated, not prefixed. A key is identified in logs by its database id or its label, resolved *after* lookup.
3. **Every rejected ingest logs at WARN with a machine-readable `reason`** — `invalid-api-key`, `duplicate`, `blacklisted`, `no-talkgroup`, `not-populated`. A Call that does not become a row must leave a line saying why. This is the rule the 2026-07-25 incident bought.
4. **Every 5xx logs at ERROR with the cause and a correlation ref**, and the response body carries **only** that ref — never the cause. Internals do not travel to clients or into pasted logs. The rdio-compatible strings for *known* outcomes (`Call imported successfully.`, `duplicate call rejected`, `Incomplete call data: no talkgroup`) are a wire contract and stay byte-identical.
5. **Listener IP addresses never appear above DEBUG.** Recorder IPs may appear at INFO on ingest routes — that is the operator's own infrastructure and the diagnostic that matters. A public instance must not accumulate a record of who listened and when, least of all in a database once the log surface lands.
6. **Messages are static strings; the variable part is structured fields.** `warn!(reason = "blacklisted", %system_ref, "ingest dropped")`, never a formatted sentence. This is what makes a log searchable, a DB sink sane, and rdio's quoting bug impossible.
7. **Levels mean something specific:** ERROR = an operator must act. WARN = something was rejected or dropped. INFO = a notable normal event (startup, ingest outcome, one line per request). DEBUG = per-asset and per-range requests, listener IPs, protocol detail. TRACE = wire-level dumps.
8. **Nothing logs unguarded inside a hot loop.** Per-Call is fine; per-range-request is DEBUG; per-sample is never.

### The request log

One line per HTTP request at INFO — method, path, status, duration — with the chatty classes demoted to DEBUG: embedded SPA assets, and `/api/call/{id}/audio` range requests, since a media element issues several per Call. A malformed, 404ing or rejected request still appears, which is exactly what was missing.

### The operator log surface (later)

A `Layer` writing to a `logs` table, retention in days, and a searchable Settings → Logs view behind admin auth (#19). rdio parity, with the console remaining first-class rather than an afterthought, parameterised inserts rather than interpolated SQL, and rule 5 applying to what gets stored.

## Considered and rejected

- **`log` + `env_logger`** — two small crates and no learning curve, but no spans (every message re-states its own context), no structured fields, no `tower-http` integration, and the DB sink becomes a hand-rolled `log::Log`.
- **A rotating file sink** (`tracing-appender`) — duplicates what journald and Docker already do for every deployment we ship, and adds a second disk-space policy to a device where retention is already rationing the disk.
- **JSON output now** — trivial to add later (tracing-subscriber ships the formatter); nobody is shipping logs to an aggregator yet.
- **rdio's DB-first logging** — putting a database write in the path of every log line on a Pi, and losing the console as the primary surface. Inverted here: console first, DB as an additional sink.
- **Keeping error detail in the response body** — convenient for an operator who only reads their recorder's log, but it means internals travel to clients by design, and the correlation ref recovers the convenience without the leak.

## Consequences

- Three crates and some compile time on a Pi; `tracing`'s cost when a level is disabled is a static check.
- Every future subsystem owes instrumentation as it is written, not afterwards — the `println!` escape hatch is gone by lint.
- The operator log surface inherits rule 5, so what the console declines to record cannot appear in the database either.
- `RUST_LOG` is the control surface until #17 lands a `[log]` section that sets the same thing from configuration.
