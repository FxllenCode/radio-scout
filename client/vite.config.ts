/// <reference types="vitest/config" />
import path from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// In dev, the SPA runs on Vite's server and proxies the API + live-feed
// WebSocket to the Rust backend, so the app sees a single origin — matching
// production, where the built SPA is embedded and served by the binary itself.
export default defineConfig({
  plugins: [react(), tailwindcss()],
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
        'src/components/ui/**',
        'src/**/*.d.ts',
      ],
      // Ratcheting project floor (ADR-0010): below the measured baseline, only
      // ever raised. Raised with #11 (live feed + scanner display), which took
      // the measured numbers to 100% lines and ~96% branches.
      thresholds: {
        lines: 95,
        functions: 95,
        statements: 95,
        branches: 90,
      },
    },
  },
})
