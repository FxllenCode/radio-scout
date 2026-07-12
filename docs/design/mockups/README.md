# Design mockups (imported from Claude Design)

**Reference only — do not implement directly.** These are high-fidelity mockups exported from Claude Design to encode the visual language defined in [`../brief.md`](../brief.md). They are a target to design/build *toward*, not source to copy.

## Contents

| File | What it is |
|---|---|
| `live-scanner.dc.html` | Claude Design `.dc.html` mockup (~227 KB). Despite the name, it covers **screen-inventory items 13–28**: Call detail, Avoid cycle, Admin login, CSV import, First-run, Notification explainer, A2HS, Queue peek, and the empty / loading / error / offline **states**, plus the **OS/PWA surfaces** (Media Session lock-screen, Push content, App icon/splash/manifest). |
| `support.js` | The Claude Design "dc-runtime" that renders the `.dc.html` (parses `<x-dc>`/`<helmet>`). Must sit beside the HTML. |

The **primary screens** (inventory items 1–12: Live scanner, Talkgroups, Search, Settings, and the live-scanner states) are **not** in this artifact yet.

## How to view

Open `live-scanner.dc.html` in a browser **with internet access** — `support.js` loads React 18 + Babel from `unpkg.com` and fonts from Google Fonts at runtime. It is not fully offline-renderable.

## Source & re-syncing

- **Source:** Claude Design project `0cf0eed2-dbc5-4f49-9b79-5c7e331476b0`, file `Live Scanner.dc.html`.
- **Imported:** 2026-07-12 via the `DesignSync` tool (read-only pull).
- To refresh or pull additional design files, use the `DesignSync` tool / `/design-sync` skill against that project id.

## Not yet imported

The source project also contains radio-scout **logo/icon assets** (`logo/radio-scout-{32,64,128,256,512,1024}.png`) — useful later for the PWA icon / favicon / splash. Pull them when the frontend work needs them.
