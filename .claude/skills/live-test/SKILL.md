---
name: live-test
description: Run Radio-Scout for real and drive it in a browser — build, launch a hermetic instance, feed synthetic Calls, check the app with Claude in Chrome, report, tear down. Use when asked to live test, to see a change working in the real app, or to verify a ticket beyond its tests.
---

# Live test

The full procedure and the reasoning behind it live in
[`docs/agents/live-testing.md`](../../../docs/agents/live-testing.md). Read it
if anything below is ambiguous — this file is the running order, not the
rationale.

Arguments, if any, say what to focus on ("the queue under load", "#12"). With
none, run the standard pass.

## Running order

1. **Build the SPA.** `cd client && npm run build` — `rust-embed` reads
   `client/dist` at compile time, so skipping this serves the fallback page and
   invalidates the whole test.
2. **Start hermetic.** `rm -rf ./radio-scout-live-test`, then run the binary in
   the background with `RADIO_SCOUT_BASE_DIR=./radio-scout-live-test`, logging
   to a file. Wait for the listening line; **parse the printed API key** (first
   run generates one). Never point a live test at `./radio-scout-data` — that
   is the durable instance.
3. **Feed it.** `cargo run --example feed -- --key <KEY> …`. Pick flags for
   what is being tested: `--burst N` for the queue, `--patches A:B` for patch
   fanout, `--seconds 8` to watch the waveform, `--interval` for a steady feed.
4. **Drive the browser.** Load the Chrome tools in one `ToolSearch` call, then
   `tabs_context_mcp` → `tabs_create_mcp` → navigate to
   `http://localhost:3000`. Screenshot before and after each interaction.
   - **Never** trigger `alert`/`confirm` — a dialog freezes the extension until
     a human clears it.
   - Check `read_console_messages` before calling anything a pass.
   - If the extension is not connected, stop and say so — that needs the user,
     and the rest of the loop is still worth reporting.
5. **Report what you saw**, not what should have happened: which Calls played,
   what the queue did, what the console said, with screenshots for anything
   visual. Say plainly if something could not be checked.
6. **Tear down.** Kill the background binary. Leave
   `./radio-scout-live-test` in place — it is gitignored and the next run wipes
   it, and its DB is useful if something needs a post-mortem.

## What a standard pass covers

The things unit and integration tests cannot reach:

- a Call arrives with no interaction and plays **audible** audio
- the waveform advances; the LED matches the Talkgroup
- Hold and Avoid change **what the server sends**, not just what is displayed
- the queue count matches a burst, and Skip walks it
- Media Session shows the Call in the OS media panel and its buttons work
- the archive finds what just played, and playback mode walks it

## Boundaries

- iOS background audio, lock-screen controls and Add-to-Home-Screen are a
  **real-device manual gate** (ADR-0005). A desktop Chrome pass says nothing
  about them — do not imply otherwise.
- A live test **supplements** the suites; it never replaces them. Fix what it
  finds test-first, in the normal way.
- Nothing here writes to the durable instance or to the maintainer's Trunk
  Recorder configuration.
