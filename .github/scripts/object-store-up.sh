#!/usr/bin/env bash
#
# Bring up an S3-compatible object store for the real-S3 suite (#35, ADR-0002).
#
#   .github/scripts/object-store-up.sh minio|garage
#
# Why a step and a script, when the Postgres half of the same ADR is a plain
# `services:` block (#22): a GitHub service container cannot be given a command,
# and neither store serves without one. MinIO needs `server /data`; Garage needs
# a mounted config file, its single-node flags, and one `key allow` *after* it is
# already running. A step can do all of that; a `services:` entry can do none of
# it.
#
# Writes the four `TEST_S3_*` variables the harness reads (`tests/common/s3.rs`)
# into `$GITHUB_ENV`, so the step that runs the suite needs to know nothing about
# which store it was handed — and so `tests/ci.rs` can check that a job which
# provisions a store actually tells the suite about it.
set -euo pipefail

store="${1:-}"

# Pinned, because an object store that silently changes version under a hard
# gate turns an upstream release into a red build on an unrelated pull request.
MINIO_IMAGE='quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z'
GARAGE_IMAGE='dxflrs/garage:v2.3.0'

# Both stores are throwaways on the runner's loopback and die with the job, so
# these are fixed rather than generated: there is nothing here to protect, and
# one less moving part when a job goes red. The shapes are Garage's — `GK` plus
# 24 hex, and 64 hex — which MinIO is happy to accept as a root user and
# password, so one pair serves both.
ACCESS_KEY='GK00112233445566778899aabb'
SECRET_KEY='00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff'

# Wait for a health endpoint to answer **200**, specifically.
#
# Not "answers at all": Garage's admin `/health` binds early and replies 503
# until the single-node layout is applied, which is the moment the S3 API starts
# serving. Accepting any status would let the steps below race that — and the
# very next thing this script does is `key allow`, and the thing after that is a
# suite whose every test opens with `CreateBucket`. MinIO's liveness endpoint
# answers 200 too, so one rule covers both.
wait_for() {
  local url="$1" name="$2" i code
  for i in $(seq 1 60); do
    code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "$url" || true)"
    if [ "$code" = '200' ]; then
      echo "$name is up after ${i}s"
      return 0
    fi
    sleep 1
  done
  echo "$name never became healthy at $url (last status: ${code:-none})" >&2
  return 1
}

case "$store" in
  minio)
    docker run -d --name rs-minio -p 9000:9000 \
      -e "MINIO_ROOT_USER=$ACCESS_KEY" \
      -e "MINIO_ROOT_PASSWORD=$SECRET_KEY" \
      "$MINIO_IMAGE" server /data
    endpoint='http://127.0.0.1:9000'
    region='us-east-1'
    wait_for "$endpoint/minio/health/live" minio
    ;;

  garage)
    # `replication_factor = 1` and `--single-node` are the whole cluster: one
    # node, no replication, metadata and data on the container's own disk, all
    # of it discarded with the job.
    #
    # `s3_region` is deliberately *not* `us-east-1`. Garage checks the region a
    # request was signed for, so leaving it at Garage's own default is what makes
    # `TEST_S3_REGION` a knob the suite actually exercises rather than one that
    # happens to match the harness default.
    conf="$(mktemp -d)/garage.toml"
    cat >"$conf" <<EOF
metadata_dir = "/tmp/meta"
data_dir = "/tmp/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "[::]:3901"
rpc_public_addr = "127.0.0.1:3901"
rpc_secret = "$SECRET_KEY"

[s3_api]
s3_region = "garage"
api_bind_addr = "[::]:3900"
root_domain = ".s3.garage.localhost"

[admin]
api_bind_addr = "[::]:3903"
EOF
    docker run -d --name rs-garage -p 3900:3900 -p 3903:3903 \
      -v "$conf:/etc/garage.toml:ro" \
      -e "GARAGE_DEFAULT_ACCESS_KEY=$ACCESS_KEY" \
      -e "GARAGE_DEFAULT_SECRET_KEY=$SECRET_KEY" \
      "$GARAGE_IMAGE" /garage server --single-node --default-access-key
    endpoint='http://127.0.0.1:3900'
    region='garage'
    wait_for 'http://127.0.0.1:3903/health' garage

    # The default key is created without it, and every test here creates a
    # bucket of its own.
    docker exec rs-garage /garage key allow --create-bucket "$ACCESS_KEY"
    ;;

  *)
    echo "usage: ${0##*/} minio|garage" >&2
    exit 2
    ;;
esac

{
  echo "TEST_S3_ENDPOINT=$endpoint"
  echo "TEST_S3_REGION=$region"
  echo "TEST_S3_ACCESS_KEY_ID=$ACCESS_KEY"
  echo "TEST_S3_SECRET_ACCESS_KEY=$SECRET_KEY"
} >>"${GITHUB_ENV:?not running under GitHub Actions}"
