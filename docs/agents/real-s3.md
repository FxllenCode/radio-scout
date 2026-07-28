# Running the suite against a real S3 store

Radio-Scout stores audio behind an S3-compatible interface ([ADR-0002](../adr/0002-audio-object-storage.md)),
so [ADR-0009](../adr/0009-testing-strategy.md) requires the S3 backend to be tested against a store
that answers. CI does that on every pull request; this is how to do it on your machine when a change
touches `src/blob.rs`, the audio endpoint, orphan-GC, or anything that signs a request.

Sibling of [`dual-dialect.md`](dual-dialect.md), which is the same idea for the database half.

## The switch

**`TEST_S3_ENDPOINT`**, plus credentials. Set them and `tests/s3.rs` runs against the store; unset —
the everyday red-green loop, and any machine without one to hand — those tests **skip, saying so on
the run's output**. Nothing else in the suite is affected either way.

```bash
# A throwaway MinIO. Nothing in it is worth keeping.
docker run -d --name rs-minio -p 9000:9000 \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  quay.io/minio/minio:latest server /data

TEST_S3_ENDPOINT=http://127.0.0.1:9000 \
TEST_S3_ACCESS_KEY_ID=minioadmin \
TEST_S3_SECRET_ACCESS_KEY=minioadmin \
  cargo nextest run --test s3

docker rm -f rs-minio
```

`TEST_S3_REGION` is the fourth variable and defaults to `us-east-1`. MinIO accepts any region;
**Garage checks it**, so point it at whatever that store's `s3_region` is.

Setting `TEST_S3_ENDPOINT` without a credential is a **panic**, not a skip: a half-configured run
that quietly skipped would be the exact failure this suite exists to remove — a green run that
exercised nothing.

### Against Garage instead

Garage is ADR-0002's first-class recommendation, so CI runs the suite against both. The bring-up CI
uses works locally too, and is the shortest path to one:

```bash
GITHUB_ENV=/tmp/s3.env .github/scripts/object-store-up.sh garage
set -a; . /tmp/s3.env; set +a          # the four TEST_S3_* variables it wrote
cargo nextest run --test s3
docker rm -f rs-garage
```

It also takes `minio`, which is what the `Backend` job runs.

## Each test gets a bucket of its own

The harness creates `rs-test-<uuid>` per test and opens a `BlobStore` onto it (`tests/common/s3.rs`).
Isolation matters here for a sharper reason than it does for the database: `list_keys` and orphan-GC
see the **whole store**, so one shared bucket would put every concurrently running test's objects in
every other test's listing — and nextest runs tests in parallel, in separate processes.

`CreateBucket` is not something `object_store` offers, because it is not something the application
ever needs. The harness signs a **presigned `PUT` against the bucket root**, which is exactly that
request, using the same SigV4 code the production store signs with — which is why none of this costs
an S3 SDK in the dependency tree.

Those buckets are **not** emptied afterwards, for the reason the Postgres databases are not dropped:
`Drop` cannot await, and the store is a throwaway. That is also why the commands above end in
`docker rm -f`.

## Why this switch does not move the whole suite

`TEST_POSTGRES_URL` moves every `TestApp::spawn` onto Postgres. `TEST_S3_ENDPOINT` deliberately does
not do the equivalent: a database is one connection per app, but a bucket would put a network
round-trip behind every `put_object`, `stored` and `object_keys` in the project, and the everyday
loop's speed is the thing [#22](https://github.com/FxllenCode/radio-scout/issues/22) was careful to
protect. The S3 backend is small and self-contained enough that a suite of its own covers it.

## What the suite actually proves

`tests/blob.rs` already covers the S3 backend *offline*: SigV4 is computed locally, so it runs
everywhere and proves nothing about a round trip. `tests/s3.rs` is the other half — each of these
fails if the object never lands:

- **The object contract** — `put`/`size`/`get`/`get_range`/`delete`, and the absent-object answers,
  which on S3 are a `404` mapped to `None` rather than a local `ENOENT`.
- **An ingested Call lands in the bucket** — in at the recorder's own boundary, so what reaches the
  store is what the ingest handler decided to write.
- **The presigned redirect, followed** — `tests/serve_audio.rs` proves the app answers `307` with a
  signed-looking `Location`; only a store that answers can prove that URL is one a browser gets audio
  from. A signature the store rejects looks identical from the app's side, and iOS reports it as
  silence. The same URL is then **range-requested**, because with the S3 backend the range request
  never reaches Radio-Scout at all — the app's own range code is not what keeps iOS playing.
- **Orphan-GC over a real bucket** — listing what the server holds and judging each object by the
  server's own `Last-Modified`, which crosses the wire as RFC 1123 seconds.

## In CI

`.github/workflows/ci.yml` provisions both stores with `.github/scripts/object-store-up.sh`, which
writes the four `TEST_S3_*` variables into `$GITHUB_ENV`:

- the **`Backend`** job gets **MinIO**, so the real-S3 suite runs on both database dialects and feeds
  the one coverage profile;
- **`Object store on Garage`** is a job of its own running `--test s3`.

A script rather than a `services:` block — which is how Postgres is provisioned — because a GitHub
service container cannot be given a command, and neither store serves without one.

Both are hard gates. `tests/ci.rs` pins the traps they could otherwise fall into: a store stood up
whose endpoint never reaches the suite — or reaches it after the suite already ran — is a green run
of tests that all skipped.

The image tags in `object-store-up.sh` are **pinned**, so an upstream release cannot turn an
unrelated pull request red. The `docker run` above deliberately isn't: a store you `rm -f` five
minutes later has nothing to gain from a pin, and a version written down in two places drifts.
