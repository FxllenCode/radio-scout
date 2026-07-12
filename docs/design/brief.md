# Radio-Scout — Design Brief

Directional brief to feed into Claude Design for full mockups. Captures the design decisions made during the grilling session; not a spec.

**Imported mockups:** Claude Design mockups covering screen-inventory items 13–28 (overlays, states, OS/PWA surfaces) live in [`mockups/`](./mockups/) — reference only. See [`mockups/README.md`](./mockups/README.md).

## Direction

- **Aesthetic:** *Modern scanner.* Clean, premium, subtly scanner-flavored — a refined status/LED indicator, crisp tabular/mono readouts, a live waveform — **not** literal skeuomorphism. "Beautiful modernized scanner."
- **Theme:** Dark-first (used at night and in vehicles). Light mode is a later nice-to-have, not a launch requirement.
- **Platform priority:** Mobile-first PWA, responsive up to desktop. Installable to the home screen; must feel native on a phone.
- **Color:** Monochrome/near-black grayscale **chrome** with a single restrained accent for interactive/active/focus states. The **per-system/per-talkgroup LED colors are the primary pop of color and always carry meaning** (which service/system is talking). LED palette carried over: blue, cyan, green, magenta, orange, red, white, yellow.
- **Motion:** Soft glow / subtle transitions; the live indicator and waveform convey "something is happening" without noise.

## Layout & navigation

- **Mobile:** Full-screen **Live** scanner as home; a **bottom tab bar**: Live · Talkgroups · Search · Settings.
- **Desktop:** Expands to a sidebar layout (tabs become a persistent side nav; panels can sit side-by-side).

## Key screens to mock

1. **Live scanner (home)** — the hero. Status/LED indicator (color by system/talkgroup); LIVE + queue (`Q`) indicators; connection status; system label + talkgroup tag; talkgroup name/label; **live waveform**; frequency + TGID; unit ID; (de-emphasized) decode/spike error counts; **controls**: Hold System, Hold Talkgroup, Skip, Avoid (with timed 30/60/120m cycle), Replay, Pause; **recent history** (last 5 calls). Double-tap display → fullscreen.
2. **Talkgroups (select)** — group/tag **category toggles** (3-state: on/off/partial), per-system talkgroup lists with on/off toggles, all-on/all-off, and a blink state for temporarily-avoided talkgroups. Drives the live-feed selection.
3. **Search (archive)** — filters (date range, system, talkgroup, group, tag, sort); results list with play / download; playback mode; pagination. Filters cascade (system narrows talkgroup, etc.).
4. **Settings** — connection/server status; audio-enhancement toggle; **notifications (Web Push) opt-in**; theme; access-code entry; admin/password.
5. **First run / empty state** — friendly zero-config state ("waiting for the first call…"), and an **install-to-home-screen** prompt (important for iOS PWA + background audio + push).

## Mobile/PWA specifics to reflect in mockups

- **Lock-screen media controls** (Media Session): now-playing metadata (system · talkgroup) + artwork; play/pause/next/prev.
- **Install-to-home-screen** promotion and **notification-permission** flow (both required for the iOS background/push experience).

## Explicit non-goals for the visual language

- No glossy skeuomorphic bezels, segmented-LCD kitsch, or faux-hardware buttons.
- Color is not decorative — if something is colored, it should mean something.

## Full screen inventory (what to design)

### A. Primary screens (v1 — bottom-tab destinations)
1. **Live scanner (home)** — the hero.
2. **Talkgroups (Select)** — the live-feed selection surface.
3. **Search (Archive)** — browse/replay stored Calls.
4. **Settings** — connection, audio, notifications, admin, about (may be a list → detail on mobile).

### B. Live-scanner states/variants (each needs a visual treatment)
5. Playing · 6. Paused (LED blinks) · 7. Idle / "listening…" (feed on, nothing playing) · 8. Live-feed OFF · 9. Playback-mode (playing from archive) · 10. Fullscreen display (double-tap) · 11. Dimmed (after inactivity) · 12. Reconnecting / "NO LINK".

### C. Sub-screens, sheets & overlays (v1)
13. **Call detail / expanded now-playing** — full metadata, frequency-over-time, units heard, patches, download/share.
14. **Avoid control cycle** — Avoid → 30M → 60M → 120M affordance/popover.
15. **Admin login** — password gate for the config surface.
16. **Talkgroup CSV import** — file picker → preview → apply.
17. **First-run / zero-config welcome** — "waiting for the first Call…", plus the recorder-setup helper (shows API key + upload URL for Trunk Recorder/SDRTrunk).
18. **Notification-permission explainer** — in-app rationale before the OS Web-Push prompt.
19. **Install-to-home-screen (A2HS) prompt** — custom banner + iOS "Add to Home Screen" instructions.
20. **(Optional) Queue peek** — a glance at what's queued.

### D. Global states (apply across screens)
21. Empty (no Systems/Talkgroups yet) · 22. Empty search / no results / empty archive · 23. Loading / skeleton · 24. Error toasts (audio/play/search failures) · 25. Offline / server-unreachable.

### E. OS / PWA surfaces (design specs + assets, not in-app pages)
26. **Lock-screen / Media Session now-playing** — metadata + artwork + transport controls (iOS/Android/Bluetooth/CarPlay).
27. **Push notification content** — the coalesced "activity on <Talkgroup>" format.
28. **App icon (incl. maskable), splash screen, manifest theme colors.**

### F. Responsive
29. **Desktop layout** of each primary screen — bottom tabs become a sidebar; Live + Select/Search can sit side-by-side.

### Deferred to v2 (design later, not now)
- Full **admin dashboard**: Systems / Talkgroups / Groups+Tags / Units / API-keys CRUD; Options; Logs viewer; import/export tools.
- **Access-code unlock overlay** (multi-user listener PIN) + access-code management.
- Dirwatch config, Downstreams, Alerts config.
- **Light theme** variants.

### Suggested design order
1 (Live) → 2 (Talkgroups) → 3 (Search) → 13 (Call detail) → 4 (Settings) → 17 (First-run) → the A2HS/notification/admin overlays → global states → OS surfaces → desktop.
