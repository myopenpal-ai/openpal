# openpal-relay — Phase 1 sync relay skeleton (docs/design/MULTI_DEVICE_SYNC.md).
#
# This is a dev/self-host deploy skeleton, not a hardened production image:
# no TLS termination, no auth in front of the process (see main.rs's module
# doc). Build from the repo root so the workspace Cargo.toml/Cargo.lock and
# the crates/sync-protocol path dependency are in the build context:
#
#   docker build -f crates/relay/Dockerfile -t openpal-relay .
#   docker compose build   (from the repo root, uses ../../docker-compose.yml)

# ---- builder ---------------------------------------------------------------
# "1-bookworm" tracks the latest stable 1.x toolchain rather than pinning an
# exact point release, since the resolved Cargo.lock (tokio 1.52, axum 0.8,
# hyper 1.10, …) is recent enough that an older pinned Rust could fall below
# some dependency's MSRV.
FROM rust:1-bookworm AS builder
WORKDIR /build

# Workspace manifests first so dependency compilation is cached across
# source-only edits to the two crates that actually change.
COPY Cargo.toml Cargo.lock ./
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/app/Cargo.toml crates/app/Cargo.toml
COPY bundler/Cargo.toml bundler/Cargo.toml
COPY crates/sync-protocol/Cargo.toml crates/sync-protocol/Cargo.toml
COPY crates/relay/Cargo.toml crates/relay/Cargo.toml

# Workspace resolution needs every member's manifest *and* a target file to
# discover (src/lib.rs or src/main.rs) even for members we never compile —
# stub the ones we don't build (core/app/bundler) so `cargo build -p
# openpal-relay` doesn't need their real (much heavier) source trees in the
# build context at all.
RUN mkdir -p crates/core/src && echo "" > crates/core/src/lib.rs \
 && mkdir -p crates/app/src && echo "fn main() {}" > crates/app/src/main.rs && echo "" > crates/app/src/lib.rs \
 && mkdir -p bundler/src && echo "fn main() {}" > bundler/src/main.rs

# Now the real sources for the two crates this image actually builds.
COPY crates/sync-protocol/src crates/sync-protocol/src
COPY crates/relay/src crates/relay/src

RUN cargo build --release -p openpal-relay

# ---- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home --home-dir /home/relay relay
WORKDIR /home/relay

COPY --from=builder /build/target/release/openpal-relay /usr/local/bin/openpal-relay

# SQLite mailbox file lives here; mount a named volume at this path so data
# survives container restarts (see docker-compose.yml).
ENV RELAY_DB_PATH=/home/relay/data/relay.sqlite
ENV PORT=8787
RUN mkdir -p /home/relay/data && chown -R relay:relay /home/relay
USER relay

EXPOSE 8787
# `openpal-relay --healthcheck` is an in-process probe (main.rs), not curl/
# wget — this runtime image ships neither, and adding one just for a health
# probe would mean new general-purpose-HTTP-client attack surface for a
# narrow purpose. Exec form (no shell) to match the CMD below.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD ["openpal-relay", "--healthcheck"]
CMD ["openpal-relay"]
