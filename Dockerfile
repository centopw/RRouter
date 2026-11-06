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

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
# crates/rrouter-ui/build.rs embeds ../../ui/dist at compile time.
COPY --from=ui /ui/dist ./ui/dist
# Cargo registry and target live in cache mounts: the multi-arch CI build compiles the
# same dependencies twice per platform, and pulling them from cache is the difference
# between a 3-minute and a 12-minute image.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    apt-get update && apt-get install -y --no-install-recommends pkg-config \
 && cargo build --release --locked \
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
