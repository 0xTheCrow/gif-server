# Stage 1: generate dependency recipe for caching
FROM rust:1.94-slim-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 2: build dependencies (cached layer — only reruns if Cargo.toml/lock changes)
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Stage 3: build the application binary
COPY . .
RUN cargo build --release --bin gif-server

# Stage 4: minimal runtime image
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl3 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
RUN mkdir -p storage
COPY --from=builder /app/target/release/gif-server /usr/local/bin/gif-server
EXPOSE 8847
CMD ["gif-server"]
