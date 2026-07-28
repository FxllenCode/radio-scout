# The published image (ticket #23, ADR-0007).
#
# This is a **packaging** Dockerfile, not a from-source one: it assembles an
# image around binaries `.github/workflows/release.yml` has already built. That
# is deliberate. Building the Rust tree for arm64 under QEMU takes the better
# part of an hour, and cross-compiling it inside a builder stage would mean the
# image shipped a binary built a different way from the one in the release —
# two artifacts, one of them untested. Here they are the same file.
#
# To build from source instead, there is no image to learn: `cargo build
# --release` (see docs/deploy.md).
#
# The result is `FROM scratch` because the Linux binaries are statically linked
# musl: there is no libc to bring, no shell, no package manager, and nothing in
# the image that is not the scanner, its CA certificates and its data directory.

FROM alpine:3 AS prep
RUN apk add --no-cache ca-certificates && \
    mkdir -p /out/data && chown 65532:65532 /out/data && \
    mkdir -m 1777 -p /out/tmp

FROM scratch

# Docker's spelling of the architecture, filled in per platform of the manifest.
ARG TARGETARCH

# Outbound TLS — Web Push (#16) and S3 (ADR-0002) — verifies against the system
# trust store, which on a scratch image is exactly this file and nothing else.
COPY --from=prep /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=prep --chown=65532:65532 /out/data /data
COPY --from=prep /out/tmp /tmp
COPY --chmod=0755 dist/radio-scout-${TARGETARCH} /radio-scout

# Not root. Numeric because there is no /etc/passwd to hold a name. A *named*
# volume inherits this ownership when Docker creates it; a bind mount does not,
# so a host directory has to be chowned to 65532 (docs/deploy.md says so).
USER 65532:65532

# The base directory as configuration rather than as a baked-in argument
# (ADR-0012): `docker run … --port 8080` then appends to the command line
# instead of replacing the one thing the image had to say.
ENV RADIO_SCOUT_BASE_DIR=/data

EXPOSE 3000
VOLUME ["/data"]
ENTRYPOINT ["/radio-scout"]
