# Configuration: TOML + flags + environment, strictly validated, loudest layer wins

> **Amended by #87 (2026-07-31).** Each section *is* its subsystem's configuration type — the mirrored structs and their translation functions are gone — and the environment layer *is* a settings table the tests walk, rather than thirty hand-written blocks described by a hand-written case list. Nothing an operator writes changed. The original decision below is the record of what was decided in July 2026; read [the amendment](#amendment-87-2026-07-31-the-section-type-is-the-subsystems-type-and-the-table-is-the-resolution) for the shape that is actually built.

## Context

Radio-Scout has to be configurable without a UI (spec US 36) *and* run with no configuration at all (US 35). Until now every knob was an environment variable read inline in `main.rs` (`RADIO_SCOUT_BASE_DIR`, `_PORT`, `RetentionConfig::from_env_vars`), parsed with `.ok()` and silently defaulted; the storage backend and Postgres URL had types but nothing that could set them; `RUST_LOG` was the only way to change the log level and it did not survive a reboot; and #28 deferred the trusted-proxy list here because believing `X-Forwarded-For` is a configuration decision, not a heuristic.

rdio-scanner's model (`server/config.go`) is the thing to beat, and it has four specific faults:

1. **The file silently overrides the flags.** `flag.Parse()` runs first, then the INI is loaded over the top (`config.go:96-137`), so `-listen` cannot override a configured `listen`.
2. **An unknown key or an unparseable value is ignored.** A typo'd key does nothing; a bad `db_port` falls back to a default; a config file that fails to load entirely is skipped without a word.
3. **Most operational settings aren't in the file at all** — retention, dedup, auto-populate, blacklists and downstreams live in the database behind the admin UI, so a headless install cannot be configured and nothing is version-controllable.
4. **No environment support**, which is how containers and systemd units are configured.

## Decision

One `Config` (`src/config.rs`), resolved once at boot, from four layers.

**Precedence, loudest first: CLI flag → environment variable → TOML file → default.** The more specific to this invocation something is, the louder it speaks. This is the inverse of rdio's ordering and the reason `--port` means what it says.

**The file is `radio-scout.toml`**, sectioned (`[server] [database] [storage] [storage.s3] [retention] [ingest] [log]`), found via `--config`, then `RADIO_SCOUT_CONFIG`, then the working directory. **No file is not an error** — that is US 35.

**Strict validation: a configuration that cannot be run stops the boot**, exiting `2` with a message naming the source, the value and what was expected. That covers unknown keys (`deny_unknown_fields`), unparseable values from *any* layer, cross-field requirements (`storage.backend = "s3"` with no credentials), impossible values (`retention.batch_size = 0`, a non-positive `max_size_gb`, a negative dedup window) and log directives `tracing` cannot parse. A typo'd `retention.dayz` that silently keeps the default is how an operator loses a month of Calls.

**Everything operational is in the file**, including the settings rdio keeps in its database: retention, the dedup window, auto-populate, and the log filter.

**Secrets never reach a log line.** The startup summary names the database *dialect* and the storage *backend*, never the URL or the key; `config::S3`'s `Debug` redacts the secret so a `Debug`-printed config cannot leak one either; and a **TOML parse error** reports its position and serde's message but never the source line, because `toml::de::Error`'s own `Display` quotes the offending line verbatim — and the line an operator mistypes is the line they were editing, which in `[storage.s3]` is the credential. Where serde's message would quote the value itself (a type mismatch), a failing line that assigns a secret-bearing key reports its position alone (ADR-0011 rule 2).

`[storage.s3]`'s credentials have **no flags** — a command line is world-readable in `ps` — and come from the file or the environment. `--database-url` does exist, because the issue asks for a DB flag and a connection URL is not always a credential; an operator who puts a password in one has chosen to, and it is still never logged. The ingest key keeps its own home in `.env` (ADR-0008), because first run *writes* it.

**`[server] trusted_proxies`** is a list of addresses and CIDR blocks. With it empty — what ships — `X-Forwarded-For` is never read and the request log names the TCP peer. When the peer matches, the client address is the **rightmost entry of the header that is not itself a trusted proxy**. Every hop appends the address it saw, so the right-hand end is what our own infrastructure wrote and the left-hand end is whatever the client chose to send: taking the leftmost — or taking the header from anyone, as rdio does at `main.go:265` — lets a stranger forge a recorder's address into the operator's log.

The walk therefore goes right to left and stops at the first entry that does not parse, falling back to the peer, so junk cannot shift which entry we land on; junk to the *left* of the answer is simply never reached, because a client prepending nonsense must not be able to invalidate what a trusted proxy appended. If every hop is trusted, the leftmost is taken — it is as close to the client as the header can get us.

**`--write-config` emits a commented file with every setting at its default**, and refuses to overwrite an existing one. Two tests hold it to being the truth: it must parse back to exactly `Config::default()`, and it must show every key the defaults serialize to, at that default — so a setting added to the code without a line in the template fails the build.

**Boot says where its configuration came from** — the file it read, or that there wasn't one — then the settings that resulted.

## Considered and rejected

- **Dropping the `RADIO_SCOUT_*` environment layer** now that a file exists. Containers (#23) and systemd units configure by environment, and dropping it would have broken the existing `.env` flow for no gain.
- **Warn-and-continue on a bad setting**, so a scanner always starts. It trades a loud failure for a silent wrong one; a misconfigured retention window is invisible in a Pi's scrollback and expensive to discover.
- **Leniency for `RUST_LOG` only.** `observability::subscriber` still falls back to the default filter rather than failing (a subscriber that fails to build would mean silence), but configuration validates the directives first from every layer. An operator who asked for `radio_scout=trace` and silently got INFO debugs the wrong log at 2am.
- **An `[enhancement]` section**, which spec US 36 lists. The ADR-0006 pipeline does not exist yet; a knob that accepts `enabled = true` and changes nothing is worse than an "unknown key" error. It lands with the feature.
- **Reloading configuration on SIGHUP.** Nothing here is expensive to restart, and a half-applied reload (a moved `base_dir`, a changed database) is a much worse failure than a restart.
- **Trusting private address ranges automatically.** "Looks like a LAN" is not the same as "is my proxy", and the difference matters exactly when the instance is public.

## Consequences

- `main.rs` is thin: parse flags, resolve config, install logging, serve. Every decision worth testing is in `config.rs`, which matters because `main.rs` is excluded from coverage.
- A boot can now fail on configuration. `2` means the configuration step failed — an unusable setting, or a config file that could not be read or (for `--write-config`) written; `1` means the start itself failed (a bound port); `0` means it ran. An init system can tell them apart.
- **A louder layer can set a value but not clear one.** Blank means unset — that is what `RADIO_SCOUT_PORT=` in an env file means — so `RADIO_SCOUT_TRUSTED_PROXIES=` cannot empty a configured trust list, and nothing on the command line can remove a `max_size_gb` or a `database.url` the file sets. Removing a setting means editing the file that set it. The alternative, treating blank as "clear", would make a stray `FOO=` in an env file silently override the file, which is the more common accident.
- Settings have two spellings (a TOML key and a `RADIO_SCOUT_*` variable) that must be kept in step. The env names are an explicit table in `resolve`, tested one case per variable, rather than derived by string mangling.
- `#23` (packaging) inherits a real config surface: a service unit points `--config` at `/etc/radio-scout.toml`, and Docker's bridge goes in `trusted_proxies`.

## Amendment (#87, 2026-07-31): the section type *is* the subsystem's type, and the table is the resolution

Two things above stayed true in letter and went wrong in practice, and #84's architecture review named both.

**"One `Config` (`src/config.rs`)" became ten sections mirrored by hand.** Each subsystem already had a configuration type of its own — `AdminConfig`, `PushConfig`, `RetentionConfig` — so the file's `[admin]` was a second struct with the same fields, a second `Default` reading the first, and a `Config::admin()` translating between them. Adding one setting meant nine to twelve edits across four files, of which only two were held together by a test. Seven of those mirrors are now gone: **the subsystem's type is the section**, deriving `Serialize`/`Deserialize` and carrying its own serde attributes. `[storage]` and `[storage.s3]` moved to `blob.rs` with them; `[log]` became `observability::LogConfig`, the one section spanning two subsystems, with the sink's level its own type (`logsink::StoredLevel`) so the console's filter and the stored log still cannot govern each other.

`[server]` and `[database]` stay in `config.rs`. They have no subsystem to move to and no mirror to delete — they are configuration's own settings.

**Units live at the serde boundary, not in a translation function.** Seven `Duration` fields keep their `_secs` TOML key through one shared helper (`config::secs`), and `retention.max_size_gb` becomes `max_size_bytes` through another. Both refuse a value that cannot be run *in the deserializer* — the `ProxyNet` precedent this ADR already set — so a bad `max_size_gb` in the file now reports with a line and column rather than as a validation pass afterwards. The expectation text is one constant shared with the environment layer, so the two layers cannot describe the same setting differently.

**"Tested one case per variable" was the weak half of the consequence above.** A hand-written case list is only as complete as whoever last extended it: a setting with no case was silently untested, and a variable forgotten in `resolve` failed nothing anywhere, because the tests described resolution rather than being it. The table (`config::SETTINGS`) is now **the environment layer itself** — `resolve` walks it — and the tests walk the same table against the serialized shape of `Config`, both ways. A setting with no environment spelling fails the suite; an entry naming a key no configuration has fails the suite; and `.env.example`, which is the only place a variable spelling is documented for an operator, is asserted against it in both directions too (`tests/docs.rs`).

One gap is left open honestly, because Rust has no reflection to close it: a newly added *optional* setting — one that serializes to nothing at its default — is invisible to the walk until something sets it. Every non-optional setting is covered by construction.

**The log sink's queue capacity is gone.** It was a field of a configuration type with no key, no environment spelling, no template line and no validation — unreachable by an operator, and therefore worse in a settings type than in the constant it now is.

**One behaviour did change, and it is the interesting one.** Because those two checks now happen in the deserializer, they happen *before* the environment and the command line — so a `radio-scout.toml` holding `max_size_gb = 0` refuses to boot even when `--retention-max-size-gb 5` is given, where the old post-resolution `validate()` let the flag win. This is deliberate. "A louder layer can set a value but not clear one" is already in the consequences above, and the file has always had to be *runnable in its entirety* whatever the louder layers say: an unparseable `port = "nope"`, an unknown key and a `trusted_proxies` entry that is not an address all stop the boot no matter what overrides them. Precedence decides which **value** wins, not whether a broken file is tolerated; these two settings have joined a family of four rather than left one of their own. The alternative — keeping both as raw values validated afterwards — is an `f64` of gigabytes beside the bytes the sweeper wants and a `String` beside the level the sink wants, which is the mirrored shape this amendment exists to delete. `config::tests::a_file_the_scanner_cannot_run_is_not_rescued_by_a_louder_layer` pins all four together.

The other visible change is one message's *source*: `RADIO_SCOUT_RETENTION_MAX_SIZE_GB=0` now names the variable rather than `retention.max_size_gb`, because the check is reached through the settings table, which knows what the operator actually wrote. That is what every other environment error already does.

Otherwise nothing an operator writes changed: the same keys, the same variables, the same defaults, the same exit codes, and a `--write-config` byte-identical to 0.1.0's. Ten variables still do not follow their key mechanically (`RADIO_SCOUT_PORT`, the six `RADIO_SCOUT_S3_*`, `RUST_LOG`); those spellings shipped in 0.1.0 and are named in the table rather than derived, because a uniform rule that renamed ten live settings would be a nicer table and a worse upgrade.

## Amendment (#90, 2026-08-01): where a resolved configuration is consumed

The amendment above settled where a setting is *written down*. This one settles where it is *acted on*, which was the half still spread across two files that had drifted apart.

**One module consumes a resolved `Config`**: `radio_scout::instance`. `instance::start(config, wiring)` creates the base directory, opens the database, drains the operator log sink, provisions all three credentials, opens the store, starts every worker, builds the router, binds and serves — and hands back a handle carrying the bound address, the database, the store, a shutdown and a restart. `main.rs` calls it. So does the integration harness.

Before this the sequence existed twice, and the second copy was wrong in ways nothing could see: `main.rs` ran thirteen steps and `TestApp::spawn` re-implemented twelve of them differently while omitting two — no Retention sweeper, no operator log sink. Every test in the suite was therefore green about an Instance that was not the one an operator boots, and no test anywhere covered how a configuration becomes a running Instance, because there was no seam between the two to test at.

**The rule, which is this ADR's `#87` rule extended one step:** a new subsystem is **a configuration section — the subsystem's own type — wired inside `instance`**. It goes in the binary's own startup **never**, because `main.rs` is excluded from coverage and a subsystem wired there is one no test can reach. It goes in the second parameter (`Wiring`) **only if something genuinely varies between two real runs**, which today is four things:

| `Wiring` | Why it cannot be a setting |
| --- | --- |
| `store` | An S3 store built elsewhere, or a decorator that makes I/O fail (#37) — neither is expressible in TOML. |
| `clock` | What time it is. Injected so "this Call is an hour old" is a fact a test arranges rather than a sleep it waits out; #94 builds on it. |
| `credentials` | The three credentials that stay *out* of the TOML because first run writes them. See below. |
| `log_writer` | The draining half of the sink, which must be created before the subscriber and given a database after — an ordering, not a value. |

Two more are there on borrowed time and say so in their doc comments: `bind`, because the suite listens on loopback where a scanner listens on every interface (an operator-facing bind-address setting would be `[server]`'s, and is a separate ask); and `heartbeat`, which #94 removes along with the need for it.

**Credential sources became inputs rather than process-environment reads.** `Credentials { env_file, ingest_key, admin_password, vapid_key }` carries the raw configured text, exactly as `RADIO_SCOUT_API_KEY` and its two siblings hold it, and `Credentials::from_env(env_file, lookup)` is how the binary fills it — the same shape `config::resolve` already took its environment in, and for the same reason. Two consequences, both of them the point:

- A test gets a **genuinely provisioned** Instance: a real generated Web Push identity, a real `.env` written `0600` into a temporary directory of its own. `AdminAuth::locked()` and `Push::disabled()` stopped being harness knobs and became what they are — *outcomes*. An env file that cannot be written leaves the admin surface shut; a VAPID value that is not a key leaves notifications off. Both are now reachable by arranging the cause rather than by injecting the effect, which is the difference between proving the state is refusable and proving a boot can arrive at it.
- Process environment is a global. Reading it inside `start` would make two Instances in one process impossible to configure differently, and would make every test that touched it racy with every other.

**Boot ordering is observable rather than commented.** The sink is installed before the subscriber and drained after the database opens, so a line written when there was nowhere to put it — `db::connect`'s own migration lines — still reaches `GET /api/admin/logs`; provisioning happens before the port is bound, so the *first* request the port ever accepts already authenticates with the credentials that boot generated. Both are asserted in `tests/instance.rs` as consequences an Operator could see, rather than as a recorded list of steps the code would be marking its own homework with.

**A restart is stop-then-start on one handle.** `Instance::stop` is graceful and does not return until the socket is closed, so "the old Instance stopped" is a fact rather than a race; `restart_with` takes the configuration the next boot will have, which is what an Operator who edited their file and restarted actually does. It replaced eight copies of a four-line preamble in the enhancement tests that stood a second app up on a hand-shared database URL while the first one was still running.

**What did not change:** every key, every variable, every default, every exit code, and `--write-config` byte for byte. `[server] port` still decides the port. The one behavioural difference is inside the test suite, and it is the one worth stating — a spawned app now runs the sweeper and the sink for real, so the harness turns the Retention *windows* off (`days = 0`, `log_days = 0`) rather than the sweeper, because the suite dates its fixtures in 1970 and 2020 and a background sweep would otherwise decide tests by when it got to them.
