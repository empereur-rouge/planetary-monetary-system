# DAG-PMS Technical Documentation

A comprehensive technical reference for the DAG-PMS payment system.
Written for developers joining the project with zero prior context.

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Architecture Overview](#2-architecture-overview)
3. [Type System & Data Structures](#3-type-system--data-structures)
4. [Configuration System](#4-configuration-system)
5. [Storage Layer (pms-storage)](#5-storage-layer)
6. [Core Engine (pms-core)](#6-core-engine)
7. [Server & API (pms-server)](#7-server--api)
8. [P2P Networking](#8-p2p-networking)
9. [Wallet System (pms-wallet)](#9-wallet-system)
10. [Multi-Ledger System (pms-ledger)](#10-multi-ledger-system)
11. [Multi-Token System](#11-multi-token-system)
12. [Gateway (pms-gateway)](#12-gateway)
13. [CLI Tools (tools-cli)](#13-cli-tools)
14. [Testing Guide](#14-testing-guide)
15. [Deployment Guide](#15-deployment-guide)

---

## 1. Introduction

### 1.1 What is DAG-PMS

DAG-PMS is a UTXO-based payment system built on a Directed Acyclic Graph (DAG) instead of a traditional blockchain. Written in Rust, it is designed for high-throughput private payment networks with coordinator-centric consensus.

Unlike a blockchain where blocks form a single linear chain, DAG-PMS allows blocks to reference multiple parent blocks simultaneously, creating a graph structure. This enables higher concurrency — multiple blocks can be created in parallel without conflicting.

### 1.2 Design Goals

- **UTXO model**: Transactions consume inputs and produce outputs, like Bitcoin. This enables parallel validation and eliminates account-state conflicts.
- **DAG structure**: Blocks reference 1-N parents, enabling parallel block creation and higher throughput than linear chains.
- **Coordinator-centric (Single Writer)**: A trusted coordinator signs and orders all blocks. This simplifies consensus while maintaining cryptographic auditability.
- **Privacy**: Supports encrypted payloads (AES-GCM + X25519 ECDH) for transaction privacy.
- **Multi-ledger**: A single node can host multiple independent ledgers sharing infrastructure.
- **Multi-token**: Native support for custom tokens alongside the PMS native currency.

### 1.3 Technology Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (edition 2024) |
| Async Runtime | Tokio (multi-threaded) |
| HTTP Framework | Axum 0.8 |
| Database | RocksDB 0.24 |
| Cryptography | k256 (ECDSA secp256k1), x25519-dalek, AES-GCM |
| Serialization | serde + serde_json |
| TLS | rustls 0.23 + tokio-rustls |
| Address Encoding | Bech32m (BIP-350) |
| Key Derivation | BIP-39 (24-word mnemonic) |
| Precision Arithmetic | rust_decimal |

---

## 2. Architecture Overview

### 2.1 System Diagram

```
                    ┌──────────────┐
                    │   Gateway    │  (pms-gateway)
                    │  HTTP Proxy  │
                    └──────┬───────┘
                           │ HTTPS
                    ┌──────▼───────┐
                    │   HTTP API   │  (pms-server/api.rs)
                    │   (Axum)     │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
       ┌──────▼──┐  ┌──────▼──┐  ┌─────▼────┐
       │  Server  │  │ Fee Dist│  │  Admin   │
       │  (P2P)   │  │  Pool   │  │  Routes  │
       └──────┬───┘  └─────────┘  └──────────┘
              │
       ┌──────▼──────────────────────────────┐
       │          Core Engine                 │
       │  ┌───────────┐  ┌────────────────┐  │
       │  │ Concurrent│  │   Sharded      │  │  (pms-core)
       │  │    DAG     │  │  UTXO Set     │  │
       │  │ (DashMap)  │  │ (256 shards)  │  │
       │  └───────────┘  └────────────────┘  │
       │  ┌───────────┐  ┌────────────────┐  │
       │  │ Validation│  │  Block Builder │  │
       │  │ Pipeline   │  │  (PoW Mining) │  │
       │  └───────────┘  └────────────────┘  │
       └──────────────┬──────────────────────┘
                      │
       ┌──────────────▼──────────────────────┐
       │          Storage Layer               │
       │  ┌─────────────────────────────────┐│
       │  │  RocksDB (Column Families)      ││  (pms-storage)
       │  │  blocks | utxos | tips | ver    ││
       │  │  token_registry | nft | config  ││
       │  └─────────────────────────────────┘│
       └─────────────────────────────────────┘
```

### 2.2 Crate Map

The workspace contains ~27 crates organized in 5 layers:

**Layer 1 — Type Definitions** (no internal dependencies)
| Crate | Purpose |
|-------|---------|
| `pms-types-transaction` | `Transaction`, `TxInput`, `TxOutput`, `OutputId`, `Unlock` |
| `pms-types-payload` | `PlainPayload` (9 variants), `PayloadEnvelope`, `TokenMetadata` |
| `pms-types-block` | `Block`, `BlockMetadata`, `BlockId` |
| `pms-types-mint` | Mint-related types |
| `pms-types-nft` | `NftAction` (Mint, Transfer, Burn, Use, BatchBurn) |
| `pms-types-dag` | DAG-specific types |
| `pms-types` | Umbrella crate re-exporting all type crates |
| `pms-errors` | Error types (`DagError`, `MigError`, `ValidationError`) |
| `pms-token` | `Amount` type using `rust_decimal` for precision |

**Layer 2 — Infrastructure**
| Crate | Purpose |
|-------|---------|
| `pms-config` | TOML config loading, `Settings`, `RuntimeConfig` |
| `pms-crypto` | Ed25519 signing (ed25519-dalek) |
| `pms-utils` | `compute_block_id()`, `check_pow_leading_zero_bits()` |
| `pms-wire` | `WireBlock` serialization, `WireMeta` network metadata |
| `pms-interface` | `NetDagAdapter` trait definition |

**Layer 3 — Core Engine**
| Crate | Purpose |
|-------|---------|
| `pms-storage` | RocksDB abstraction, atomic writes, UTXO storage, token registry |
| `pms-core` | `CoreAdapter`, `ConcurrentDag`, `ShardedUtxoSet`, validation pipeline |
| `pms-wallet` | Key derivation (BIP-39), address generation (Bech32m), signing |
| `pms-consensus` | Coordinator public key constants (placeholder for future consensus) |

**Layer 4 — Server**
| Crate | Purpose |
|-------|---------|
| `pms-server` | HTTP API (Axum), P2P server, fee distribution, node registry |
| `pms-network` | P2P message types (`NetMsg` enum) |
| `pms-event` | Event bus (tokio broadcast channels) |
| `pms-ledger` | `LedgerManager`, `LedgerInstance` for multi-ledger support |

**Layer 5 — Applications**
| Crate | Purpose |
|-------|---------|
| `bin` | Main binary entry point |
| `pms-gateway` | Reverse proxy gateway |
| `tools-cli` | CLI utilities (key generation, treasury signing, REPL) |
| `pms-testkit` | Test helpers (RocksDB spawn, mock adapters, mint helpers) |

### 2.3 Transaction Lifecycle (End-to-End)

```
1. Client builds transaction
   └── Selects UTXOs as inputs
   └── Creates outputs (recipients + change)
   └── Calculates fee
   └── Signs each input with ECDSA

2. Client submits via HTTP
   └── POST /submit/block (or POST /wallet/tx/send for auto-build)

3. Server receives WireBlock
   └── Validates network_id + protocol_version
   └── Verifies block signature (ECDSA)
   └── Enforces single-writer (coordinator only)
   └── Checks payload size limit

4. Core Engine validates
   └── Parent existence (RAM DAG + RocksDB)
   └── UTXO availability (ShardedUtxoSet)
   └── Balance conservation per asset
   └── Signature verification per input
   └── Fee validation

5. Persistence
   └── Update RAM: ShardedUtxoSet (spend inputs, create outputs)
   └── Insert into ConcurrentDag (lock-free)
   └── Fire-and-forget to background RocksDB writer

6. Broadcast
   └── Gossip Inv message to connected peers
   └── Peers request block via GetBlock
```

---

## 3. Type System & Data Structures

### 3.1 Transaction Model

**File:** `crates/pms-types-transaction/src/transaction.rs`

```rust
struct Transaction {
    inputs:  Vec<TxInput>,   // References to UTXOs being spent
    outputs: Vec<TxOutput>,  // New UTXOs being created
    fee:     String,         // Decimal string (e.g., "0.00100000")
    unlocks: Vec<Unlock>,    // One signature per input
}

struct TxInput {
    out: OutputId,           // Reference to the UTXO being consumed
}

struct TxOutput {
    address:  String,              // Bech32m recipient address
    amount:   String,              // Decimal string (e.g., "5.00000000")
    asset_id: Option<String>,      // None = PMS native, Some("token_id") = custom token
}

struct OutputId {
    txid:  String,  // Transaction/block ID that created this output
    index: u32,     // Output index within that transaction
}

struct Unlock {
    pubkey_hex:    String,  // Signer's ECDSA public key (hex)
    signature_b64: String,  // ECDSA signature (base64)
}
```

**Signing Message:** `SHA256(canonical_json(inputs + outputs + fee))` — deterministic serialization ensures the same transaction always produces the same signing message.

### 3.2 Block Structure

**File:** `crates/pms-types-block/src/block.rs`

```rust
struct Block {
    id:       String,                   // SHA256 hash of (parents, payload, nonce)
    parents:  Vec<String>,              // 0 (genesis) to N parent block IDs
    payload:  Option<PayloadEnvelope>,  // Block content (transaction, mint, etc.)
    nonce:    u64,                      // Anti-spam PoW nonce
    signer_pk:  Option<String>,         // ECDSA public key of signer
    signature:  Option<String>,         // ECDSA signature over canonical message
    metadata:   Option<BlockMetadata>,  // NOT included in block ID computation
}

struct BlockMetadata {
    description:      Option<String>,        // Human-readable
    tags:             Vec<String>,           // Categorization
    extra:            Option<serde_json::Value>,  // Extensibility
    signer_x25519_hex: Option<String>,       // X25519 pubkey for encryption
}
```

**Block ID computation:** `SHA256(sort(parents) + serialize(payload) + nonce)` — metadata is excluded, allowing it to be modified without changing the block's identity.

### 3.3 Payload Variants

**File:** `crates/pms-types-payload/src/payload.rs`

```rust
enum PayloadEnvelope {
    Plain(PlainPayload),         // Visible content
    Encrypted(EncryptedPayload), // AES-GCM encrypted
}

enum PlainPayload {
    Genesis,                          // First block, no parents
    Mint { outputs: Vec<TxOutput> },  // Create new coins (coordinator only)
    TxUtxo(Transaction),              // Standard UTXO transaction
    Milestone {                       // Finality checkpoint
        approved: Vec<String>,
        distribute_node_rewards: bool,
    },
    Nft(NftAction),                   // NFT operations
    ConfigUpdate(ConfigUpdate),       // Runtime config change
    Reward { ... },                   // Fee distribution (plain)
    EncryptedReward { ... },          // Fee distribution (encrypted)
    TokenCreate(TokenMetadata),       // Register a new token type
}
```

### 3.4 DAG Structure

The DAG is a directed graph where:
- Each block references 1+ parent blocks (except genesis which has 0)
- Parents are sorted lexicographically before ID computation (canonical ordering)
- **Tips** are blocks with no children (the "frontier" of the DAG)
- **Finality** is determined by k-depth confirmation or Milestone blocks

In Single Writer mode (current production mode), the DAG degenerates into a linear chain (1 parent per block).

### 3.5 Token & NFT Types

**TokenMetadata** (registered via `PlainPayload::TokenCreate`):
```rust
struct TokenMetadata {
    asset_id:       String,          // Unique identifier (e.g., "edenite")
    symbol:         String,          // Short symbol (e.g., "EDEN")
    name:           String,          // Full name
    decimals:       u8,              // Precision (0-18)
    max_supply:     Option<String>,  // None = unlimited
    creator:        String,          // Creator's address
    mint_authority: String,          // Public key authorized to mint
}
```

**NftAction** variants: `Mint`, `Transfer`, `Burn`, `Use`, `BatchBurn`

---

## 4. Configuration System

### 4.1 Config Loading Chain

**File:** `crates/pms-config/src/settings.rs`

Configuration is loaded in priority order (later sources override earlier):

1. **Built-in defaults** — hardcoded in `Settings::default()`
2. **Config file** — path from `$PMS_CONFIG` env var, or auto-discovered:
   - `./config.{mode}.toml`
   - `./config/config.{mode}.toml`
   - `/etc/pms/config.{mode}.toml`
3. **Environment variables** — `PMS__SECTION__KEY` format (double underscore for nesting)

### 4.2 Settings Sections

**File:** `crates/pms-config/src/config.rs`

| Section | Key Fields | Description |
|---------|-----------|-------------|
| `[rocks]` | `path`, `prefix`, `tip_limit` | RocksDB storage configuration |
| `[network]` | `mode`, `network_id`, `protocol_version` | Network identity |
| `[address]` | `hrp` | Bech32 human-readable prefix (default: `"8e"`) |
| `[admin]` | `signer_pubkeys`, `treasury_wallets_file` | Admin authorization |
| `[client]` | `bind_addr`, `api_addr` | Server bind addresses |
| `[tls]` | `cert_pem`, `key_pem`, `ca_pem` | TLS certificate paths |
| `[limits]` | `max_body_bytes`, `rate_limit_rps`, `burst` | Rate limiting |
| `[auth]` | `admin_api_token`, `allowed_ips`, `require_signed_submit` | Authentication |
| `[validation]` | `min_pow_leading_zero_bits`, `max_payload_bytes`, `enforce_single_writer` | Block validation rules |
| `[fees]` | `treasury_addresses`, `annual_inflation_percent`, `authority_public_keys` | Fee distribution |
| `[p2p]` | `known_peers`, `allowed_peer_ips`, `strict_whitelist` | P2P networking |
| `[[ledgers]]` | `id`, `prefix`, `network_id` | Multi-ledger definitions |

### 4.3 Runtime Config (Hot-Swappable)

**File:** `crates/pms-config/src/runtime.rs`

Some parameters can be changed at runtime via `PlainPayload::ConfigUpdate` blocks:

```rust
struct RuntimeConfig {
    fee_rate_bps:         u32,   // Fee rate in basis points
    base_fee:             String,// Minimum base fee
    coordinator_fee_bps:  u32,   // Coordinator's fee share (bps)
    treasury_fee_bps:     u32,   // Treasury's fee share (bps)
    min_pow_bits:         u8,    // PoW difficulty
    max_mint_per_block:   String,// Max mint amount per block
    mint_enabled:         bool,  // Whether minting is enabled
    updated_at_block:     String,// Block ID of last update
    updated_at_timestamp: i64,   // Unix timestamp of last update
}
```

### 4.4 Multi-Ledger Config

If a `[[ledgers]]` array is present in the TOML config, each entry defines a separate ledger:

```toml
[[ledgers]]
id = "main"
prefix = "main"
network_id = "pms-main"
protocol_version = 1

[[ledgers]]
id = "secondary"
prefix = "sec"
network_id = "pms-secondary"
protocol_version = 1
```

If no `[[ledgers]]` is defined, a single `"main"` ledger is automatically created using the top-level `[rocks]` and `[network]` settings (backward compatibility via `Settings::effective_ledgers()`).

---

## 5. Storage Layer

### 5.1 RocksDB Architecture

**File:** `crates/pms-storage/src/rocks_store/store.rs`

The storage layer uses RocksDB with **Column Families** (CFs) for logical separation:

| Column Family | Content | Key Format |
|--------------|---------|------------|
| `blocks` | Block data (JSON) | Block ID (hex string) |
| `utxos` | UTXO values | `{txid}:{index}` |
| `tips` | Current DAG tips | Block ID |
| `idx_blocks` | Block index (ordered) | Auto-increment |
| `id2ts` | Block ID → timestamp | Block ID |
| `children_count` | Parent → child count | Parent block ID |
| `ver` | Schema version | `"ver"` |
| `config` | Runtime config | `"runtime_config"` |
| `token_registry` | Token metadata | Asset ID |
| `nfts` | NFT ownership | Token ID |
| `fee_pool` | Fee accumulation | `"total"` |
| `node_rewards` | Per-node block counts | Node public key |

**Multi-ledger isolation:** Each ledger gets its own CF prefix (e.g., `main:blocks`, `sec:blocks`). This allows multiple ledgers to share a single RocksDB instance while remaining logically isolated.

**RocksDB options:**
- Write buffer: 64 MB (3 buffers max)
- SSTable size: 64 MB
- Dynamic level bytes enabled (LSM optimization)
- Bloom filters (10 bits/key) on index CFs for fast lookups
- Background compaction: hourly
- WAL flush: every 10 minutes

### 5.2 Block Persistence (Atomic Writes)

**File:** `crates/pms-storage/src/rocks_store/atomic.rs`

Block persistence uses `WriteBatch` for atomicity — all-or-nothing writes:

```
WriteBatch:
  1. PUT blocks/{id} = serialized block
  2. PUT idx_blocks/{auto_id} = block_id
  3. PUT id2ts/{id} = timestamp
  4. ADD tips/{id}
  5. REMOVE tips/{parent_id} for each parent
  6. INCREMENT children_count/{parent_id} for each parent
  7. (If UTXO delta) PUT/DELETE utxos/{txid:idx}
```

### 5.3 UTXO Storage

**File:** `crates/pms-storage/src/rocks_store/utxo.rs`

UTXO values are stored as JSON:
```rust
struct OutVal {
    addr: String,           // Owner address
    amt:  String,           // Amount (decimal string)
    ast:  Option<String>,   // Asset ID (None = PMS native)
}
```

Key format: `{txid}:{index}` (binary). The `asset_id` field uses `#[serde(default, skip_serializing_if)]` for backward compatibility with pre-multi-token data.

### 5.4 Token Registry

**File:** `crates/pms-storage/src/rocks_store/token_registry.rs`

Stores `TokenMetadata` JSON in the `token_registry` CF. Key methods:
- `register_token(metadata)` — validates metadata format, checks uniqueness, persists
- `get_token(asset_id)` — retrieves metadata by ID
- `list_tokens()` — iterates all registered tokens

### 5.5 NFT Storage

**File:** `crates/pms-storage/src/rocks_store/nft_storage.rs`

NFT state tracks ownership and provenance:
- `apply_mint(token_id, creator, block_id)` — creates NFT record
- `apply_transfer(token_id, new_owner, block_id)` — updates owner + reference block
- `apply_action(action)` — handles Burn, Use, BatchBurn
- `get_nft(token_id)` — retrieves ownership info
- `get_nfts_by_owner(address)` — lists all NFTs owned by an address

### 5.6 Migrations

**File:** `crates/pms-storage/src/rocks_store/migration.rs`

Version-based sequential migrations:
- **v0 → v1**: Initialize block index CF
- **v1 → v2**: Rebuild tips from all persisted blocks (iterates entire DB)

`ensure_schema()` runs at startup, applying any pending migrations. Progress is logged every 10,000 blocks.

### 5.7 Multi-Ledger Prefix Isolation

Each ledger gets a unique `prefix` string. Column families are named `{prefix}:{cf_name}`. Example:
- Ledger "main": `main:blocks`, `main:utxos`, `main:tips`, ...
- Ledger "secondary": `sec:blocks`, `sec:utxos`, `sec:tips`, ...

CFs are created at database open time. Dynamic ledger creation requires pre-configured CFs in the TOML config.

---

## 6. Core Engine

### 6.1 ShardedUtxoSet

**File:** `crates/pms-core/src/utxo.rs`

The UTXO set is partitioned into **256 independent shards** for concurrent access:

```
Shard index = first byte of txid (hex[0..2] parsed as u8)

┌────────┐ ┌────────┐     ┌────────┐
│Shard 0 │ │Shard 1 │ ... │Shard255│
│RwLock  │ │RwLock  │     │RwLock  │
│HashMap │ │HashMap │     │HashMap │
└────────┘ └────────┘     └────────┘
```

Each shard is protected by an independent `tokio::sync::RwLock`. Operations:
- `get(outpoint)` — read lock on 1 shard
- `add(outpoint, output)` — write lock on 1 shard
- `remove(outpoint)` — write lock on 1 shard
- `apply_diff(spends, creates)` — groups by shard, single write lock per affected shard
- `balance_by_address(addr)` — scans all 256 shards (read locks)
- `circulating_supply()` — scans all 256 shards, sums native PMS amounts

### 6.2 ConcurrentDag

The DAG is stored as a lock-free `DashMap<String, Block>` with:
- `insert_block(block)` — O(1) insertion
- `contains_block(id)` — O(1) lookup
- `find_tips()` — returns blocks with no children
- `mark_spent(txid, idx)` — tracks spent outpoints for double-spend detection
- `count_descendants(id, max_depth)` — BFS for k-depth finality
- `finality: RwLock<FinalityState>` — tracks finalized blocks and last milestone

### 6.3 Block Builder & PoW Mining

**File:** `crates/pms-core/src/block_builder.rs`

`BlockMineBuilder` mines a nonce that satisfies the PoW difficulty:

1. Sort parents lexicographically (canonical ordering)
2. Try 1M sequential nonces starting from a random offset
3. If not found, switch to fully random nonce generation
4. PoW check: count leading zero nibbles in hex block ID

In Single Writer mode, PoW is typically disabled (`min_pow_bits = 0`) since the coordinator's signature is the trust anchor.

### 6.4 Validation Pipeline

**File:** `crates/pms-core/src/validations/check.rs`, `crates/pms-core/src/net_adapter.rs`

When a block arrives, validation proceeds in this order (cheapest checks first):

#### 6.4.1 Wire-Level Validation
1. **Network check**: `network_id` and `protocol_version` match local config
2. **Signer required**: `signer_pk_hex` must be non-empty
3. **Signature required**: `signature_hex` must be non-empty
4. **Signature verification**: ECDSA verify over canonical wireblock message
5. **Single Writer enforcement**: signer must be the coordinator (if enabled)

#### 6.4.2 Structural Validation
6. **Payload size**: JSON length ≤ `max_payload_bytes`
7. **Payload deserialization**: JSON → `PayloadEnvelope`
8. **Payload-specific checks**: Mint policy, NFT validation, ConfigUpdate
9. **Parent uniqueness**: no duplicate parent references
10. **Self-parent forbidden**: block cannot reference itself
11. **Parent count**: ≥ `min_parents_after_boot` (or exactly 1 in Single Writer)

#### 6.4.3 DAG Validation
12. **Parent existence**: each parent must exist in RAM DAG or RocksDB store
13. **UTXO validation** (for TxUtxo payloads): inputs exist and are unspent

#### 6.4.4 Transaction Validation

**File:** `crates/pms-core/src/validations/transactions.rs`

Per-asset balance conservation:
```
For each asset_id:
  sum(input amounts for this asset) == sum(output amounts for this asset)
```
Fees are always in PMS native token (`asset_id: None`).

#### 6.4.5 Signature Verification

**File:** `crates/pms-core/src/validations/signature.rs`

- **< 4 inputs**: sequential verification
- **≥ 4 inputs**: parallel verification with `rayon`
- Signing message computed once, shared across all verifications

#### 6.4.6 Fee Validation

**File:** `crates/pms-core/src/validations/fees.rs`

Validates that the transaction includes a platform fee output at the correct address and amount.

### 6.5 NetDagAdapter

**File:** `crates/pms-interface/src/net_adapter.rs`

`NetDagAdapter` is the core trait that bridges storage, DAG, and UTXO operations:

```rust
#[async_trait]
trait NetDagAdapter: Send + Sync {
    async fn have_block(&self, id: &str) -> bool;
    async fn persist_block(&self, wb: &WireBlock) -> Result<PutResult>;
    async fn top_tips(&self, limit: usize) -> Result<Vec<String>>;
    async fn get_block(&self, id: &str) -> Result<Option<WireBlock>>;
    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>>;
    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>>;
    fn min_pow_leading_zero_bits(&self) -> u8;
    async fn circulating_supply(&self) -> (Decimal, u64);
    async fn balance_by_address(&self, address: &str) -> Decimal;
    async fn utxos_by_address(&self, addr: &str) -> Vec<(OutputId, TxOutput)>;
    async fn add_utxo(&self, txid: String, index: u32, address: String, amount: String, asset_id: Option<String>);
    async fn remove_utxo(&self, output_id: &OutputId) -> bool;
    async fn get_utxo(&self, output_id: &OutputId) -> Option<TxOutput>;
}
```

`CoreAdapter<S>` is the concrete implementation that wires together `ConcurrentDag`, `ShardedUtxoSet`, and any `DagStorage + NftStorage` store.

---

## 7. Server & API

### 7.1 AppState Structure

**File:** `crates/pms-server/src/api.rs`

```rust
struct AppState {
    srv:             Arc<Server>,                  // P2P server + adapter
    _cfg:            Arc<ServerConfig>,             // Server configuration
    _ready:          Arc<AtomicBool>,               // Health check flag
    stats:           Arc<Stats>,                    // Perf counters
    store:           Arc<RocksStore>,               // Direct RocksDB access
    admin_token:     Option<String>,                // Resolved admin token
    node_wallet:     Arc<Wallet>,                   // Node identity
    settings:        Arc<Settings>,                 // Full config
    allowed_networks: Vec<IpNetwork>,               // Admin IP whitelist
    treasury_wallets: TreasuryWallets,              // Signed treasury wallet list
    node_registry:   SharedNodeRegistry,            // Registered TX processing nodes
    fee_pool:        SharedFeePool,                 // Fee accumulation
    ledger_mgr:      Option<Arc<LedgerManager>>,    // Multi-ledger (optional)
}
```

### 7.2 Router & Middleware Stack

**File:** `crates/pms-server/src/api.rs`

Middleware is applied bottom-up (last added = first executed):

```
Request → TraceLayer → CORS → ConcurrencyLimit(256) → BodyLimit → Timeout → RateLimit → Router
```

| Middleware | Purpose |
|-----------|---------|
| `TraceLayer` | Request/response logging |
| `CorsLayer::permissive()` | Cross-origin requests |
| `ConcurrencyLimit(256)` | Max concurrent requests |
| `RequestBodyLimitLayer` | Configurable body size limit |
| `TimeoutLayer` | Request timeout (default 4s) |
| `GovernorLayer` | Per-IP rate limiting (SmartIpKeyExtractor) |

### 7.3 Authentication & Authorization

**Admin routes** require BOTH:
1. **IP allowlist**: Request must come from localhost OR a CIDR-whitelisted IP
2. **Token**: `Authorization: Bearer {token}` or `X-Admin-Token: {token}` header

Token comparison uses constant-time equality (`subtle::ConstantTimeEq`) to prevent timing attacks.

### 7.4 Public API Endpoints

| Method | Path | Handler | Description |
|--------|------|---------|-------------|
| GET | `/livez` | — | Process alive check |
| GET | `/healthz` | — | Ready check (DB initialized) |
| POST | `/submit/block` | `submit_block` | Ingest a signed WireBlock |
| POST | `/wallet/tx/send` | `wallet_send_tx` | Build + sign + submit transaction |
| POST | `/wallet/balance` | `wallet_balance` | Query wallet balance |
| POST | `/wallet/history` | `get_wallet_history` | Encrypted transaction history |
| POST | `/v1/balance` | `balance_by_address` | Balance by address |
| POST | `/v1/tx/prepare` | `prepare_tx` | Prepare unsigned transaction |
| GET | `/v1/supply` | `get_circulating_supply` | Total PMS in circulation |
| GET | `/v1/fee_pool` | `get_fee_pool_status` | Fee pool balance |
| GET | `/v1/tokens` | `list_tokens` | List registered tokens |
| GET | `/v1/tokens/{asset_id}` | `get_token` | Token metadata |
| GET | `/v1/blocks/{id}` | `get_block_by_id` | Fetch block by ID |
| POST | `/v1/dag/tips` | `get_tips` | Current DAG tips |
| GET | `/v1/config` | `get_config` | Runtime configuration |
| GET | `/v1/nft/{token_id}` | `get_nft` | NFT details |
| GET | `/v1/wallet/{address}/nfts` | `get_nfts_by_owner` | NFTs by owner |
| POST | `/v1/nft/mint` | `mint_nft` | Mint NFT |
| POST | `/v1/nft/burn` | `burn_nft` | Burn NFT |
| GET | `/v1/wallet/{address}/utxos` | `get_utxos_by_address` | UTXOs by address |
| GET | `/v1/coordinator/info` | `get_coordinator_info` | Coordinator details |
| GET | `/blocks/stream` | `stream_blocks` | WebSocket block stream |

### 7.5 Admin API Endpoints

| Method | Path | Handler | Description |
|--------|------|---------|-------------|
| GET | `/admin/ping` | `admin_ping` | Test admin auth |
| POST | `/admin/compact` | `admin_compact` | Trigger DB compaction |
| GET | `/admin/config` | `admin_get_config` | Get runtime config |
| POST | `/admin/config` | `admin_update_config` | Update runtime config |
| POST | `/admin/tokens/create` | `admin_create_token` | Create new token type |
| POST | `/admin/tokens/mint` | `admin_mint_token` | Mint custom tokens |
| GET | `/admin/ledgers` | `admin_list_ledgers` | List all ledgers |
| POST | `/admin/ledgers/create` | `admin_create_ledger` | Create new ledger |
| GET | `/admin/ledgers/{id}` | `admin_get_ledger` | Ledger details |
| POST | `/admin/distribute_fees` | `distribute_fees` | Trigger fee distribution |

### 7.6 Fee Pool & Distribution

**File:** `crates/pms-server/src/fee_distribution.rs`

Fees are accumulated in a pool (not distributed per-transaction) and distributed periodically:

1. **Accumulation**: Each TxUtxo block's fee is split:
   - `treasury_fee_bps / 10000` → stored in RocksDB `fee_pool` CF
   - Per-node block count tracked in `node_rewards` CF

2. **Distribution** (triggered via `/admin/distribute_fees` or Milestone):
   - Treasury tax: configurable percentage to treasury wallets
   - Node rewards: remaining pool distributed proportionally to block count
   - Creates a `PlainPayload::Mint` block with output UTXOs for each recipient

3. **Daily Inflation**: Optional scheduled mint based on `annual_inflation_percent`:
   - `daily_amount = circulating_supply * rate / 365`
   - Split: creator reward + treasury reward + burn (not minted)

### 7.7 Node Registry

Nodes register via `POST /v1/register` with their public key and wallet address. Active nodes participate in fee distribution proportionally to their block contributions.

### 7.8 Dynamic Ledger Routing

Requests to `/l/{ledger_id}/{*rest}` are handled by `dynamic_ledger_handler`:

1. Extract `ledger_id` from URL path
2. Look up `LedgerInstance` in `LedgerManager`
3. Build a per-ledger `AppState` with the correct `store` and `adapter`
4. Strip the `/l/{ledger_id}` prefix from the URI
5. Route through the standard API router via `Router::oneshot()`

---

## 8. P2P Networking

### 8.1 Protocol Messages

**File:** `crates/pms-network/src/messages.rs`

The P2P protocol uses **line-delimited JSON** (JSONL) over TCP or TLS:

| Message | Direction | Purpose |
|---------|-----------|---------|
| `Hello` | Bidirectional | Handshake initiation (proto version, node_id, nonce) |
| `HelloAck` | Response | Handshake acknowledgment (ok/reject) |
| `Ping` | Request | Liveness check |
| `Pong` | Response | Liveness response |
| `Inv` | Broadcast | Announce new block IDs (gossip) |
| `GetBlock` | Request | Request single block by ID |
| `GetBlocks` | Request | Request multiple blocks by IDs |
| `Block` | Response | Single block data |
| `Blocks` | Response | Multiple blocks data |
| `GetTips` | Request | Request current DAG tips |
| `Tips` | Response | Current tip IDs |

### 8.2 Peer Lifecycle

```
1. TCP/TLS connection established
2. Both sides send Hello message
3. Receiver validates proto version and node_id
4. Receiver sends HelloAck (ok=true or ok=false)
5. On success: request GetTips to sync
6. Normal operation: exchange Inv/GetBlock/Block messages
7. Periodic Ping/Pong for liveness (every PING_EVERY_MS)
8. Disconnect on: timeout (60s idle), too many errors, or shutdown
```

### 8.3 Block Propagation

**Gossip protocol:**
1. When a new block is persisted, broadcast `Inv { ids: [block_id] }` to all inbound peers
2. Peers receiving `Inv` check if they have the block
3. If not, request via `GetBlock` or `GetBlocks`
4. Received blocks are validated and persisted, then re-gossiped

**Broadcast batching:** The `spawn_broadcast_worker` aggregates block IDs into batches (max 100) with a 10ms flush interval, reducing network overhead.

### 8.4 Orphan Handling

When a block arrives but its parents don't exist yet:

1. Store block in `orphans` DashMap (bounded to `MAX_ORPHANS = 10,000`)
2. Register dependency in `parent_dependency` map (bounded to `MAX_PARENT_DEPS = 20,000`)
3. Request missing parents via `GetBlock`
4. When a parent arrives and is persisted, re-process all dependent children
5. Periodic retry (every 200ms) requests missing parents for all orphans

### 8.5 Rate Limiting & Anti-Abuse

| Limit | Value | Purpose |
|-------|-------|---------|
| `MAX_LINE_BYTES` | 10 MB | Max message size |
| `PER_PEER_Q_CAP` | 10,000 | Output queue per peer |
| `RATE_MSGS_PER_SEC` | 10,000 | Token bucket rate |
| `RATE_BURST` | 20,000 | Token bucket burst |
| `MAX_PARSE_ERRORS` | 8 | Kick after N JSON errors |
| `HANDSHAKE_TIMEOUT_MS` | 1,500 | Max handshake wait |
| `MAX_BLOCKS_BATCH` | 512 | Max blocks per response |
| `MAX_INFLIGHT_GETBLOCK` | 100,000 | Max pending block requests |
| `INFLIGHT_TTL_MS` | 10,000 | TTL for pending requests |
| `SEEN_CAPACITY` | 10,000 | LRU gossip dedup cache |

### 8.6 TLS for P2P

- **Production mode**: TLS is mandatory. Missing cert/key files cause startup failure.
- **Dev/Testnet mode**: TLS is optional. Falls back to cleartext TCP if cert/key files are missing.
- **Client connections**: Support both IP-based and DNS-based SNI for `connect_to_peer()`.

---

## 9. Wallet System

### 9.1 Key Derivation

**File:** `crates/pms-wallet/src/wallet.rs`

```
24-word BIP-39 mnemonic
    └── Entropy (32 bytes)
        └── ECDSA secp256k1 private key (k256)
            ├── ECDSA public key (33 bytes compressed → hex)
            └── HKDF-SHA256 with key "pms/x25519-sk/v1"
                └── X25519 private key
                    └── X25519 public key (32 bytes → hex)
```

### 9.2 Address Format

Bech32m encoding with HRP (Human-Readable Part) `"8e"`:

```
Address = Bech32m("8e", SHA256(ECDSA_pubkey)[0..20] + X25519_pubkey[0..32])
         = 52 bytes data → ~90 character string
```

The address embeds both the ECDSA hash (for ownership verification) and the X25519 public key (for encrypted communication).

### 9.3 Transaction Building

**File:** `crates/pms-wallet/src/helpers.rs`

`prepare_and_sign_tx()`:
1. Select UTXOs from the sender's address (greedy, largest first)
2. Create outputs: recipient + change back to sender
3. Calculate fee and add fee output
4. Compute signing message: `SHA256(canonical_json(inputs, outputs, fee))`
5. Sign each input with the sender's ECDSA private key
6. Build `WireBlock` with the transaction as payload
7. Sign the wireblock with the node's identity key

### 9.4 Balance Computation

Balance is computed by scanning the `ShardedUtxoSet` for UTXOs matching the given address. The scan iterates all 256 shards (read locks) and sums amounts for the matching `asset_id`.

### 9.5 History Queries

**Plain history**: Iterates blocks in reverse order, extracting transactions involving the given address.

**Encrypted history**: Uses X25519 ECDH shared secret between the coordinator and the querying wallet to decrypt encrypted payloads. Only the coordinator and the transaction participants can decrypt.

---

## 10. Multi-Ledger System

### 10.1 LedgerManager & LedgerInstance

**Files:** `crates/pms-ledger/src/manager.rs`, `crates/pms-ledger/src/instance.rs`

`LedgerManager` manages N isolated `LedgerInstance`s:

```rust
LedgerManager {
    ledgers: DashMap<String, Arc<LedgerInstance>>,  // Thread-safe
    shared_db: Arc<PmsDb>,                          // Single RocksDB
    global_tip_limit: usize,
}

LedgerInstance {
    id:      String,                    // "main", "secondary", etc.
    dag:     Arc<ConcurrentDag>,        // Independent DAG per ledger
    utxos:   Arc<ShardedUtxoSet>,       // Independent UTXO set per ledger
    store:   Arc<RocksStore>,           // Prefix-isolated RocksDB access
    adapter: Arc<dyn NetDagAdapter>,    // CoreAdapter instance
    def:     LedgerDef,                 // Config (network_id, etc.)
}
```

### 10.2 Bootstrapping Flow

```
1. LedgerManager::bootstrap(settings)
2. Open shared RocksDB with all required CFs for all ledgers
3. For each LedgerDef in settings.effective_ledgers():
   a. Create RocksStore with ledger's prefix
   b. Run ensure_schema() (migrations)
   c. Create ConcurrentDag + ShardedUtxoSet
   d. Create CoreAdapter
   e. Bootstrap UTXOs from store
   f. Wrap as LedgerInstance
4. Return LedgerManager with all instances
```

### 10.3 Dynamic Ledger Creation

`POST /admin/ledgers/create` creates a new ledger at runtime:
1. Validate the `LedgerDef` (id, prefix, network_id must be unique)
2. Create `RocksStore` from the shared DB with the new prefix
3. Bootstrap the new `LedgerInstance`
4. Insert into `LedgerManager`'s DashMap

**Limitation:** Column families must be pre-configured in the initial TOML config, as RocksDB doesn't allow adding CFs to an open database.

### 10.4 P2P Routing by network_id

When a block arrives via P2P with a `network_id`:
1. `Server::adapter_for_network(network_id)` looks up the correct adapter
2. Falls back to the default adapter if no match
3. Cross-ledger queries (`have_block_any`, `get_block_any`, `all_tips`) iterate all adapters

---

## 11. Multi-Token System

### 11.1 Asset Model

Each `TxOutput` has an optional `asset_id` field:
- `asset_id: None` → PMS native currency
- `asset_id: Some("edenite")` → Custom token "edenite"

The `#[serde(default, skip_serializing_if = "Option::is_none")]` attribute ensures backward compatibility — old blocks without `asset_id` deserialize as PMS native.

### 11.2 Token Creation Flow

1. Coordinator creates a `PlainPayload::TokenCreate(TokenMetadata)` block
2. `net_adapter.rs` detects the payload and delegates to validation
3. `token_registry.rs` validates metadata format (asset_id, symbol, name, decimals, max_supply)
4. Token is persisted in the `token_registry` CF
5. Subsequent `Mint` blocks can specify `asset_id` to create tokens of that type

### 11.3 Per-Asset Balance Conservation

**File:** `crates/pms-core/src/validations/transactions.rs`

```
For each asset_id present in inputs OR outputs:
  input_sum  = sum(input.amount where input.asset_id == asset_id)
  output_sum = sum(output.amount where output.asset_id == asset_id)

  REQUIRE: input_sum == output_sum
```

### 11.4 Fee Handling

Fees are **always** in PMS native token (`asset_id: None`). A transaction involving custom tokens must still include PMS native inputs to cover the fee.

---

## 12. Gateway

### 12.1 Proxy Architecture

**File:** `crates/pms-gateway/src/main.rs`

The gateway is a standalone reverse proxy process that sits in front of the engine:

```
Client → Gateway (public) → Engine (internal)
```

Configuration is via environment variables (no config file):
- `ENGINE_URL` — upstream engine address
- `CORS_ALLOWED_ORIGINS` — comma-separated origins (or `*`)
- `TLS_CERT` / `TLS_KEY` — optional TLS for the gateway itself
- `RATE_LIMIT_RPS` / `RATE_LIMIT_BURST` — per-IP rate limiting

### 12.2 Route Mapping

| Gateway Path | Engine Path | Method |
|-------------|-------------|--------|
| `/healthz` | `/healthz` | GET |
| `/tips` | `/v1/dag/tips` | POST |
| `/blocks/{id}` | `/v1/blocks/{id}` | GET |
| `/submit/block` | `/submit/block` | POST |
| `/config` | `/v1/config` | GET |
| `/utxos/{addr}` | `/v1/wallet/{addr}/utxos` | GET |
| `/{*path}` | `/{*path}` | GET/POST (generic proxy) |

### 12.3 CORS & Rate Limiting

- CORS: Permissive if no origins configured (logs a warning). Specific origins when `CORS_ALLOWED_ORIGINS` is set.
- Rate limiting: Per-IP via `PeerIpKeyExtractor` with configurable RPS and burst.
- Request/connect timeouts: 30s request, 5s connect.

---

## 13. CLI Tools

### 13.1 Key Generation Commands

**File:** `crates/tools-cli/src/main.rs`

| Command | Description |
|---------|-------------|
| `gen-coordinator` | Generate a new coordinator wallet (keypair + mnemonic) |
| `derive-coordinator` | Derive keys from an existing private key hex |
| `check-coordinator` | Verify if the current node is the coordinator |

### 13.2 Treasury Management

| Command | Description |
|---------|-------------|
| `treasury-sign` | Sign a treasury wallet list with the coordinator's key |

The signed treasury wallet list is a JSON file containing wallet addresses and a coordinator signature, ensuring only the coordinator can modify the fee distribution targets.

### 13.3 REPL Mode

Interactive mode for manual operations:
- Query balances
- Send transactions
- Inspect blocks
- Manage configuration

---

## 14. Testing Guide

### 14.1 Test Infrastructure

**File:** `crates/pms-testkit/`

The testkit provides helpers for integration tests:
- `spawn_rocks(prefix)` — creates a temporary RocksDB in a tmpdir
- `make_test_app()` — builds a full `AppState` with test configuration
- `mint_genesis(store, amount)` — creates a genesis block with initial supply
- `MintHelper` — simplifies minting test UTXOs

### 14.2 Test Locations

| Crate | Test Files | Focus |
|-------|-----------|-------|
| `pms-core/tests/` | `general.rs`, `tx_validation.rs`, `multi_token_test.rs` | UTXO validation, balance conservation |
| `pms-storage/tests/` | `rocks_utxo.rs`, `token_registry_test.rs`, `multi_token_utxo_test.rs` | RocksDB operations |
| `pms-server/tests/` | `security_http.rs`, `wallet_send_fees.rs`, `network_batching.rs`, etc. | HTTP API, fee distribution, P2P |
| `pms-wallet/tests/` | `history_*.rs`, `tx_fees_and_change.rs` | Wallet operations, history |
| `pms-ledger/tests/` | `multi_ledger_test.rs` | Multi-ledger isolation |
| `pms-types-payload/tests/` | `general.rs` | Serialization roundtrips |

### 14.3 Running Tests

```bash
# Full workspace
cargo test --workspace

# Specific crate
cargo test -p pms-core

# Specific test
cargo test -p pms-server --test security_http

# With output
cargo test --workspace -- --nocapture
```

### 14.4 Known Test Issues

- `admin_ping_requires_token` — admin token config mismatch in test setup
- `wallet_send_tx_injects_fee_and_admin_can_decrypt_fee_utxo` — fee calculation precision
- 2 tests in `wallet_send_fees.rs` marked `#[ignore]` — admin decrypt/scan logic

---

## 15. Deployment Guide

### 15.1 Configuration for Environments

| Setting | Dev | Testnet | Mainnet |
|---------|-----|---------|---------|
| `network.mode` | `Dev` | `Testnet` | `Mainnet` |
| `network.network_id` | `pms-dev` | `pms-test` | `pms-main` |
| TLS | Optional | Optional | **Required** |
| `auth.require_signed_submit` | `false` | `true` | `true` |
| `validation.enforce_single_writer` | `false` | `true` | `true` |
| `validation.min_pow_leading_zero_bits` | `0` | `0` | `0` |

### 15.2 TLS Setup

1. Generate or obtain TLS certificates (cert.pem + key.pem)
2. Configure in `config.toml`:
```toml
[tls]
cert_pem = "/path/to/cert.pem"
key_pem = "/path/to/key.pem"
ca_pem = "/path/to/ca.pem"    # Optional, for P2P client verification
```
3. In Mainnet mode, missing TLS files cause startup failure

### 15.3 Docker Deployment

The project includes Docker support:
```bash
# Build
docker build -t pms-node .

# Run with config mount
docker run -v /host/config:/etc/pms pms-node --config /etc/pms/config.prod.toml
```

Environment variables override config file values using `PMS__` prefix:
```bash
PMS__NETWORK__MODE=Mainnet
PMS__CLIENT__API_ADDR=0.0.0.0:8080
PMS__AUTH__ADMIN_API_TOKEN=env:ADMIN_TOKEN
```

### 15.4 Monitoring & Health Checks

| Endpoint | Purpose |
|----------|---------|
| `GET /livez` | Process is running (always 200) |
| `GET /healthz` | DB initialized and ready (200 when ready) |
| `GET /metrics` | Prometheus metrics (admin auth required) |

**Key Metrics:**
- `pms_blocks_total` — total blocks persisted
- `blocks_rejected` — rejected block count
- RocksDB internal stats (logged every 30 min)

**Logs:** Structured logging via `tracing` crate. Set `RUST_LOG=info` for standard output, `RUST_LOG=pms_perf=info` for performance timing.

---

## Appendix: Glossary

| Term | Definition |
|------|-----------|
| **Block** | A node in the DAG containing a payload and parent references |
| **DAG** | Directed Acyclic Graph — the data structure replacing a blockchain |
| **Tip** | A block with no children (the DAG frontier) |
| **UTXO** | Unspent Transaction Output — a discrete unit of value |
| **OutputId** | Reference to a specific UTXO: `(txid, index)` |
| **WireBlock** | Network serialization format for blocks |
| **StoredBlock** | RocksDB serialization format for blocks |
| **Coordinator** | The single trusted node that signs all blocks (Single Writer mode) |
| **Milestone** | A special block that finalizes all preceding blocks |
| **ShardedUtxoSet** | 256-shard concurrent UTXO cache in RAM |
| **ConcurrentDag** | Lock-free in-memory DAG using DashMap |
| **HRP** | Human-Readable Part of a Bech32m address (default: "8e") |
| **BPS** | Basis points (1 bps = 0.01%) |
