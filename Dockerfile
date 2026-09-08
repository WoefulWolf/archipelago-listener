FROM rust:1-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- runtime stage ----
FROM debian:bookworm-slim

# ca-certificates: rustls-platform-verifier reads the system trust store.
# libssl3: runtime half of the libssl-dev the builder linked against.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/archipelago-listener /usr/local/bin/archipelago-listener

CMD ["archipelago-listener"]
