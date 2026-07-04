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
- Adding v2 access codes reuses the same scope model as API keys ([server analysis](../research/) — scope = `"*"` or `[{id, talkgroups}]`).
