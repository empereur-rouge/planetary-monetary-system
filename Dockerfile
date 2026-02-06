# ------------------------------------------------------------------------------
# 1. FRONTEND BUILDER
# ------------------------------------------------------------------------------
FROM node:20-alpine AS frontend-builder
WORKDIR /app
COPY pms-dashboard/package*.json ./
RUN npm install
COPY pms-dashboard/ .
RUN npm run build

# ------------------------------------------------------------------------------
# 2. RUST BUILDER: Naive build (skips cargo-chef caching to avoid stuck install)
# ------------------------------------------------------------------------------
FROM rustlang/rust:nightly AS builder
# Install build dependencies for RocksDB and Protobuf
RUN apt-get update && apt-get install -y \
    clang \
    llvm \
    libclang-dev \
    protobuf-compiler \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

# Build application directly
RUN cargo build --release -p bin -p tools-cli

# ------------------------------------------------------------------------------
# 3. RUNTIME: Minimal production image
# ------------------------------------------------------------------------------
# Debian 13 (Trixie) - has GLIBC 2.38+ required by nightly Rust
FROM debian:trixie-slim AS runtime

# Install runtime dependencies (OpenSSL, CA certs)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    openssl \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create dedicated user
RUN useradd -ms /bin/bash pms
USER pms
WORKDIR /home/pms

# Copy binaries
COPY --from=builder /app/target/release/bin /usr/local/bin/pms-node
COPY --from=builder /app/target/release/tools-cli /usr/local/bin/
COPY --from=frontend-builder /app/dist ./pms-dashboard/dist

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