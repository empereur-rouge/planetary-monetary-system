# Changelog

All notable changes to the PMS DAG will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.4.4] - 2026-03-15 — OOM Fix + Containerd Cleanup

### Fixed
- **infra(critical)**: Fix engine OOM-kill at 4GB container limit. With 632K accumulated blocks, 500K UTXO cache, and 512MB RocksDB block cache, the engine exceeded the 4GB memory cap — triggering 108 container restarts and generating 200GB+ of containerd snapshots.

### Infrastructure
- **docker-compose**: Bumped engine `mem_limit` from 4GB to 6GB (`memswap_limit` too) to prevent OOM kills with large block histories.
- **config**: Reduced `max_utxos` from 500,000 to 250,000 in testnet config to lower memory footprint.
- **deploy**: Added containerd snapshot prune documentation and `docker image prune` to deploy script.
- **systemd**: Added `pms-containerd-cleanup.timer` (daily at 4 AM) to prevent containerd snapshot accumulation from container restarts.

---

## [0.4.3] - 2026-03-14 — Gateway TLS Fix + Error Logging + Deploy Cleanup

### Added
- **config**: New `api_tls_enabled` option in `[client]` section. When `false`, the HTTP API serves plain HTTP even if `[tls]` is configured. P2P TLS is unaffected. Default: `true` (backward compatible). Not used in testnet (simulates prod with full HTTPS).
- **gateway**: `EngineClient` now logs upstream URL and TLS mode on initialization.

### Fixed
- **gateway(critical)**: Fix persistent 502 Bad Gateway caused by silent client fallback. `Client::builder().build()` previously fell back to `Client::new()` on error, losing the `danger_accept_invalid_certs(true)` setting — all HTTPS requests to self-signed engine then failed with opaque "error sending request" messages. Now panics with a clear error message instead of silently degrading.
- **gateway**: Fix opaque error logging — proxy errors now use `{:?}` (Debug format) to show the full reqwest error chain (TLS failures, DNS errors, connection refused) instead of just the top-level "error sending request for url" message.
- **gateway**: `danger_accept_invalid_certs` now only applied when upstream is HTTPS (not for HTTP upstreams).

### Infrastructure
- **deploy**: Added `docker rmi` for old images before `docker load` to prevent containerd snapshot bloat (`/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/` grew to 213G+ in production).
- **deploy**: Added post-load `docker image prune -f` for dangling layers cleanup.

---

## [0.3.1] - 2026-03-14 — Documentation Obsidian Vault

### Added
- **docs**: Obsidian vault with 28 feature documentation files in `documentation/features/`.
- **docs**: Map of Content (`documentation/MOC.md`) indexing all fiches by category (Infrastructure, Données, Protocole & Consensus, Fonctionnalités).
- **docs**: API reference docs in `documentation/api/` (15 endpoint category files).
- **docs**: Added `.obsidian/` to `.gitignore` (user-specific workspace settings).
- **rules**: Documentation rules in CLAUDE.md — mandatory Obsidian fiches and rustdoc on all public items.
- **rules**: Changelog update rule in CLAUDE.md — mandatory update after every conversation with code changes.

---

## [0.3.0] - Unreleased — Economics System

### Economics Features

All economics features are **opt-in and disabled by default**. No configuration change is required — existing setups continue to work identically.

#### Feature 1: Fee Burn (Deflationary Mechanism)
- Configurable percentage of transaction fees permanently burned (removed from circulation).
- **Config (TOML):** `burn_rate_bps = 3000` in `[fees]` (3000 = 30%, default: `0` = disabled).
- **Runtime hot-swap:** `POST /admin/config` with `{ "SetBurnRate": { "bps": 3000 } }`.
- Set to `0` to disable.
- Burn stats exposed via `/v1/supply` (`total_burned` field).

#### Feature 2: Contract Deployment Fee
- Fee charged when registering a smart contract via `POST /admin/contracts`.
- **Config:** `contract_deployment_fee = "10.0"` in `[fees]` (default: not set = free).
- **Runtime hot-swap:** `{ "SetContractDeploymentFee": { "fee": "10.0" } }` (or `{ "fee": null }` to disable).

#### Feature 3: Cross-Ledger Fee Multiplier
- Bridge transfers between ledgers cost X times more than intra-ledger transfers.
- **Config:** `cross_ledger_fee_multiplier = 2.0` in `[fees]` (default: `2.0`).
- Set to `1.0` to effectively disable the surcharge.

#### Feature 4: Storage Fees (Per-KB Surcharge) — DISABLED BY DEFAULT
- Charges proportional to payload size (relevant for large NFT metadata).
- **Config:** `storage_fee_per_kb = "0.01"` in `[fees]` (default: not set = **disabled**).
- **Runtime hot-swap:** `{ "SetStorageFeePerKb": { "fee": "0.01" } }` (or `{ "fee": null }` to disable).
- Applied on: NFT mints (metadata size), transactions (payload size).

#### Feature 5: Dynamic Fees (Congestion-Based) — DISABLED BY DEFAULT
- Fee multiplier based on current TPS: `multiplier = max(1.0, current_tps / target_tps)`, capped at `max_fee_multiplier`.
- **Config fields in `[fees]`:**
  - `dynamic_fee_enabled = false` (default: `false` = **disabled**)
  - `target_tps = 100` (default: 100 — fees start increasing above this)
  - `max_fee_multiplier = 5.0` (default: 5.0 — max 5x fee increase)
- **Runtime hot-swap:** `{ "SetDynamicFee": { "enabled": true, "target_tps": 100, "max_multiplier": 5.0 } }`.
- When disabled, fee multiplier is always `1.0` (no effect).

#### Feature 6: Gas Pool (Per-Ledger Anti-Spam)
- Each custom ledger has a PMS gas pool. Every transaction consumes gas. Depleted pool = ledger rejects transactions (HTTP 402).
- Main ledger is exempt (no gas check).
- **Config:** `gas_per_tx = "0.001"` and `gas_pool_min_balance = "10.0"` in `[fees]` (default: not set = disabled).
- **API endpoints:**
  - `POST /admin/gas-pool/deposit` — Deposit PMS into a ledger's gas pool.
  - `POST /admin/gas-pool/withdraw` — Withdraw from gas pool.
  - `GET /v1/gas-pool/{ledger_id}` — View pool balance and stats.
- Gas pools are auto-created (balance=0) when a ledger is created.

#### Subscription (Removed)
- Ledger subscription (annual fee) was removed from the engine. This is a billing/business concern better handled at the dashboard/application layer, not in the protocol engine.

### New Crates
- **`pms-types-economics`**: Shared types (`GasPool`, `FeeBurnResult`, `DynamicFeeInfo`).
- **`pms-economics`**: Pure business logic (fee burn, gas pool, storage fee, dynamic fee). No I/O dependencies.

### Storage
- **New RocksDB column families:** `gas_pools`, `ledger_subscriptions` (inert, kept for migration compatibility).
- **Migration 7 → 8** (`mig_7_to_8`): Creates new CFs. Auto-applied on startup.
- **Schema version:** `CURRENT_VER` 7 → 8.

### Gateway
- **refactor(gateway)**: Replaced ~100 explicit proxy routes with catch-all fallback. New Engine endpoints are now automatically proxied without gateway code changes.
- Only 7 routes remain explicit: 5 using `/internal/*` API with typed payloads + 2 SSE stream endpoints requiring streaming proxy.

### Version Bumps
- Software: `0.2.7` → `0.3.0` (MINOR — new features)
- Schema DB: `7` → `8` (new CFs)
- API: `1` → `2` (new endpoints)
- DAG Protocol: `1.1.0` → `1.2.0` (backward-compatible)

---

## [0.2.7] - 2026-03-14

### Fixed
- **fix(rocks)**: Pin L0 index/filter blocks in cache + increase shared LRU block cache 256MB → 512MB. v0.2.6's bloom filters on 31 CFs caused catastrophic cache thrashing (index+filter blocks from 31 CFs competed for 256MB), collapsing TPS to 0-2. `set_pin_l0_filter_and_index_blocks_in_cache(true)` prevents L0 eviction.

---

## [0.2.6] - 2026-03-14

### Performance
- **perf(rocks)**: Apply bloom filters (10-bit) + shared block cache to ALL 31 column families. Only 7/31 CFs had bloom filters — the other 24 (including hot-path CFs: `children_count`, `children_set`, `addr_activity`, `node_block_counts`, `tx_applied`) used `Options::default()`. As DB grew, point lookups on unfiltered CFs required scanning multiple SSTable levels, causing progressive TPS degradation (170 → 120 over ~1h).

---

## [0.2.5] - 2026-03-13

### Performance
- **perf(storage)**: Fix RocksDB L0 write stall causing TPS cliff 120 → 20. Root cause: default L0 thresholds (slowdown=20, stop=24) hit after ~40-60 min of sustained 120 TPS.
  - Centralize DB tuning in `apply_db_tuning()` (eliminates `new()`/`open_db_multi_prefix()` drift)
  - Raise L0 thresholds: slowdown 20 → 40, stop 24 → 56
  - `max_subcompactions=3` for parallel L0 draining, `max_background_jobs` 4 → 6
  - Rate-limit `compact_all()` with 200ms inter-CF pause, move interval 1h → 6h
  - Amortize `trim_tips()` every 64 blocks via `maybe_trim_tips()` + `persist_counter`
  - Stale-while-revalidate `top_tips()` cache (5s stale window)

---

## [0.2.4] - 2026-03-13

### Fixed
- **fix(storage)**: `ensure_column_families()` at bootstrap — creates any missing CFs from `CF_NAMES` at runtime. Fixes testnet crash with `missing column family eden:contracts` on DBs created before the smart contract system.
- **fix(storage)**: `add_ledger()` now checks ALL CFs unconditionally (not just `blocks`).

---

## [0.2.3] - 2026-03-13

### Performance
12 hot-path optimizations eliminating allocations, lock contention, and redundant RocksDB I/O:

**Storage layer:**
- Cache CF name strings in HashMap (eliminates ~11 `format!()` per block)
- Cache `RuntimeConfig` with 500ms TTL + write-through invalidation
- In-memory `DashSet` for frozen addresses (skip RocksDB on `is_frozen`)
- RocksDB write buffer 32MB×2 → 128MB×3 (reduce flush stalls)
- `persist_final()` → WriteBatch (batch all finalized block writes)
- `trim_tips()` → WriteBatch (batch tip deletes)
- `make_utxo_key()` pre-allocated buffer with `itoa` (no `format!()`)
- `multi_get_cf()` for parent counts in `append_block_atomic()`

**P2P / server layer:**
- `inflight_fetch`: Tokio `Mutex<HashMap>` → `DashMap` (lock-free)
- `seen_invs`: `tokio::sync::Mutex` → `std::sync::Mutex` (no `.await` in CS)
- Broadcast: serialize once as `Arc<str>`, clone ref per peer

**DAG (RAM layer):**
- `find_tips()`: `.take(MAX_TIPS_CAP)` before collect (avoid full scan)

---

## [0.2.1] - 2026-03-13

### Added
- **Smart Contract System**: Rule-based declarative contracts (coordinator-only).
  - `ContractScope::Global` / `ContractScope::Ledger(vec)` for per-ledger targeting
  - Triggers: `OnNftBurn`, `OnTokenBurn`; Formulas: `FixedRate`, `AttributeFormula`, `FixedAmount`
  - Contract engine, RocksDB storage (`contracts` CF), admin API endpoints (`POST/GET/DELETE /admin/contracts`)
  - Integrated into all 3 NFT burn handlers via `evaluate_contracts_after_burn()`
  - DB schema v5 → v6 migration for `contracts` CF

### Performance
- **fix(perf)**: Resolve TPS degradation with 2.6M+ blocks (4 critical bottlenecks):
  - K-depth finalization: O(N×BFS) → O(k²) incremental via `ancestors_within_depth()`
  - Cache `Settings` + `WireMeta` in `CoreAdapter` — removes `load_config()` disk I/O per block
  - `GetTips` frequency 200ms/1024 → 10s/64 — reduces network amplification
  - Prune finalized `HashSet` in `prune_oldest()` — bounds RAM (~340MB saved)

---

## [0.2.0] - 2026-03-12

Cumulative release covering all work from initial deployment (2026-01-08) through pre-versioning era. Includes architecture rewrite, feature additions, performance optimizations, and production hardening.

### Architecture

- **Centralized Private DAG**: Major rewrite from decentralized multi-writer to centralized single-writer model. Coordinator is the sole block authority — no PoW, no trustless consensus.
- **Single Writer Protocol Lock** (`enforce_single_writer` in `[validation]` config): linear chain (1 parent per block except genesis), immediate finality, coordinator signature verification at protocol level.
- **Engine + Gateway separation**: 4-process VPS architecture (Engine, Gateway, Prometheus, Caddy) with internal API.
- **Multi-ledger system**: Multiple isolated ledgers on a single engine instance. Each ledger has its own DAG, UTXO set, and block history.

### Features

#### Security & Authentication
- **API key authentication**: SHA-256 hashed keys, constant-time comparison, granular per-group/per-endpoint scopes (`wallet`, `nft`, `dag`, `supply`, `tokens`, `history`, `coordinator`, `*`). Admin CRUD: `POST/GET/DELETE /admin/api-keys`.
- **Admin Config API**: `GET/POST /admin/config` for runtime configuration changes (fee_rate_bps, base_fee, coordinator_fee_bps, treasury_fee_bps, min_pow_bits, max_mint_per_block, mint_enabled).
- **RuntimeConfig**: Simplified for Single Writer mode — `coordinator_fee_bps` (67%) + `treasury_fee_bps` (33%) with `validate_fee_split()`.

#### Treasury & Fees
- **Encrypted Reward Blocks**: Each reward output encrypted individually for [recipient, coordinator]. X25519 key extracted from bech32 address.
- **Automated Fee Distribution**: Background task with configurable `distribution_interval_sec`. Coordinator auto-creates Mint blocks for accumulated fees and burn refunds.
- **Fee system reinforcement**: Basis-point precision (bps), ascending tier validation, immediate mint fee enforcement.
- **Fee precision fix**: Fees could exceed 8 decimals. Enriched `Amount` type with arithmetic operators that auto-round to 8 decimals.

#### NFTs & Tokens
- **NFT Burn-to-Mint & Refund**: "Cube" NFTs trigger refund `(weight * size * density) / 100`.
- **NFT queries**: `GET /v1/wallet/:address/nfts` with dual-write to `nfts` and `nfts_by_owner` CFs.
- **Coordinator-Only Minting**: `Mint` action restricted to Coordinator public key.
- **Multi-token**: Custom token creation and management.

#### Wallet & Activity
- **Wallet API**: Return `x25519_sk_hex` in wallet create/restore responses.
- **Per-address activity index** (`addr_activity` CF): O(wallet_blocks) instead of O(total_blocks) for activity queries. Migration 3 → 4.
- **Per-type activity index** (`addr_type_activity` CF): Filtered queries (e.g. `?type=mint`) scan only matching category. 9 categories. Migration 4 → 5.
- **Pre-computed activity items**: `activity_items` CF written at block time. LRU cache (10K entries, 30s TTL). `POST /admin/reindex-activity-items` for backfill.
- **Encrypted activity visibility**: Encrypted payloads now appear in activity feeds with minimal "encrypted" placeholder when no decryption key provided.
- **`POST /v1/tx/prepare`**: Server-side unsigned transaction preparation (UTXO selection, fee calculation).
- **`GET /v1/balance`**: Address-only balance queries without private keys.

#### Bridge & Multi-Ledger
- **Bridge module**: Cross-ledger token transfers with lock/mint mechanism.
- **Compliance module**: KYC/AML compliance framework.
- **Wallet factory**: Custodial wallet creation and management.

#### Simulator
- **Game engine**: Edenite game with cube NFT burn → EDN reward loop.
- **252-agent profiles** (7 types: user, fast, miner, whale, sniper, saver, observer).
- **Coordinator agent**: Sends 0.1-1 PMS per minute using node wallet.
- **OOM prevention**: Bounded channels, cube_registry cleanup, Docker memory limits.

### Performance

#### Lock-Free DAG (2437 → 4028 TPS)
- `ConcurrentDag` with `DashMap<BlockId, Block>` for lock-free storage.
- `DashSet` for concurrent spent outpoint tracking.
- Parents validated via `parents_exist_in_store()` (lock-free).
- IOTA-style genesis bootstrap for `min_parents = 2`.
- **Benchmark: 4028 TPS** (10 workers × 1000 tx, 2.48s, 0 failures).

#### Memory Optimizations
- **LRU UTXO cache**: Replace unbounded HashMap with bounded `LruCache` + RocksDB fallback. Configurable via `max_utxos` (default 500k).
- **UTXO streaming bootstrap**: `stream_all_utxos()` via `sync_channel` (10k buffer). O(buffer_size) instead of O(total_utxos).
- **CompactOutput** (~32 bytes vs ~148 bytes) with `Arc<str>` interning.
- **Address index**: `DashMap<String, DashSet<OutputId>>` for O(k) balance queries.
- **Shared RocksDB block cache**: Single 256MB cache across all CFs (~1.5 GB saved).
- **DAG pruning**: Insertion-order pruning with configurable `max_dag_blocks` (default 50K).

#### Storage Optimizations
- Selective DAG loading at bootstrap: only load newest `max_dag_blocks` from RocksDB (50K reads instead of 971K).
- Activity endpoint: `multi_get_cf` batch reads, bloom filter on `activity_items` CF.
- `native_balance_cache` (`DashMap`) for O(1) `balance_by_address()`.

### Fixed
- **Tipless DAG**: `prune_oldest()` could remove ALL tips, silently blocking fee distribution indefinitely (231k PMS blocked on testnet).
- **RocksDB tip protection**: `trim_tips()` and `remove_tip()` now refuse to delete the last remaining tip (dual-layer consistency with RAM fix).
- **Activity classification**: Sender resolved BEFORE UTXO spend. `transfer_self` only when ALL outputs return to sender.
- **Activity timestamps**: Fix mismatch between `id2ts` and `addr_type_activity` timestamps.
- **Fee output classification**: TxUtxo fee outputs classified as `fee_received` instead of `transfer_in`.
- **UTXO key separator**: Standardize on `#` (was inconsistent between write and parse paths).
- **Encrypted UTXO delta**: Encrypted transactions now correctly update UTXO cache.
- **Coordinator keys**: Allow custom coordinator keys in Testnet/Mainnet mode (don't override).
- **Mint policy**: Reject mint with empty `signer_pubkeys` in Testnet/Mainnet (Dev mode only allows bypass).
- **DAG pruning bugs**: Ghost entries, children_count overwrite, tip-skipping causing unbounded growth, poisoned mutex recovery.
- **Metrics**: DAG Size gauge now reflects actual in-memory count (not cumulative).
- **UTXO deadlock**: Fix lock ordering deadlock in `ShardedUtxoSet` under concurrent load.
- **Silent errors**: Replace `unwrap()` with poison recovery, bound PoW mining loop, replace `eprintln!` with tracing.

### Infrastructure
- **CI/CD**: GitHub Actions with formatting, clippy (hard failure), security audit, Docker builds (engine + gateway).
- **Testnet deployment**: `deploy-testnet.sh` with simulator (97 agents), `upgrade-testnet.sh` for code updates.
- **Production deployment**: `deploy.sh` with pre-flight checks, auto-generated API keys, secure credential backup.
- **Docker**: Memory limits, healthcheck start_period 120s, optimized `.dockerignore`.
- **Dependency security**: `time` crate updated for RUSTSEC-2026-0009.

### Version Bumps
- Software: `0.1.0` → `0.2.0`
- Schema DB: `3` → `7` (migrations 3→4, 4→5, 5→6, 6→7)
- API: `1` (introduced)

---

## [0.1.0] - 2025-12-28

### Added

#### Core DAG
- Block structure with parents, payload, nonce, signature.
- UTXO ledger with atomic updates.
- Tips selection algorithm.
- Orphan block handling with parent dependency tracking.

#### P2P Network
- TLS mutual authentication.
- Gossip protocol for block propagation.
- `GetTips`, `GetBlock`, `Inv`, `Blocks` messages.
- Rate limiting and anti-flood protection.

#### Storage
- RocksDB persistence with column families.
- Background maintenance (flush, compaction).
- Crash recovery support.

#### API
- REST endpoints: `/submit/block`, `/wallet/tx/send`, `/wallet/balance`.
- Health checks: `/live`, `/ready`, `/healthz`.
- Metrics endpoint: `/metrics`.
- Admin routes with token authentication.

#### Wallet
- Ed25519 + X25519 keypair generation.
- Bech32 address encoding.
- Transaction signing.
- UTXO scanning and balance calculation.

#### Security
- Payload encryption (X25519 + ChaCha20-Poly1305).
- Block signature verification.
- IP-based rate limiting.
- Proof-of-Work validation.

### Infrastructure
- Docker multi-node setup (3 nodes + Caddy).
- CI workflow + Grafana dashboard.
- TLS certificate generation scripts.
- Configuration management (TOML).

---

## Version History

| Version | Date | Highlights |
|---------|------|------------|
| 0.3.0 | Unreleased | Economics system (fee burn, gas pools, dynamic fees) |
| 0.2.7 | 2026-03-14 | Pin L0 index/filter + 512MB cache |
| 0.2.6 | 2026-03-14 | Bloom filters on all 31 CFs |
| 0.2.5 | 2026-03-13 | Fix RocksDB L0 write stall (120→20 TPS cliff) |
| 0.2.4 | 2026-03-13 | Fix missing CF crash at bootstrap |
| 0.2.3 | 2026-03-13 | 12 hot-path optimizations |
| 0.2.1 | 2026-03-13 | Smart contracts + TPS fix at scale |
| 0.2.0 | 2026-03-12 | Architecture rewrite, features, 4028 TPS |
| 0.1.0 | 2025-12-28 | Initial DAG implementation |
