# Multi-stage build for Raft server
# Build stage: compile the binary with dependency caching
FROM rust:latest AS builder

WORKDIR /usr/src/raft_vibe

# Cache dependencies: copy manifests first, build a dummy to populate cache
COPY Cargo.toml Cargo.lock ./
COPY chaos-test/Cargo.toml chaos-test/Cargo.toml
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    mkdir -p src/bin && echo "fn main() {}" > src/bin/server.rs && \
    mkdir -p chaos-test/src && echo "" > chaos-test/src/lib.rs && \
    cargo build --release --bin raft-server 2>/dev/null || true && \
    rm -rf src chaos-test/src

# Now copy real source and build
COPY src src
COPY chaos-test/src chaos-test/src
RUN cargo build --release --bin raft-server

# Runtime stage: minimal image with networking tools
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    iproute2 \
    iptables \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/raft_vibe/target/release/raft-server /usr/local/bin/raft-server

ENTRYPOINT ["raft-server"]
