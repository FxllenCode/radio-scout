# Configuration: TOML + flags + environment, strictly validated, loudest layer wins

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
