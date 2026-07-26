import { defineConfig, devices } from '@playwright/test'

/**
 * The narrow browser-only layer ADR-0010 reserves Playwright for: PWA install
 * criteria, the service worker, and offline (#15).
 *
 * None of it is testable anywhere else — jsdom has no service worker, no cache
 * storage and no manifest processing, and the worker itself only exists in a
 * production build. So this suite runs against `vite preview` over the real
 * `dist/`, which is also what the Rust binary embeds (ADR-0007).
 *
 * Everything else stays in Vitest. This is deliberately four specs.
 */
const PORT = 4173

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'on-first-retry',
  },
  // Chromium only: this suite is about the service-worker/manifest contract,
  // which is a standard. The platform that actually matters here — iOS Safari —
  // is a real-device manual gate (ADR-0005), not something WebKit-on-macOS
  // could stand in for.
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    // A service worker only exists in a production build, so this builds.
    command: `npm run build && npx vite preview --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}/`,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
})
