# syntax=docker/dockerfile:1
FROM rust:1.97-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home hugh \
    && install -d -o hugh -g hugh /app/data
WORKDIR /app
COPY --from=builder /build/target/release/hugh /usr/local/bin/hugh
USER hugh
ENV HUGH_CONFIG=/app/config.toml
ENTRYPOINT ["hugh"]
