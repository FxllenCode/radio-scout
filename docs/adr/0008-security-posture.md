# Security posture: cookie-session admin, hashed API keys, open v1 listening

## Context

v1 must secure the admin/config surface and authenticate recorders while keeping the install simple. rdio-scanner used JWT (with an in-memory token allowlist) for admin auth and compared API keys as plaintext. Radio-Scout is single-origin and single-server, which changes the best-practice calculus.

## Decision

- **Admin auth: an httpOnly + SameSite session cookie**, `Secure` whenever the request arrived over TLS, backed by server-side session state — **not** JWT-in-localStorage. Simpler for a single-origin app, immune to XSS token theft, and trivially revocable. (`Secure` is conditional rather than constant because this ADR also chooses plain HTTP by default, and a browser discards a `Secure` cookie sent over plain HTTP; see "How admin auth is built" below for how TLS is detected and what an operator must do to get the flag.)
- **Ingest auth: per-system API keys**, high-entropy tokens, stored **hashed** and matched by hash on each upload.
- **Brute-force guard** on the admin login (lockout after N failed attempts per IP).

### How admin auth is built (#19, `src/admin.rs`)

Every choice below is a place rdio-scanner's `server/admin.go` is worth beating, not merely matching.

- **The credential lives where the ingest key does.** `RADIO_SCOUT_ADMIN_PASSWORD`, from the environment or `.env`; first run generates a random one, writes it `0600` and logs only the path (ADR-0011 rule 2). rdio ships a **known default password** (`rdio-scanner`, `defaults.go`) behind a `passwordNeedChange` nag, so a fresh instance is open to anyone who has read its source. We never have a guessable credential at all. If the generated one cannot be saved, **no password is set and the admin surface stays closed** — a credential only the server ever saw would lock the operator out while convincing them they had one.
- **Verified with Argon2id** (OWASP's 19 MiB / t=2 / p=1) rather than bcrypt, so each guess costs real work even before the lockout. The hash is computed once at boot and held in memory; there is no durable copy to steal, because the password is re-read from the environment every boot.
- **Sessions are in-memory, bounded twice.** A rolling **idle window** (8 h, refreshed by use) so an operator working a long evening is never signed out mid-edit, inside an **absolute lifetime** (7 d, never refreshed) that bounds a cookie somebody walked off with. rdio's tokens carry no `exp` claim at all and live until the process restarts. The table is capped at 32 with expired entries reclaimed first; rdio's cap is **five, evicted unconditionally**, so logging in from a sixth place silently signs out the first. A restart revokes everything, which for a configuration surface is the right default.
- **CSRF: a synchronizer token**, generated with the session, held server-side, returned by `POST /api/admin/login` and `GET /api/admin/session`, and required in `X-CSRF-Token` on every unsafe method under `/api/admin/`. Not the double-submit-cookie pattern, which an attacker who can write cookies on a sibling subdomain can forge. rdio needs no CSRF defence because it authenticates with an `Authorization` header; a cookie session buys XSS resistance and owes this in exchange. `SameSite=Strict` is the first line, this is the one that does not depend on the browser.
- **`Secure` follows the transport, not a constant.** v1 serves plain HTTP and recommends a reverse proxy for TLS (below), and a browser silently discards a `Secure` cookie sent over plain HTTP — so setting it unconditionally would make admin login impossible on exactly the zero-config LAN install this project exists to make easy. The flag rides when `X-Forwarded-Proto: https` arrives from a peer named in `[server] trusted_proxies` (#17), which is the same trust decision `X-Forwarded-For` already goes through.

  **The known limitation, stated plainly:** an operator who terminates TLS at a proxy but leaves `trusted_proxies` empty — which is what ships, and which they have no *logging* reason to change — gets a working session cookie **without** `Secure`, and a browser will then replay it over any `http://` request to the same host. The fix is one line of configuration (`[server] trusted_proxies = ["127.0.0.1"]`, or whatever the proxy's address is), and it is called out in the `[admin]` section of `--write-config` and in `.env.example`. A `[admin] cookie_secure = "always"` override was considered and deliberately deferred: it is a second way to say the same thing, and the trust list is the setting an operator behind a proxy should be setting anyway. Revisit if real deployments trip over it.
- **The lockout counts the address the network established.** rdio keys its ledger on `GetRemoteAddr`, which reads `X-Forwarded-For` unconditionally (`main.go:265`) — so on a public instance an attacker rotates the header and is never locked out, *and* can lock anyone else out by forging their address. Three further rdio bugs are fixed here: its cooldown is `time.Duration(time.Duration.Minutes(10))` (`admin.go:64`), a method expression that evaluates to **zero**, so failures never decay and three wrong passwords lock an address until the process restarts; a successful login clears the ledger for **everybody**, handing every attacker a fresh budget; and a locked address gets the same 401 as a wrong password, so an operator cannot tell "wrong password" from "stop trying". Ours decays from the last attempt, clears only the address that authenticated, and answers **429 with `Retry-After`**.
- **The guard is a prefix layer** over the admin router, so a route added beside the others is protected by construction rather than by remembering — which is what made #18's deviation, below, cheap to close.
- **Policy is configurable without a UI** (`[admin]`, ADR-0012) — necessarily, since the UI it gates is the thing you would need it for. A zero for any of the four windows refuses to boot rather than bricking the surface.
- **v1 listening is open.** Public exposure is secured externally (reverse proxy / VPN / Cloudflare Access), documented for operators. Full multi-user **access codes** (per-listener PINs with per-system/talkgroup scopes, expiry, connection limits) are a **v2** feature.
- **TLS:** plain HTTP by default with a **reverse proxy recommended** for HTTPS in v1; built-in Let's Encrypt autocert is a v2 convenience.

## Considered and rejected

- **JWT-in-localStorage** (rdio's approach) — susceptible to XSS token theft and unnecessary for a single-origin deployment; a server-side cookie session is both simpler and safer.

## Consequences

- Exposing a v1 instance directly to the internet without a fronting auth layer means open listening — this must be clearly documented.
- The admin/config surface is always password-gated; recorders always require a valid per-system key. (`POST /api/admin/talkgroups/import` shipped unauthenticated with #18 and was the whole reason the prefix existed; **#19 closed it with one `route_layer`**, as planned.)
- **An operator who loses the admin password has a way back in**: it is `RADIO_SCOUT_ADMIN_PASSWORD` in `.env`, readable with `cat` and replaceable with an edit and a restart. There is no recovery flow to build, and no reset endpoint to attack.
- **Argon2id costs ~12 ms per login** (measured, release build, Apple silicon; a Pi 5 is some multiple of that and still far under a human's threshold). That cost is the point — it is what makes each guess expensive — and it is paid only on login, never per request. A locked-out attempt answers in **~0.3 ms** because the lockout is consulted *before* the hash, so a spent address buys the attacker nothing and cannot be used to exhaust a Pi's 19 MiB-per-hash working set.
- **Sessions do not survive a restart.** An operator mid-edit when the service is upgraded logs in again. Accepted deliberately: a durable session table would need a migration and a sweeper to make a configuration surface *less* revocable.
- Adding v2 access codes reuses the same scope model as API keys ([server analysis](../research/) — scope = `"*"` or `[{id, talkgroups}]`).
