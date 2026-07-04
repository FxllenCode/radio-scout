# Radio-Scout — Design Brief

Directional brief to feed into Claude Design for full mockups. Captures the design decisions made during the grilling session; not a spec.

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
