# Changelog

## [Unreleased]
- **feat(server)**: Added API key authentication middleware for public routes. Keys are SHA-256 hashed, validated with constant-time comparison, and support granular per-group/per-endpoint scopes (`wallet`, `nft`, `dag`, `supply`, `tokens`, `history`, `coordinator`, `*`).
- **feat(server)**: Added admin CRUD endpoints for API key management (`POST /admin/api-keys`, `GET /admin/api-keys`, `DELETE /admin/api-keys/{id}`).
- **feat(config)**: Added `api_keys_file` field to `[auth]` config section for specifying the JSON key store path. BREAKING: Auth struct has a new field (uses `#[serde(default)]`).
- **feat(server)**: New `api_keys` module with `ApiKeyStore` (JSON file persistence, hot-reload support, atomic file writes).
- **ops(deploy)**: Integrated `api_keys_file` into all deployment configs (`config.prod.template.toml`, `config.prod.toml`, `config.testnet.toml`, `config.docker-test.toml`) and scripts (`deploy.sh`, `deploy-testnet.sh`, `docker_test.sh`). Deployment scripts now automatically create a default SDK API key (with wildcard scope) after the gateway is healthy and include it in the secure credentials backup JSON.
- **feat(sdk)**: Extracted TypeScript SDK to standalone repo `pms-sdk/` and published as `@empereur-rouge/pms-sdk@0.1.0` on npm. Installable via `npm install @empereur-rouge/pms-sdk`.
- **feat(sdk)**: Added mandatory API key authentication (`apiKey` field in `PmsClientConfig`). All HTTP requests now include `X-API-Key` header. BREAKING: `apiKey` is required — existing code must add it.
- **fix(utxo)**: Fixed bug where encrypted transactions didn't update UTXO cache. Added `remove_utxo()` method to `NetDagAdapter` trait and implemented manual UTXO delta application in `wallet_send_tx` for encrypted payloads.
- **feat(interface)**: Added `remove_utxo()` and `get_utxo()` methods to `NetDagAdapter` trait for complete UTXO cache management.
- **test(server)**: Added `encrypted_utxo_delta_test.rs` with 2 regression tests verifying encrypted transactions correctly consume inputs and create outputs in UTXO cache.
- **fix(token)**: Fixed fee precision bug - fees could exceed 8 decimals (e.g., `0.00106478824`). Enriched `Amount` type with arithmetic operators (`Add`, `Sub`, `Mul`, `Div`) that auto-round to 8 decimals after each operation.
- **refactor(token)**: BREAKING: Simplified `FeePolicy::new()` signature from 3 args to 2 args (removed `precision` parameter, now uses `Amount::DECIMALS` constant). `compute_fee()` now returns `Amount` instead of `String`.
- **test(server)**: Added `fee_consistency_test.rs` with 2 tests verifying that `/v1/tx/prepare` and `/wallet/tx/send` use identical fee calculation logic.
- **fix(api)**: Fixed fee calculation mismatch in `wallet_send_tx` - sender address now compared directly instead of H20 derivation, ensuring change outputs are correctly excluded from taxable amount.
- **refactor(config)**: Simplified RuntimeConfig for Single Writer mode (BREAKING):
  - Replaced `platform_fee_bps` → `coordinator_fee_bps` (default: 6700 = 67%)
  - Replaced `node_fee_bps` → `treasury_fee_bps` (default: 3300 = 33%)
  - Replaced `SetPlatformFee`/`SetNodeFee` → `SetCoordinatorFee`/`SetTreasuryFee` in ConfigUpdate
  - Added `validate_fee_split()` to ensure coordinator + treasury = 100%
  - Updated tests, API docs, and all usages
- **feat(api)**: Added Admin Config API for runtime configuration changes:
  - `GET /admin/config` - Retrieve current RuntimeConfig
  - `POST /admin/config` - Modify RuntimeConfig parameters (fee_rate_bps, base_fee, coordinator_fee_bps, treasury_fee_bps, min_pow_bits, max_mint_per_block, mint_enabled)
  - Protected by `require_local_or_admin` middleware (requires admin token)
  - Changes persisted to RocksDB with full audit history
- **feat**: Single Writer Protocol Lock (`enforce_single_writer` in `[validation]` config):

  - Only Coordinator can create blocks (signature verified at protocol level).
  - Linear chain enforced (exactly 1 parent per block, except genesis).
  - Finality is immediate (no k-depth, no orphans, no conflicts).
  - Default: `true` (Private DAG mode). Set to `false` for multi-writer.
- **test**: Added `single_writer_enforcement.rs` with 7 tests for linear chain enforcement.
- **refactor**: Deleted 7 obsolete multi-writer/k-depth tests in preparation for Single Writer DAG:
  - `finality_kdepth.rs`, `finality_mvp.rs` (probabilistic finality)
  - `node_rewards_e2e.rs` (multi-miner rewards)
  - `inv_roundtrip.rs`, `local_stress.rs` (P2P multi-node)
  - `utxo_ledger_atomic.rs`, `stress_persist.rs` (concurrent UTXO/k-depth finality)
- fix: Added missing `treasury_addresses` field to `FeesSettings` in `automated_distribution_test.rs`.
- fix: Added missing `encrypted_metadata` and `new_owner_x25519_pubkey` fields to `NftAction::Transfer` in `nft_validation.rs`.
- arch: VPS 4-process separation (Engine, Gateway, Prometheus, Caddy) with internal API
- refactor: Remove PoW validation from block persistence (Private DAG: coordinator signature is sole authority)
- feat: Add P2P strict whitelist (`allowed_peer_ips`, `strict_whitelist` in `[p2p]` config) for private network enforcement
- docs: Update API documentation to align with Private DAG vision (removed PoW references, clarified server authority)
- config: Set `min_pow_leading_zero_bits = 0` in production template (Private DAG: server authority replaces PoW)
- docs: Major rewrite of README.md to align with "Centralized Private DAG" vision. Removed PoW, trustless, public blockchain references.
- fix: automatic inclusion of admin XPK in encrypted recipients for fee outputs to ensure Treasury visibility
- fix: Fixed `mint_security` and `mint_security_full` test compilation by aligning `ValidatePolicy` initialization with new fields.
- refactor: Updated `net_adapter.rs` to strictly use injected policy for Single Writer enforcement, fixing test overrides.
- feat(api): Added `POST /v1/tx/prepare` endpoint for preparing unsigned wallet-to-wallet transactions (UTXO selection, fee calculation, client-side signing flow).
- feat(sdk): Added `client.prepareTx()` method and `PrepareTxRequest`/`PrepareTxResponse` types for server-side transaction preparation.


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

- **Security & Configuration**
  - **Git Cleanup**: Removed sensitive `config.prod.toml` from version control.
  - **Simplified Setup**: Made `admin_wallet_file` optional for non-coordinator nodes.
  - **User Config**: Added `etc/config/pms-config-user.toml` template for simple nodes.
  - **Deployment**: Enhanced `deploy.sh` to generate secure production config automatically.

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

### Added
- **Automated Fee Distribution**:
  - Added automated fee distribution background task (interval configurable via `distribution_interval_sec`).
  - Refactored fee distribution logic into `fee_distribution.rs` module.
  - Coordinator automatically creates Mint blocks to distribute accumulated fees and burn refunds.
  - Coordinator automatically creates Mint blocks to distribute accumulated fees and burn refunds.
  - Added `automated_distribution_test.rs` ensuring distribution happens and pool resets.

### Fixed
- **Fee Distribution**: Fixed `extract_tx_fee` to correctly decrypt `EncryptedPayload` (was causing "Envelope mismatch" and preventing fee accumulation).
- **Dashboard Balances**: Fixed zero balances by exposing `node_balance` (Identity) and Aligned Treasury source to use file-backed wallets.
- **SDK Tests**: Fixed "NetworkError" in tests by allowing self-signed certificates via `setup.ts` and fixed `mintCube` mock.
- **DAG Metrics**:
  - feat: ajout du champ `treasury_details` à `/v1/supply` pour lister individuellement les portefeuilles [api]
- ui: remplacement de la carte Trésorerie par une liste détaillée "Treasury Wallets" sur le dashboard [dashboard]
- fix: initialisation de la métrique `PMS_BLOCKS_TOTAL` au démarrage [server]
- feat: ajout des champs `coordinator_balance` et `treasury_balance` à `/v1/supply` [api]
- ui: affichage des soldes Coordinateur et Trésorerie sur le dashboard [dashboard]
  - Fixed initialization of `PMS_BLOCKS_TOTAL` metric on startup (was starting at 0, now reads count from Store).
  - Added `block_count()` to `DagStorage` trait and `RocksStore` implementation.

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
    - **Verification Logic**: Refined `grep` to ignore placeholders in `authority_public_keys` and only check `coordinator_public_key`.
  - **Fail Fast**: Node process now exits (crashes) if API fails to start (e.g. missing treasury config), preventing "zombie" states where only P2P runs
  - **Checklist**: Added visual file status table to `deploy.sh`. Runs AFTER setup and STOPS deployment if critical files are missing. Correctly handles files to be generated.
  - **Docker Build**: Optimized build context by excluding `scripts/`, `docs/`, `backups/`, and other dev files in `.dockerignore`.
  - **TLS Config**: Fixed Caddy ACME registration failure by replacing invalid `admin@localhost` with `admin@pms-network.com` in generated Caddyfile.
  - **Healthcheck**: Restored `curl` healthcheck (validated working via debug script) as process-check proved unreliable.
  - **Secure Export**: Refactored `deploy.sh` to use `scp` for retrieving sensitive files. Expanded export to include **full treasury keys** (with mnemonics) from `treasury-keys/` directory.

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
