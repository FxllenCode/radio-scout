# Running the suite against an ACME CA

Built-in TLS gets its certificate from an ACME CA ([ADR-0022](../adr/0022-making-an-instance-public.md)),
and the half of that worth proving is the CA reaching *back* into the Instance — over TLS-ALPN-01
on the HTTPS port, or HTTP-01 through the **LAN door** — and liking what it finds. A harness cannot
fake that honestly, so `tests/acme.rs` runs against **Pebble**, Let's Encrypt's own test CA. CI does
it in `Backend` and in `ACME on Pebble`; this is how to do it on your machine when a change touches
`src/tls/`.

Sibling of [`real-s3.md`](real-s3.md), which is the same idea for the object store.

## The switch

**`TEST_ACME_DIRECTORY`**, plus two more. Set them and `tests/acme.rs` runs against the CA; unset —
the everyday loop — every test there **skips, saying so on the run's output**. Nothing else in the
suite is affected: everything about built-in TLS that needs no CA (the listener, the LAN door, the
`Secure` cookie, an Operator's own files, the status card) is `tests/tls.rs`, which always runs,
against a test CA rcgen makes on the spot.

| Variable | What it is |
| --- | --- |
| `TEST_ACME_DIRECTORY` | Pebble's directory, `https://localhost:14000/dir` |
| `TEST_ACME_DIRECTORY_ROOT` | the root that signs Pebble's *own* HTTPS — what `[tls] directory_root` is for |
| `TEST_ACME_ISSUED_ROOTS` | where Pebble serves the root it *issues* under, which it makes afresh on every start |

Setting the directory without the other two is a **panic**, not a skip — `tests/common/s3.rs`'s rule:
a half-configured run that quietly skipped would be a green run that issued nothing.

## Bringing one up

The bring-up CI uses works locally too:

```bash
.github/scripts/acme-up.sh > /tmp/acme.env      # prints three `export` lines
source /tmp/acme.env
cargo nextest run --test acme
docker rm -fv rs-pebble rs-acme-dns; docker network rm rs-acme 2>/dev/null
```

Two containers: Pebble, and `pebble-challtestsrv` as nothing but its DNS, answering **every** name
with the machine the suite runs on — so a test asks for `scanner.test` and Pebble's validator dials
the Instance under test. On Linux both share the host's network and that machine is `127.0.0.1`.
Docker Desktop has no usable host network, so on a Mac they share a bridge and the answer is
`host.docker.internal`'s address, which Docker Desktop routes back to the Mac's loopback.

## Why it runs one test at a time

Pebble validates on **fixed ports** — TLS-ALPN-01 on 5001, HTTP-01 on 5002 — so two tests at once
would be two Instances fighting for one port. `.config/nextest.toml` puts the `acme` binary in a
test group of one; every other binary keeps its parallelism. Each test proves one challenge by
leaving the *other* challenge's port ephemeral, where Pebble cannot find it — that is how "HTTP-01
is the fallback" becomes an outcome rather than a log line.

## What Pebble is told

`PEBBLE_VA_NOSLEEP=1` (validate at once rather than after up to fifteen seconds),
`PEBBLE_WFE_NONCEREJECT=0` (never reject a nonce on purpose) and `PEBBLE_AUTHZREUSE=100` (always reuse
an authorization the account already holds as valid, where Pebble's default is a coin toss — Let's
Encrypt reuses them too, and a suite cannot assert on a coin). The first two are realism for client
authors and noise for a suite asserting outcomes. Pebble issues 90-day certificates and answers ARI, so the
renewal test moves the Instance's clock 89 days and settles — it never sleeps.

**Pebble refuses a request without a `User-Agent`** (RFC 8555 §6.1 requires one). That is how the
terms-of-service lookup was found sending none; a real CA is entitled to the same refusal.
