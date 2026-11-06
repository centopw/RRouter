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
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
 && cargo build --release --locked \
 && strip target/release/rrouter

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/rrouter /usr/local/bin/rrouter
# Credentials DB, key file, and any operator-supplied /ext assets live here.
VOLUME ["/data"]
ENV RROUTER_DATA_DIR=/data \
    RROUTER_HOST=0.0.0.0 \
    RROUTER_PORT=20128
EXPOSE 20128
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/rrouter"]
