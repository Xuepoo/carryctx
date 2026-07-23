# syntax=docker/dockerfile:1
FROM rust:1.96-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src/ src/
COPY migrations/ migrations/

RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:latest

COPY --from=builder /build/target/release/carryctx /usr/local/bin/carryctx

ENTRYPOINT ["carryctx"]
CMD ["--help"]
