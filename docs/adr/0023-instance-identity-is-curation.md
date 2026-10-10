# Instance identity is curation; the listener cap is configuration

*Decided while grilling #77 (2026-10-10). Overrides that ticket's "configurable in admin and TOML", and records what spec v2's US 3, 4, 56, 57 and 58 became.*

## Context

#77 asked for an Instance's name, the rdio "options" cluster (listener count display, 12/24-hour time, talkgroup sort, playback-goes-live, display dimmer, help link), a reference page and a listener cap — and asked for the name to be **configurable in admin and TOML**. Nothing in the database was Instance-wide before this: every Instance-wide setting lived in `radio-scout.toml`, every edited entity in the database (ADR-0012, CONTEXT.md **Curation**). Two homes for one value is a precedence rule every Operator would have to know, and ADR-0012 has none for it: either the file silently loses to a form, or the form greys out over a value some file elsewhere set.

rdio keeps all of it, the machine included, in one `options` document behind its admin page, pushed down the live socket in its config frame.

## Decision

**The identity is curation, and only curation.** The name, the help link, the defaults (`timeFormat`, `panelSort`, `playbackGoesLive`, `dimmerSecs`, `showListeners`) and the **Reference page** are one row in the database (`identity`, `src/identity/`), edited at Settings → Admin → Instance, never written in `radio-scout.toml`. It is what Listeners *see*, not how the machine runs — ADR-0012's "everything operational is in the file" is untouched, because none of this is operational. Read once at boot, held in memory, and replaced by the request that writes it. An Instance with no row reads every default and calls itself Radio-Scout, so a fresh install serves what it served before.

**The listener cap is configuration**: `[live] max_listeners`, 200 (rdio's `maxClients`), at least 1. It protects the machine — a Pi's upload, at about 4 KB/s per listener in a busy minute — so it belongs beside `[station] max_streams`. `0` is refused at boot rather than read: rdio's `0` seats nobody, `max_streams = 0` means off, and a third meaning for the same digit would be one too many. There is no "unlimited"; an Operator who means it writes a large number.

Four calls made while grilling, with the alternative each beat:

- **`panelSort` is where a Profile starts, not an override.** rdio's `sortTalkgroups` replaced the Listener's order; #57 had already given Listeners an order of their own, and taking it back would be a regression. A Profile that has chosen keeps its choice; one that has not follows the Operator. A by-Ref order was added for rdio's default (its admin order, which was RadioReference's TGID order); an Operator-defined manual order was not built.
- **`timeFormat` is the Operator's, for everyone, everywhere** — the display, search, the queue, the admin screens and an embedded player — rather than a per-Listener setting or the device's locale.
- **The Reference page is never published unasked.** The editor offers a seed holding only what is the same everywhere (the NATO and APCO alphabets, the 24-hour clock); no entry is drawn until the Operator publishes. The spec's "seeded" and the ticket's "absent page hides the entry" are both kept this way. It is rendered on the server (`pulldown-cmark`) with raw HTML shown as text and link schemes allow-listed, so the page is markdown and nothing that runs, whoever holds the admin password.
- **The name reaches the installed app.** The binary writes it into `manifest.webmanifest` as it serves it, `no-cache`, and the service worker leaves that file out of its precache, so a rename reaches a phone that installed the app before it.

## Consequences

- **A full Instance says so and says nothing more.** The live socket is upgraded, sent `{"t":"refused","reason":"instance-full"}` and closed — the frame #68 introduced for a code at its limit, which the client now reads for both — and the client keeps retrying, showing *full* rather than *no link*. The frame carries no number: how many are listening is the Operator's (CONTEXT.md, **Listener count**), and a cap told to whoever it refuses would publish it for them.
- **The Listener count, when shown, rides the heartbeat** every connection already has, sent only when it moves — where rdio pushes it to every socket on every connect and disconnect, N² frames when a popular moment brings N people in.
- **The palette doubled to sixteen**, appended after rdio's eight so the hash fallback keeps half an uncurated archive on the colour it had. Still a closed list, still pinned between `src/import.rs` and `client/src/lib/led.ts` by a test.
- **Not in the configuration document (#51) yet.** `GET /api/admin/config` carries entities, and the identity is now curation too; a backup restored to a new Instance does not bring the name or the page with it. Left to a follow-up rather than guessed at here, because the document's versioning rules decide how an absent section is read.
