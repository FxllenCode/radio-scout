/// <reference types="vitest/config" />
import path from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { playwright } from '@vitest/browser-playwright'

/**
 * The real-browser layer ADR-0010 reserves Vitest Browser Mode for: the audio
 * player and its Media-Session wiring, in an engine that actually has a media
 * stack (#34).
 *
 * # Why it cannot live in the jsdom suite
 *
 * jsdom has no media stack at all. `src/test/setup.ts` defines `play`, `pause`
 * and `load` onto `HTMLMediaElement.prototype` because otherwise they raise "not
 * implemented", and every test that cares about `duration`, `currentTime` or
 * `paused` hand-defines them. That is enough to prove *what the component
 * decides* — which is most of it, and stays there — but it cannot prove anything
 * about what a browser then does:
 *
 * - that swapping `src` on the one reused element really restarts playback,
 * - that the keep-alive loop (`lib/silence.ts`) is a WAV a real decoder accepts,
 * - that `MediaMetadata` and `setPositionState` accept what we hand them,
 *   including the indexed PNG artwork `lib/artwork.ts` encodes by hand,
 * - that a rejected `play()` is caught, without a mock to make it reject.
 *
 * # Its own config, not a second project
 *
 * `npm run test` stays the fast jsdom workhorse (a few seconds, no browser
 * binary needed), and this runs on demand and in CI as `npm run test:browser`.
 * Folding both into one command would put browser startup in front of every
 * red-green cycle, which is the loop TDD is actually made of.
 *
 * jsdom also owns the **coverage profile** the ratcheting floor and the patch
 * gate are measured from — one measured profile, the way the Rust `Backend` job
 * owns the one Rust profile while the arm64 job (#38) does not. What is asserted
 * here is not line reachability; it is that a real engine accepts our bytes.
 *
 * # Where the boundary is
 *
 * Chromium on a desktop. It cannot validate iOS background audio or lock-screen
 * controls — Playwright's bundled WebKit is not iOS Safari, and the OS behaviour
 * is the point. That stays the real-device manual gate (ADR-0005, #33).
 */
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { '@': path.resolve(import.meta.dirname, './src') },
  },
  test: {
    // Named, so `--project` can pick it out and a failure says which layer
    // failed rather than just "a test".
    name: 'browser',
    include: ['src/**/*.browser.test.{ts,tsx}'],
    globals: true,
    setupFiles: './src/test/setup.browser.ts',
    browser: {
      enabled: true,
      // Reuses the Chromium the Playwright suite (#15) already installs, so the
      // marginal tooling cost of this layer is a dev-dependency and no download.
      provider: playwright(),
      headless: true,
      // One instance. Cross-engine differences are not what this layer is for:
      // it exists to prove the wiring works in *a* real media stack, and the
      // engine that would actually differ is the one behind the manual gate.
      instances: [{ browser: 'chromium' }],
      // Chromium's **autoplay policy is left on**, deliberately, and it is worth
      // saying why: it would have been easy to launch with
      // `--autoplay-policy=no-user-gesture-required` and make every test here
      // simpler. Leaving it gives this layer two real things instead of one
      // convenient one. A test that wants audio performs a real click first —
      // which is exactly what unlocks playback on a device, where the listener's
      // first tap grants activation for the life of the page. And a test that
      // wants `play()` to be *refused* simply doesn't click, so the rejection
      // arm runs against the browser genuinely saying no, rather than a mock
      // pretending to.
      // Vitest's own UI would be a second thing to keep working for no gain
      // here; failures are read from the terminal like every other suite.
      ui: false,
      screenshotFailures: false,
    },
    // No coverage: see the note above.
    coverage: { enabled: false },
  },
})
