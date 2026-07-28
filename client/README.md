# Radio-Scout — web app

The listener-facing app: Vite + React + TypeScript, Tailwind v4, shadcn/ui, Redux Toolkit /
RTK Query. It is a PWA, and on iOS it is the only thing that makes background audio work
([ADR-0005](../docs/adr/0005-client-audio-media-session-background.md)).

**This is not deployed separately.** The production build is compiled *into* the Rust binary by
`rust-embed`, and the API, the WebSocket and the app are all served from one origin
([ADR-0007](../docs/adr/0007-single-binary-embedded-frontend-distribution.md)).

> **`npm run build` must run before `cargo build` or `cargo test`.** `rust-embed` reads
> `client/dist` **at compile time**. Without it the binary serves a minimal fallback page — and
> the Rust tests that assert about the frontend then pass by asserting the *fallback*, which is
> green and proves nothing.

## Commands

```sh
npm install          # first time
npm run dev          # Vite dev server; proxies /api, /healthz and the WS to :3000
npm run build        # type-check + production build into dist/, which the binary embeds
npm run typecheck    # tsc -b
npm run lint         # oxlint
npm run test         # Vitest + React Testing Library
npm run test:watch
npm run test:coverage  # with thresholds; MSW mocks at the network boundary
npm run test:e2e     # Playwright: service worker, offline, install criteria
```

For the dev server to be useful, run the backend too — `cargo run` in the repository root.

## Testing

Integration tests with React Testing Library are the workhorse, with **MSW** mocking at the
network boundary — never `fetch` or module mocking. Unit tests cover `store/`, `lib/` and
`utils/`. Coverage thresholds are enforced and ratchet upward.

Playwright is reserved for what jsdom cannot do at all: service-worker registration, offline
app-shell serving, install criteria, and delivering a real push. It runs against a production
build served by `vite preview`, because the service worker only exists in a build.

**iOS background audio, lock-screen controls and Add-to-Home-Screen are a real-device manual
gate.** Playwright's bundled WebKit is not iOS Safari and cannot validate them.

## Icons

App icons are rasterised from `icons/icon.svg` by `scripts/build-icons.sh` into `public/`. The
PNGs are committed, so neither the build nor CI runs it — re-run it by hand only when the mark
changes.

---

Project overview: [../README.md](../README.md) · Contributing and the full test policy:
[../CLAUDE.md](../CLAUDE.md)
