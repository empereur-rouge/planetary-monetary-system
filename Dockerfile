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
# 2. RUST BUILDER: Dependency caching via dummy crates
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

# --- Step A: Copy only manifests + lockfile (rarely changes) ---
COPY Cargo.toml Cargo.lock ./
COPY bin/Cargo.toml bin/Cargo.toml
COPY crates/pms-wallet/Cargo.toml crates/pms-wallet/Cargo.toml
COPY crates/pms-core/Cargo.toml crates/pms-core/Cargo.toml
COPY crates/pms-server/Cargo.toml crates/pms-server/Cargo.toml
COPY crates/pms-storage/Cargo.toml crates/pms-storage/Cargo.toml
COPY crates/pms-utils/Cargo.toml crates/pms-utils/Cargo.toml
COPY crates/pms-token/Cargo.toml crates/pms-token/Cargo.toml
COPY crates/pms-types-transaction/Cargo.toml crates/pms-types-transaction/Cargo.toml
COPY crates/pms-types-mint/Cargo.toml crates/pms-types-mint/Cargo.toml
COPY crates/pms-types-payload/Cargo.toml crates/pms-types-payload/Cargo.toml
COPY crates/pms-types/Cargo.toml crates/pms-types/Cargo.toml
COPY crates/pms-types-block/Cargo.toml crates/pms-types-block/Cargo.toml
COPY crates/pms-network/Cargo.toml crates/pms-network/Cargo.toml
COPY crates/pms-interface/Cargo.toml crates/pms-interface/Cargo.toml
COPY crates/pms-wire/Cargo.toml crates/pms-wire/Cargo.toml
COPY crates/pms-crypto/Cargo.toml crates/pms-crypto/Cargo.toml
COPY crates/tools-cli/Cargo.toml crates/tools-cli/Cargo.toml
COPY crates/pms-config/Cargo.toml crates/pms-config/Cargo.toml
COPY crates/pms-testkit/Cargo.toml crates/pms-testkit/Cargo.toml
COPY crates/pms-types-dag/Cargo.toml crates/pms-types-dag/Cargo.toml
COPY crates/pms-errors/Cargo.toml crates/pms-errors/Cargo.toml
COPY crates/pms-ledger/Cargo.toml crates/pms-ledger/Cargo.toml
COPY crates/pms-consensus/Cargo.toml crates/pms-consensus/Cargo.toml
COPY crates/pms-event/Cargo.toml crates/pms-event/Cargo.toml
COPY crates/pms-types-nft/Cargo.toml crates/pms-types-nft/Cargo.toml
COPY crates/pms-gateway/Cargo.toml crates/pms-gateway/Cargo.toml
COPY crates/pms-bridge/Cargo.toml crates/pms-bridge/Cargo.toml

# --- Step B: Create dummy source files so cargo resolves the dependency graph ---
RUN mkdir -p bin/src && echo 'fn main() {}' > bin/src/main.rs \
    && for crate in \
        pms-wallet pms-core pms-server pms-storage pms-utils pms-token \
        pms-types-transaction pms-types-mint pms-types-payload pms-types \
        pms-types-block pms-network pms-interface pms-wire pms-crypto \
        tools-cli pms-config pms-testkit pms-types-dag pms-errors \
        pms-ledger pms-consensus pms-event pms-types-nft pms-bridge; do \
        mkdir -p "crates/$crate/src" && echo '' > "crates/$crate/src/lib.rs"; \
    done \
    && mkdir -p crates/pms-gateway/src && echo 'fn main() {}' > crates/pms-gateway/src/main.rs \
    && mkdir -p crates/tools-cli/src && echo 'fn main() {}' > crates/tools-cli/src/main.rs

# --- Step C: Build dependencies only (cached as long as Cargo.toml/lock don't change) ---
RUN cargo build --release -p bin -p tools-cli 2>/dev/null || true

# --- Step D: Copy real source code (only this layer invalidates on code changes) ---
COPY . .
# Touch all source files so cargo knows they changed vs the dummies
RUN find bin/src crates/*/src -name '*.rs' -exec touch {} +

# --- Step E: Build final binaries (only recompiles our crate code, deps are cached) ---
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
CMD ["--config", "/home/pms/config/config.toml"]
