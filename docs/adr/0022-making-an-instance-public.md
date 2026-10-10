# Making an Instance public: a tunnel first, built-in TLS for the rest

*Decided while grilling #76 (2026-10-10). Amends ADR-0008's TLS line and its trust posture, and spec v2's US 61 framing.*

## Context

#76 asked for built-in TLS — rustls with ACME autocert — so a public Instance could be one binary with no proxy in front. The maintainer runs their own scanner behind a **Cloudflare Tunnel**, and asked during the grilling for that to be the recommended way public. The two are not rivals: a tunnel needs no certificate on the Pi at all, so built-in TLS stops being *the* way public and becomes the way for an Operator who will not put a third party in front.

Two facts surfaced while grilling, both of which shaped the decision more than the feature did:

- **Behind a tunnel on the same machine, every visitor is `127.0.0.1`.** With `[server] trusted_proxies` empty — the posture ADR-0008 shipped — the admin lockout and the **Access code** lockout counted them as one address. Five bad guesses from anybody on the internet locked the Operator out of their own admin surface, from everywhere, for fifteen minutes. And the session cookie never got `Secure`, ADR-0008's own "known limitation".
- **Cloudflare's CDN terms name audio files** ("a disproportionate percentage of pictures, audio files, or other large files"), and a scanner's payload is audio. That is the Operator's judgment to make, but it is a real reason someone would want a path that does not go through Cloudflare.

## Decision

**Three ways public, recommended in this order: a Cloudflare Tunnel, a reverse proxy, built-in TLS.** `docs/deploy.md` puts them side by side. Built-in TLS ships, because rdio-scanner has `ssl_auto_cert` and `ssl_cert_file` and parity is a floor (CLAUDE.md).

- **Loopback is a trusted proxy by default** — `trusted_proxies = ["127.0.0.1", "::1"]`. A tunnel or proxy on the same machine then works with nothing configured: real client addresses in the log, per-visitor lockouts, a `Secure` cookie. A process on this machine is already the Operator's, so believing it costs nothing that believing a LAN peer would; `[]` still means "believe nobody".
- **Built-in TLS is either ACME or the Operator's own files.** `[tls] domains` switches ACME on (Let's Encrypt unless `directory` says otherwise); `cert_file` + `key_file` serve an Operator's own certificate, **re-read every minute** so a certbot or `tailscale cert` renewal lands without a restart. Both at once refuses to boot.
- **ACME answers both challenges, TLS-ALPN-01 first**, falling back to HTTP-01. TLS-ALPN-01 needs only the port the site is on; rdio's `autocert` only ever did this one, so an rdio migrant with only 443 forwarded keeps working, and one with only 80 forwarded starts working.
- **With built-in TLS on, the plain port is a LAN door.** It answers an HTTP-01 challenge to anyone, serves the whole app to loopback and private-network peers, and redirects everyone else to HTTPS. Decided on the **TCP peer**, never a forwarded address. A Trunk Recorder on the same Pi keeps posting to `http://127.0.0.1:3000`, which matters because posting to the public name from inside a home network needs hairpin NAT many routers lack — and the maintainer's scanner is exactly that setup.
- **The CA's terms are accepted implicitly** by naming a domain; the account's creation logs the terms URL the CA's own directory names. What rdio and Caddy do.
- **Certificate health is visible**: an ERROR line on every failed attempt, the status page's HTTPS card (expiry, renewal, the last error in the CA's words), and `radio_scout_tls_certificate_expiry_timestamp_seconds` / `radio_scout_tls_certificate_failing` in `/metrics`.

## Consequences

- **The test seam is Pebble**, Let's Encrypt's own test CA, behind `TEST_ACME_DIRECTORY` in its own CI job and in `Backend` — the spec's "local ACME directory stub". Pebble validates on fixed ports, so `tests/acme.rs` runs one test at a time. Everything that does not need a CA (the listener, the LAN door, the `Secure` cookie, the files source) runs in the ordinary suite against a test CA of rcgen's.
- **Renewal runs on the Instance's own `Clock`**, as a Worker, which is why the client is `instant-acme` rather than `rustls-acme`: the latter keeps its own timer, which the suite could only have slept on.
- **No HSTS, and a 307 rather than a 308.** Both are sticky in a browser for longer than an Operator who later turns built-in TLS off would expect, and a plain address that every browser has learned to refuse is a scanner nobody can reach. (The ticket's decision said 308; the maintainer confirmed 307 in review, for this reason.)
- **Trusting loopback trusts whatever relays from it.** A tunnel or a proxy *appends* the visitor's address to `X-Forwarded-For`, which is what makes believing it safe. A raw TCP forwarder on the same machine — `socat`, `ssh -L`, a router-in-software — passes the visitor's own header through untouched, so behind one a stranger can claim any address: dodge the per-address lockout, or forge an address into the log. An Operator exposing the scanner that way sets `trusted_proxies = []`; `docs/operating.md` says so.
- **Carrier-NAT space counts as local.** `100.64.0.0/10` is where Tailscale numbers a tailnet, so the LAN door serves it the whole app over plain HTTP, as it does an RFC 1918 address. It is also ISP carrier-NAT space, where some ISPs route customers to one another; an Instance there should not expose `[server] port`. Kept deliberately, by the maintainer's call in review.
- **The LAN door trusts the address a connection arrives from**, so anything that rewrites sources to a private address makes a stranger look local. Rootless Docker and Docker Desktop do that to every published port; `docs/deploy.md` says to publish the plain port to the internet only on standard Docker on Linux.
- **Ports below 1024 need a capability on Linux.** `radio-scout service install` grants `CAP_NET_BIND_SERVICE` when the configuration it is installed with binds one, so turning TLS on means installing again — and a boot refused for it says so.

## Considered and rejected

- **The plain port unchanged, plus a third listener on :80.** Simplest to explain, and it leaves the whole app — admin login included — open over plain HTTP on 3000 on any VPS whose Operator did not think to firewall it. That is rdio-scanner's behaviour.
- **Every plain request redirected to HTTPS.** Breaks the recorder on the same Pi, and every LAN listener behind a router without hairpin NAT.
- **Requiring `accept_terms = true`.** Certbot's shape: one more line for every Operator, for an agreement naming a domain already expresses.
- **Built-in TLS deferred, or dropped, now that the tunnel is recommended.** Strands the rdio migrants using `ssl_auto_cert`, and leaves the Operator who will not route audio through Cloudflare with only a second program to run.
