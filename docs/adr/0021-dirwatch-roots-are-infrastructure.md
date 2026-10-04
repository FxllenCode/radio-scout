# Where a Dirwatch may be is infrastructure

*Decided while grilling #72 (2026-10-04). Amends the ticket's "per-watch admin config: path".*

## Context

#72 asked for **Dirwatch** entries curated in the browser, path included, as rdio-scanner has them. A watch reads every matching file in its folder, ingests its bytes as a **Call** whose audio any Listener can then download, and — with delete-after — removes it. A path field bounded only by the admin session therefore hands that session the service user's whole filesystem: point a "mask" watch with extension `conf` at `/etc` and its files become downloadable Calls, then disappear. rdio-scanner has exactly this (`dirwatch.go`: any `Directory`, any `Extension`, `os.Remove` after ingest).

The admin session is already powerful — it can delete the Archive — but every power it has is over Radio-Scout's own data. Reading and deleting the host's files is a different class, and a stolen session should not reach it. ADR-0012 already draws the line this needs: infrastructure lives in the TOML, which takes a shell to change, and the domain lives in the browser.

The grilling weighed three options: an allow-list of roots in the TOML, an audio-extension allow-list alone, and rdio parity.

## Decision

**`[dirwatch] roots` (and `RADIO_SCOUT_DIRWATCH_ROOTS`) names the folders a watch may be created inside, and the browser chooses only within them.** It is empty by default, and empty means Dirwatch is unavailable: the admin listing reports the roots, and the screen says how to turn Dirwatch on instead of offering a form that would be refused.

- **Both sides are resolved before they are compared**, symlinks and `..` included, so neither can walk a watch out of its root. The folder is stored resolved.
- **The bound is checked every time a watch starts**, not only when it is saved. A root removed from the TOML stops every watch inside it, and its row says `outside-roots`.
- **What a watch may pick up is closed as well**: audio extensions only (`wav`, `mp3`, `m4a`, `aac`, `ogg`, `opus`, `flac`), plus Trunk Recorder's `.json`. Symlinks inside the folder are not followed.
- A root must be an absolute path, refused at boot otherwise, because a relative one would resolve against whichever directory the service manager started in.

Three other decisions from the same grilling are recorded in the module rather than here, because they are design rather than posture:

- watching by the operating system's events, with a per-watch poll for network shares;
- a per-watch **watermark** rather than a ledger of files, for the watch that keeps its files;
- times with no zone read in the Instance's own (`src/dirwatch/mod.rs`, `src/dirwatch/worker.rs`).

## Consequences

- Turning Dirwatch on takes one line of configuration and a restart. The Operator who wants it is running a recorder on the same machine, so has a shell anyway.
- An rdio dirwatch does not migrate as a row. It is re-created by hand inside the allowed folders (`docs/migrating-from-rdio-scanner.md`).
- **Dirwatch entries are not in the configuration document** (#51). A watch names a path on one machine, bounded by that machine's TOML. A document that carried one would either fail on every other Instance or need the roots to travel too, and the roots are exactly what must not travel through a browser.
- Docker needs the recorder's folder mounted in and named as a root (`docs/deploy.md`).
