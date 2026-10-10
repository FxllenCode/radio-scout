#!/usr/bin/env bash
#
# Bring up a local ACME CA for the built-in TLS suite (#76, `tests/acme.rs`).
#
#   .github/scripts/acme-up.sh
#
# Two containers: **Pebble**, Let's Encrypt's own RFC 8555 test CA, and
# **pebble-challtestsrv** as nothing but its DNS — answering every name with the
# machine the suite runs on, so a test can ask for a certificate for
# `scanner.test` and Pebble's validator dials the Instance under test. Its own
# challenge responders are switched off: the Instance is what must answer.
#
# Pebble validates on fixed ports (TLS-ALPN-01 on 5001, HTTP-01 on 5002), on
# whatever address that DNS gives it. On a Linux runner both containers share
# the host's network and that address is loopback. Docker Desktop (a Mac,
# for a contributor running this locally) has no usable host network, so there
# they share a bridge, and the address is the one `host.docker.internal`
# resolves to — which Docker Desktop routes to the Mac's own loopback.
#
# Writes the three `TEST_ACME_*` variables the suite reads into `$GITHUB_ENV`
# when there is one, and prints them as `export` lines when there is not.
# `docs/agents/acme.md` is the local walkthrough.
set -euo pipefail

# Pinned, because a CA that silently changes behaviour under a gate turns an
# upstream release into a red build on an unrelated change.
PEBBLE_IMAGE='ghcr.io/letsencrypt/pebble:2.10.1'
CHALLTESTSRV_IMAGE='ghcr.io/letsencrypt/pebble-challtestsrv:2.10.1'

# Where the root that signs Pebble's own HTTPS is copied, for the suite to
# trust: `TEST_ACME_DIRECTORY_ROOT`.
out="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/radio-scout-acme"
mkdir -p "$out"

# Pebble's knobs for a test run: validate at once rather than after a random
# sleep of up to fifteen seconds, never reject a nonce on purpose, and always
# reuse an authorization the account already holds as valid, where Pebble's
# default is a coin toss. The first two are realism for client authors and noise
# for a suite asserting on outcomes; the third is what Let's Encrypt does, made
# deterministic so the path a client takes through a reused one is always taken.
pebble_env=(-e PEBBLE_VA_NOSLEEP=1 -e PEBBLE_WFE_NONCEREJECT=0 -e PEBBLE_AUTHZREUSE=100)
chall_flags=(-defaultIPv6 '' -http01 '' -https01 '' -tlsalpn01 '' -doh '' -dnsserver ':8053' -management ':8055')

case "$(uname -s)" in
  Linux)
    docker run -d --name rs-acme-dns --network host "$CHALLTESTSRV_IMAGE" \
      -defaultIPv4 127.0.0.1 "${chall_flags[@]}"
    docker run -d --name rs-pebble --network host "${pebble_env[@]}" "$PEBBLE_IMAGE" \
      -dnsserver 127.0.0.1:8053
    ;;
  *)
    host_ip="$(docker run --rm alpine:3 getent hosts host.docker.internal | awk '{print $1}')"
    docker network create rs-acme >/dev/null
    docker run -d --name rs-acme-dns --network rs-acme "$CHALLTESTSRV_IMAGE" \
      -defaultIPv4 "$host_ip" "${chall_flags[@]}"
    docker run -d --name rs-pebble --network rs-acme -p 14000:14000 -p 15000:15000 \
      "${pebble_env[@]}" "$PEBBLE_IMAGE" -dnsserver rs-acme-dns:8053
    ;;
esac

docker cp rs-pebble:/test/certs/pebble.minica.pem "$out/pebble.minica.pem"

# Wait for the directory to answer — Pebble binds within a second, but a suite
# that starts first fails every test on its very first request.
for i in $(seq 1 30); do
  if curl -sf --cacert "$out/pebble.minica.pem" https://localhost:14000/dir >/dev/null; then
    echo "pebble is up after ${i}s"
    break
  fi
  if [ "$i" = 30 ]; then
    echo "pebble never answered at https://localhost:14000/dir" >&2
    docker logs rs-pebble >&2 || true
    exit 1
  fi
  sleep 1
done

vars=(
  "TEST_ACME_DIRECTORY=https://localhost:14000/dir"
  "TEST_ACME_DIRECTORY_ROOT=$out/pebble.minica.pem"
  "TEST_ACME_ISSUED_ROOTS=https://localhost:15000/roots/0"
)
if [ -n "${GITHUB_ENV:-}" ]; then
  printf '%s\n' "${vars[@]}" >>"$GITHUB_ENV"
else
  printf 'export %s\n' "${vars[@]}"
fi
