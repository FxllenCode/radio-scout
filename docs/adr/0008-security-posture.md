# Security posture: cookie-session admin, hashed API keys, open v1 listening

## Context

v1 must secure the admin/config surface and authenticate recorders while keeping the install simple. rdio-scanner used JWT (with an in-memory token allowlist) for admin auth and compared API keys as plaintext. Radio-Scout is single-origin and single-server, which changes the best-practice calculus.

## Decision

- **Admin auth: an httpOnly + Secure + SameSite session cookie** backed by server-side session state — **not** JWT-in-localStorage. Simpler for a single-origin app, immune to XSS token theft, and trivially revocable.
- **Ingest auth: per-system API keys**, high-entropy tokens, stored **hashed** and matched by hash on each upload.
- **Brute-force guard** on the admin login (lockout after N failed attempts per IP).
- **v1 listening is open.** Public exposure is secured externally (reverse proxy / VPN / Cloudflare Access), documented for operators. Full multi-user **access codes** (per-listener PINs with per-system/talkgroup scopes, expiry, connection limits) are a **v2** feature.
- **TLS:** plain HTTP by default with a **reverse proxy recommended** for HTTPS in v1; built-in Let's Encrypt autocert is a v2 convenience.

## Considered and rejected

- **JWT-in-localStorage** (rdio's approach) — susceptible to XSS token theft and unnecessary for a single-origin deployment; a server-side cookie session is both simpler and safer.

## Consequences

- Exposing a v1 instance directly to the internet without a fronting auth layer means open listening — this must be clearly documented.
- The admin/config surface is always password-gated; recorders always require a valid per-system key.

  **Open deviation (#18 → #19).** The first admin endpoint —
  `POST /api/admin/talkgroups/import` (Talkgroup CSV import) — shipped **before**
  the cookie session that gates it, so it is currently **unauthenticated**.
  Anyone who can reach the server can, through it: rewrite Talkgroup labels,
  names, Tags, Groups, and LED colors; **create** Talkgroups, Tags, Groups, and
  — for an unknown numeric `system` value — **Systems**, unbounded and one per
  distinct value. It cannot read, download, or delete Calls, cannot delete any
  entity row, and cannot touch API keys. Every admin route is parked under the `/api/admin/`
  prefix so **#19 closes this with a single `route_layer`** rather than a hunt
  through handlers. Until #19 lands, an instance reachable beyond a trusted LAN
  should keep `/api/admin/` blocked at the reverse proxy.
- Adding v2 access codes reuses the same scope model as API keys ([server analysis](../research/) — scope = `"*"` or `[{id, talkgroups}]`).
