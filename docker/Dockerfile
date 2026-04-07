FROM rust:1.84-bookworm AS builder

RUN apt-get update && apt-get install -y \
    libssl-dev \
    pkg-config \
    perl \
    cmake \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies
COPY Cargo.toml Cargo.lock* rust-toolchain.toml ./
COPY crates/cdn-common/Cargo.toml crates/cdn-common/Cargo.toml
COPY crates/cdn-config/Cargo.toml crates/cdn-config/Cargo.toml
COPY crates/cdn-cache/Cargo.toml crates/cdn-cache/Cargo.toml
COPY crates/cdn-middleware/Cargo.toml crates/cdn-middleware/Cargo.toml
COPY crates/cdn-proxy/Cargo.toml crates/cdn-proxy/Cargo.toml

# Create dummy source files for dependency caching
RUN mkdir -p crates/cdn-common/src && echo "pub mod error; pub use error::{CdnError, CdnResult};" > crates/cdn-common/src/lib.rs && echo "use thiserror::Error; #[derive(Error, Debug)] pub enum CdnError { #[error(\"x\")] Config(String) } pub type CdnResult<T> = Result<T, CdnError>;" > crates/cdn-common/src/error.rs \
    && mkdir -p crates/cdn-config/src && echo "" > crates/cdn-config/src/lib.rs \
    && mkdir -p crates/cdn-cache/src && echo "" > crates/cdn-cache/src/lib.rs \
    && mkdir -p crates/cdn-middleware/src && echo "" > crates/cdn-middleware/src/lib.rs \
    && mkdir -p crates/cdn-proxy/src && echo "fn main() {}" > crates/cdn-proxy/src/main.rs

RUN cargo build --release 2>/dev/null || true

# Copy real source code
COPY crates/ crates/
COPY config/ config/

# Touch source files to invalidate cache
RUN find crates -name "*.rs" -exec touch {} +

RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    libssl3 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/cdn-proxy /app/cdn-proxy
COPY config/ /app/config/

EXPOSE 80 443 8080 8443 9090

ENTRYPOINT ["/app/cdn-proxy"]
CMD ["-c", "/app/config/default.yaml"]
