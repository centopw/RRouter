# RRouter — minimal self-hosted image.
#
# Two stages: build with the Rust toolchain, ship only the static-ish binary on a
# distroless runtime. No Node, no npm, no JS runtime, no build step at deploy time.

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
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
