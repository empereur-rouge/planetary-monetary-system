# Changelog

All notable changes to the PMS DAG will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **NFT Burn-to-Mint & Refund**
  - Implemented `burn_refund` logic: "Cube" NFTs trigger refund `(weight * size * density) / 100`
  - Added signature verification for NFT attributes using `authority_public_key`
  - Extended `NftMetadata` with `extra` field for generic attributes
  - Renamed "Game Authority" to "Authority" in config and code

- **NFT Management & Queries**
  - **Retrieve by Owner**: New API `/v1/wallet/:address/nfts`
  - Added `get_by_owner` to `pms-storage` with dual-write to `nfts` and `nfts_by_owner` column families
  - Updated SDK with `getNfts(address)` method

- **Minting Security & SDK**
  - **Coordinator-Only Minting**: Restricted `Mint` action to Coordinator public key
  - **Encrypted Minting**: Added `mintCube` to SDK with payload encryption for Owner + Coordinator
  - **SDK Enhancements**: Added `encryptNftPayload` supporting multiple recipients, and `mintEncryptedNft`

- **Docker & Config**
  - Exposed Docker backend ports for local network access
  - Updated `tools-cli` to derive and update Coordinator X25519 keys automatically
- **Treasury Fee Distribution**
  - Reward blocks automatically created after each transaction
  - Fee split: 15% Treasury, 45% Creator, 40% Parents
  - Block reward split: 70% Creator, 20% Treasury, 10% Burn
  - New `/v1/balance` API endpoint for address-only balance queries

- **Encrypted Reward Blocks** (Privacy Enhancement)
  - Each reward output encrypted individually for [recipient, coordinator]
  - Uses `PlainPayload::EncryptedReward` with `EncryptedRewardOutput`
  - X25519 key extracted from bech32 address for encryption
  - Only recipient and coordinator can see transaction details
  - Coordinator manually creates UTXOs after block creation

- **Balance Query API**
  - `balance_by_address()` method in `ShardedUtxoSet`
  - `NetDagAdapter::balance_by_address()` and `add_utxo()` trait methods
  - Works without private keys (useful for treasury audits)

### Tests
- `fee_distribution_e2e_test.rs` - E2E verification of treasury fee distribution

### Fixed
- **Docker Deployment**
  - Reverted healthcheck to HTTPS (`-k`) as server enforces TLS in production
  - Corrected service name (`node` -> `node1`) to match Caddy reverse proxy configuration
  - Updated `scripts/deploy.sh` to generate Caddyfile with correct backend host (`node1`) and configured proper HTTPS transport (skip verify) for internal traffic
  - **Manual Sync**: Automatic `docker-compose.yml` sync removed from `deploy.sh`. Use `git push` before deploying (warning added).
  - **Automated Treasury Details**:
    - Relocated generation to **Pre-Start** phase (between Build and Up) using `docker compose run --rm`.
    - Solves "egg/chicken" crash where node needed treasury file to start, but file generation needed running node.
    - Moved file path to `./etc/pms/treasury-wallets.json` ensures persistence.
    - Added explicit permission fix (`chmod 777 etc/pms`, `chmod 777 etc/config`) to ensure container can write generated keys.
    - Updated `tools-cli` with `--force` flag to bypass interactive confirmation during automated deployment.
    - **Persistence Fix**: Mounted `etc/config` as directory instead of file to solve Docker atomic write/inode issues on host.
  - **Fail Fast**: Node process now exits (crashes) if API fails to start (e.g. missing treasury config), preventing "zombie" states where only P2P runs
  - **Checklist**: Added visual file status table to `deploy.sh`. Runs AFTER setup and STOPS deployment if critical files are missing. Correctly handles files to be generated.

---

## [0.3.0] - 2026-01-08

### Added
- **Mainnet Configuration**
  - Network mode `mainnet` with `pms:main` prefix
  - Strict TLS validation (`allow_insecure_tls = false`)
  - Production-ready validation rules

- **IOTA-style Genesis Bootstrap**
  - Genesis can be used as supplementary parent during DAG bootstrap
  - Maintains `min_parents = 2` while allowing network to start
  - Automatic genesis parent injection in `/wallet/tx/send` API

- **Security Enhancements**
  - Renamed `PMS_ADMIN_TOKEN_DEV` → `PMS_ADMIN_TOKEN`
  - Added CORS layer to API (permissive for SDK development)
  - Fixed validation: removed incorrect `api_addr` HTTPS check (bind address ≠ URL)

### Performance
- **🚀 4028 TPS** (mainnet mode, Docker benchmark)
  - 10 workers × 1000 tx = 10,000 transactions
  - Duration: 2.48s
  - 0 failures

### Changed
- Parent validation now considers available tips count
- API automatically adds genesis as 2nd parent when < 2 tips available

---

## [Unreleased - Previous]

### Added
- **Phase 1: Parallel Benchmark** (IOTA-like optimization)
  - 10 parallel workers with independent UTXO chains
  - HTTP connection pooling (`pool_max_idle_per_host`)
  - Detailed comments explaining ownership, async/await, and concurrency

- **Phase 2: Server Optimizations**
  - `parents_exist_in_store()` - validates parents against RocksDB instead of RAM
  - Fixed race condition where tips selected from RocksDB were validated against RAM
  - Removed redundant RAM-based parent check in `validate_block()`

- **Phase 3: Lock-Free DAG (IOTA-like architecture)**
  - Added `dashmap = "6.1"` dependency for concurrent HashMap
  - Created `ConcurrentDag` module with `DashMap<BlockId, Block>` for lock-free storage
  - Added `DashSet` for concurrent spent outpoint tracking
  - Bypassed locked `validate_block()` that was causing 27-280ms latency per block
  - Parents validated via `parents_exist_in_store()` (lock-free)
  - Double-spend checked via `ShardedUtxoSet` (lock-free)

### Performance
- **🚀 Phase 3 Result: 2437 TPS** (50x improvement!)
  - Before: 48 TPS (DAG lock contention)
  - After: 2437 TPS (lock-free)
  - Duration for 10k tx: 220s → **4.1s**
  - DAG insert time: 20ms → **2-3µs**

### Planned
- Phase 4: Background persistence (RocksDB writes async)
- Phase 5: P2P gossip optimization

---

## [0.2.0] - 2025-12-31

### Added
- **Benchmark infrastructure**
  - `docker-compose.bench.yml` for single-node benchmark
  - `docker_bench_single.rs` test (10,000 tx stress test)
  - `benchmark_local.sh` script with `--docker` mode
  - Tracing instrumentation (`pms_bench` target) in `server.rs`

### Performance
- Baseline TPS: **22.35 tx/sec** (single node, 1 tx/bloc with PoW `0000`)

---

## [0.1.0] - 2025-12-27

### Added
- **Core DAG**
  - Block structure with parents, payload, nonce, signature
  - UTXO ledger with atomic updates
  - Tips selection algorithm
  - Orphan block handling with parent dependency tracking

- **P2P Network**
  - TLS mutual authentication
  - Gossip protocol for block propagation
  - `GetTips`, `GetBlock`, `Inv`, `Blocks` messages
  - Rate limiting and anti-flood protection
  - Parallel parent fetching for orphans

- **Storage**
  - RocksDB persistence with column families
  - Background maintenance (flush, compaction)
  - Crash recovery support

- **API**
  - REST endpoints: `/submit/block`, `/wallet/tx/send`, `/wallet/balance`
  - Health checks: `/live`, `/ready`, `/healthz`
  - Metrics endpoint: `/metrics`
  - Admin routes with token authentication

- **Wallet**
  - Ed25519 + X25519 keypair generation
  - Bech32 address encoding
  - Transaction signing
  - UTXO scanning and balance calculation

- **Security**
  - Payload encryption (X25519 + ChaCha20-Poly1305)
  - Block signature verification
  - IP-based rate limiting
  - Proof-of-Work validation

### Infrastructure
- Docker multi-node setup (3 nodes + Caddy)
- TLS certificate generation scripts
- Configuration management (TOML)

---

## Version History

| Version | Date | Highlights |
|---------|------|------------|
| 0.2.0 | 2025-12-31 | Benchmark infrastructure |
| 0.1.0 | 2025-12-27 | Initial DAG implementation |
