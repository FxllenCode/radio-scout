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
    globals: true,
    setupFiles: './src/test/setup.ts',
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
      // Ratcheting project floor (ADR-0010): below the measured baseline
      // (~94%), only ever raised. Per-file 100% on pure logic (store/lib) can be
      // added once those modules stabilize.
      thresholds: {
        lines: 85,
        functions: 85,
        statements: 85,
        branches: 80,
      },
    },
  },
})
