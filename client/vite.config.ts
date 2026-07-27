/// <reference types="vitest/config" />
import path from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { VitePWA } from 'vite-plugin-pwa'

// In dev, the SPA runs on Vite's server and proxies the API + live-feed
// WebSocket to the Rust backend, so the app sees a single origin — matching
// production, where the built SPA is embedded and served by the binary itself.
export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    // The PWA half of #15. Installing is the gate on everything mobile: iOS
    // gives a home-screen app the standalone display mode background audio
    // needs (ADR-0005), and offers Web Push (#16) to nothing else.
    VitePWA({
      // We register the worker ourselves (src/lib/serviceWorker.ts) because
      // *when a new version takes over* is a product decision here, not a
      // default — see that module.
      injectRegister: false,
      registerType: 'prompt',
      // Our own worker source (#16): a generated worker cannot have a `push`
      // handler, and handling `push` is the whole of Web Push on the device.
      // Everything the generated one did — precache, navigation fallback, the
      // `/api` denylist — `src/sw.ts` now does explicitly.
      strategies: 'injectManifest',
      srcDir: 'src',
      filename: 'sw.ts',
      manifest: {
        name: 'Radio-Scout',
        short_name: 'Radio-Scout',
        description:
          'Scanner audio from Trunk Recorder and SDRTrunk — live feed, archive, and background playback.',
        // Required for iOS Web Push, and what takes us out of a browser tab.
        display: 'standalone',
        start_url: '/',
        scope: '/',
        // The mockup's manifest colors (design brief 28): the app is dark
        // before a pixel of it has loaded, so the splash never flashes white.
        background_color: '#09090b',
        theme_color: '#0c0c0e',
        orientation: 'portrait',
        categories: ['utilities', 'news'],
        icons: [
          {
            src: '/icon-192.png',
            sizes: '192x192',
            type: 'image/png',
            purpose: 'any maskable',
          },
          {
            src: '/icon-512.png',
            sizes: '512x512',
            type: 'image/png',
            purpose: 'any maskable',
          },
        ],
      },
      injectManifest: {
        // The app shell, fonts included — a phone that opens the app offline
        // should get the app, not a browser error page. (`clientsClaim`,
        // `skipWaiting` and the navigation denylist moved into `src/sw.ts`,
        // which is where they are now decided.)
        globPatterns: ['**/*.{js,css,html,svg,png,ico,woff2}'],
      },
    }),
  ],
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  server: {
    proxy: {
      '/api': { target: 'http://localhost:3000', changeOrigin: true, ws: true },
      '/healthz': 'http://localhost:3000',
    },
  },
  test: {
    environment: 'jsdom',
    // The Playwright suite is a different runner against a different target
    // (a real browser over a real build) — Vitest must not try to run it.
    exclude: ['e2e/**', 'node_modules/**', 'dist/**'],
    // One test origin for everything: the relative-URL shim in src/test/setup.ts
    // resolves fetches against `http://localhost`, and the live-feed socket
    // derives its URL from `location` — they have to agree for MSW to match
    // both.
    environmentOptions: { jsdom: { url: 'http://localhost/' } },
    globals: true,
    setupFiles: './src/test/setup.ts',
    // Spies (`vi.spyOn(HTMLMediaElement.prototype, 'play')`) are per-test by
    // construction: without this, one test's stubbed media element silently
    // shapes the next one's.
    restoreMocks: true,
    coverage: {
      // V8 with AST-aware remapping (Vitest 4) — Istanbul-grade accuracy.
      provider: 'v8',
      reporter: ['text', 'html', 'lcov'],
      include: ['src/**/*.{ts,tsx}'],
      // ADR-0010 exclusions: tests, the MSW harness, the entrypoint, generated
      // shadcn primitives, and type-only modules.
      exclude: [
        'src/**/*.{test,spec}.{ts,tsx}',
        'src/test/**',
        'src/main.tsx',
        // The service worker is a different global scope with no jsdom
        // implementation (no `PushEvent`, no `clients`, no precache manifest).
        // Everything in it with a decision is `lib/pushMessage.ts`, at 100%;
        // what is left is glue, covered by the Playwright layer and the
        // real-device gate (ADR-0010).
        'src/sw.ts',
        'src/components/ui/**',
        'src/**/*.d.ts',
      ],
      // Ratcheting project floor (ADR-0010): below the measured baseline, only
      // ever raised. Raised with #16 (Web Push), which holds 100% lines and
      // ~97% branches with the push manager, the notification builder and the
      // Settings switch all covered.
      thresholds: {
        lines: 99,
        functions: 98,
        statements: 98,
        branches: 94,
      },
    },
  },
})
