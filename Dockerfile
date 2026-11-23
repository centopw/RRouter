# syntax=docker/dockerfile:1

# RRouter — self-hosted image.
#
# Three stages: Node builds the dashboard, Rust builds the binary with that dist
# embedded, distroless runs it. The runtime image has no package manager, no shell,
# and no Node — just the static binary.

FROM node:22-alpine AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

# Dependency compilation is its own layer (cargo-chef). Docker's `--mount=type=cache` is
# not exported by buildx's GHA cache backend, so a mount-based cache is cold on every CI
# runner; a layer is exported, so a change to application code only recompiles the
# workspace crates while `Cargo.lock` is unchanged.
FROM rust:1-slim-bookworm AS chef
WORKDIR /src
# cmake + make are for aws-lc-sys, the native backend behind `jsonwebtoken`'s
# `aws_lc_rs` feature (HS256 session tokens); the image's gcc does the C compile.
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config cmake make \
 && rm -rf /var/lib/apt/lists/* \
 && cargo install cargo-chef --locked --version 0.1.78

FROM chef AS planner
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
# Cached until a dependency or manifest changes.
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
# crates/rrouter-ui/build.rs embeds ../../ui/dist at compile time.
COPY --from=ui /ui/dist ./ui/dist
RUN cargo build --release --locked \
 && cp target/release/rrouter /rrouter \
 && strip /rrouter

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /rrouter /usr/local/bin/rrouter
# Credentials DB, key file, and any operator-supplied /ext assets live here. Creating the
# directory with the runtime user's ownership makes a fresh named volume writable: Docker
# seeds a new volume from the image path, including its owner.
COPY --chown=nonroot:nonroot --from=ui /ui/package.json /data/.keep
VOLUME ["/data"]
ENV RROUTER_DATA_DIR=/data \
    RROUTER_HOST=0.0.0.0 \
    RROUTER_PORT=20128
EXPOSE 20128
USER nonroot:nonroot
# The binary probes itself: the runtime image has no shell, curl or wget.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/rrouter", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/rrouter"]
