# A recorder can name the System it files under

*Decided while reviewing #109 (2026-09-28 to 2026-10-04), tracked as #111. Reverses #44's "No `systemId`".*

## Context

#44 shipped the Trunk Recorder plugin with a deliberate omission: "No `systemId` — the native endpoint resolves the System from `short_name`." That holds while one recorder `system` is one network. It breaks for a **multi-site** network, which Trunk Recorder configures as several `system` entries, each with its own `shortName`:

- Each site arrived as a System of its own, named after the site.
- **Dedup is per System** (#46), so one transmission heard by two sites became two Calls, and a Listener heard it twice.
- An API key scoped to the network's System could not authorise its sites, whose labels did not match it.

The fix arrived as an outside contribution from an Operator running such a network (#109). Naming the Ref turned out to be **rdio parity**, not invention. rdio-scanner's own Trunk Recorder endpoint honours a `system` part and checks the Ref before the label (`server/parsers.go:390`, `server/controller.go:123-131`), and `systemId` is the rdio uploader's key for the same setting.

The alternative was **server-side aliases**: a System answering to several `shortName`s, configured in Admin the way #45 gives a Talkgroup member Refs. It would leave recorder configs untouched, but it costs schema and Admin UI and is not parity. It was not taken, and is worth revisiting only if a second Operator asks for it.

## Decision

**A recorder may name the System Ref itself, and a named Ref wins over any label match.** The native endpoint reads an optional `system` part. Both shipped ways in carry it, as the two-artifact rule requires: `--system <ref>` on `radio-scout-upload.sh`, and a per-system `systemId` in the plugin, using the rdio uploader's own key.

- **The `shortName` names the Site instead.** When a Ref is named, the `shortName` becomes the **Site** that heard the Call (spec US 11). A System created from a named Ref takes #8's default label, `System <ref>`, rather than whichever site happened to arrive first. With no Ref named, nothing changes.
- **An unknown named Ref follows Auto-populate**, exactly as any unknown System does. It is one policy (#92), not a second rule.
- **A value that is not a Ref is refused by the recorder, on every Call.** Falling back to the `shortName` would file a typo's Calls under whatever System that name matches.
  - The script exits non-zero, which Trunk Recorder logs as a failed upload.
  - The plugin logs an ERROR and answers 0, because no retry can fix a setting.
  - Both accept 1 through 999999999999999999. Eighteen digits always fit Radio-Scout's i64, and the plugin reads a fixed-width integer because `long` is 32 bits on 32-bit Raspberry Pi OS.
- **The server ignores an unusable `system` part**, as the generic endpoint does, and says so with a bounded WARN. The part arrives before the key is checked, so the line carries its length and an escaped head of 32 characters, never the part.

## Consequences

- **Folded sites deduplicate as one System**: one transmission heard by two sites is one Call, and keep-best keeps the better copy's Site with it.
- **A fresh Ref strands what the site already had.** Its old System keeps the Talkgroup names, groups, restrictions and retention, and a System a recorder creates is unrestricted. `docs/recorders.md` therefore tells Operators to reuse the Ref their site already landed under.
- **Three gaps this widened, each tracked:**
  - Receive health (#71) is keyed by System, frequency and SDR, so folded sites' same-numbered SDRs merge into one row: **#113**.
  - Copies from several sites now arrive at the same moment routinely, which races dedup (pre-existing): **#116**.
  - A Site number Radio-Scout mints from a name shares a column with numbers a recorder reports: **#117**.
- **The plugin's own config parsing is compiled in CI but never run by a test**: #112.
