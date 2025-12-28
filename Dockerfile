# syntax=docker/dockerfile:1
# ------------------------------------------------------------------------------
# 1. PLANNER: Compute dependency recipe
# ------------------------------------------------------------------------------
# Le tag specific 'latest-rust-nightly' n'existe peut-être pas. On part de l'officiel.
FROM rustlang/rust:nightly AS chef
# Install build dependencies for RocksDB and Protobuf
RUN apt-get update && apt-get install -y \
    clang \
    llvm \
    libclang-dev \
    protobuf-compiler \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# On installe cargo-chef à la main
RUN cargo install cargo-chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ------------------------------------------------------------------------------
# 2. BUILDER: Cache dependencies & Build binary
# ------------------------------------------------------------------------------
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching layer!
RUN cargo chef cook --release --recipe-path recipe.json

# Build application
COPY . .
# On build uniquement le binaire serveur et CLI
RUN cargo build --release -p bin -p tools-cli

# ------------------------------------------------------------------------------
# 3. RUNTIME: Minimal production image
# ------------------------------------------------------------------------------
# On utilise testing-slim pour avoir une GLIBC récente (compatible avec l'image rust:nightly)
FROM debian:testing-slim AS runtime

# Install runtime dependencies (OpenSSL, CA certs)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    openssl \
    && rm -rf /var/lib/apt/lists/*

# Create dedicated user
RUN useradd -ms /bin/bash pms
USER pms
WORKDIR /home/pms

# Copy binaries
# Le package s'appelle "bin", donc le binaire est "bin"
COPY --from=builder /app/target/release/bin /usr/local/bin/pms-node
COPY --from=builder /app/target/release/tools-cli /usr/local/bin/

# Default directories for persistence
RUN mkdir -p /home/pms/data /home/pms/config /home/pms/tls

# Ports: P2P (8050), API (8080)
EXPOSE 8050 8080

# Healthcheck
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD /usr/local/bin/pms-node --version || exit 1

# Default Entrypoint
ENTRYPOINT ["pms-node"]
CMD ["--help"]