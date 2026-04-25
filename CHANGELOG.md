# Changelog

All notable changes to the PMS DAG will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.7.4] - 2026-04-25 — Production hardening sprint

### Added
- **feat(coordinator-key/rotate)**: Rotation in-band of the secp256k1 key that signs every coordinator block (audit item 8 of the production-sprint plan). Pre-0.7.4 the only way to change the key was to restart the network with a new bootstrap value — every block signed by the new key would be rejected by peers still on the old config. New `PlainPayload::CoordinatorKeyRotate { old_pk, new_pk, grace_window_seconds }` lets the operator hand authority over with a single signed block: validation in `do_persist_block_internal` accepts it only when signed by the **current** coordinator pk and `old_pk` matches that current pk (so a stolen-but-grace-window key can't keep authority by chaining a fresh rotation), then writes the rotation to a new RocksDB CF `coordinator_key_history` and refreshes the in-RAM cache. The single-writer signer check on every subsequent block consults the cache: signer is accepted if it's the new `current_pk` OR any rotation's `old_pk` whose grace window hasn't yet expired (overlap of consecutive grace windows is intentional — it widens tolerance during back-to-back rotations). Mint authority is narrower: it tracks `current_pk` only, so the rotation atomically transfers the right to mint to `new_pk` while old keys in grace can sign chain blocks but not new tokens. New trait `CoordinatorKeyStorage` (in `pms-storage`) with default empty impls so mock backends opt in transparently. New CLI `tools-cli rotate-coordinator-key --old-key <path> --new-key <path> --parent <block_id> [--grace 60] [--out file.json]` forges + signs the rotation block offline and prints the wire JSON; the operator submits it via the existing `POST /v1/submit/block` endpoint. Schema `CURRENT_VER` 9→10 (new CF). Smoke-tested end-to-end: two random keys, valid signed rotation block emitted with correct payload + signature. Unit tests cover the storage layer (round-trip, idempotence on the same block ID, grace boundary), and the in-memory `KeyRotationState` (empty fallback to bootstrap, current pk follows latest rotation, atomic-revocation grace=0). See [crates/pms-types-payload/src/payload.rs](crates/pms-types-payload/src/payload.rs), [crates/pms-storage/src/coordinator_key_store.rs](crates/pms-storage/src/coordinator_key_store.rs), [crates/pms-storage/src/rocks_store/coordinator_key_storage.rs](crates/pms-storage/src/rocks_store/coordinator_key_storage.rs), [crates/pms-core/src/core_adapter.rs](crates/pms-core/src/core_adapter.rs), [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs), [crates/tools-cli/src/main.rs](crates/tools-cli/src/main.rs).
- **test(chaos)**: New `crates/pms-server/tests/chaos_recovery.rs` — four `#[ignore]` disaster-recovery tests proving the persist pipeline holds its durability/atomicity contracts (audit item 7 of the production-sprint plan). `s1_durability_after_unclean_drop` writes 32 blocks via the single-block `append_block_atomic` path, drops the store, reopens, asserts every block is back. `s4_batch_atomicity_across_reopen` does the same for `append_blocks_batch` (64 blocks in one `WriteBatch`) — the contract is "all or nothing" across an unclean restart, this enforces it. `s3_corrupted_sst_fails_loud_or_recovers` writes 200 blocks, compacts to materialise SSTs, overwrites 64 bytes mid-file in the latest one, reopens — RocksDB must either recover gracefully or refuse to open with a diagnostic message, never silently accept corrupted data. `s5_pipeline_failure_surfaces_to_caller` spawns the background persist task against a storage that always fails, asserts the task drains its retry budget and shuts down, and that subsequent `tx.send().await` calls observe the closed channel — that's what `do_persist_block` translates into an HTTP error instead of a fake `Inserted`. Plan scenario S2 (disk-full) is documented as already covered by the existing `pms-core/tests/persist_no_silent_drops.rs`. New helper script `scripts/run-chaos-tests.sh` runs the suite in release mode with `--ignored --test-threads=1`.
- **feat(rocksdb/tips-rebuild)**: Curative reconciliation of the RocksDB `tips` CF (audit finding H3, item 6 of the production-sprint plan). The 0.7.3 fix to `trim_tips` only DELETES — it evicts zombies (entries with `children_count > 0`) but never adds anything. So a tip that's missing because of an older crash between `append_block_atomic` and `add_tip` stayed missing forever, leaving `top_tips()` to return the wrong set and silently blocking fee distribution. New helper `RocksStore::rebuild_tips_from_children_count(scan_limit)` walks the recent window via `by_time` (newest first), and adds to `tips` any block whose `children_count == 0` is missing. Idempotent: a second call is a no-op. Bounded scan: production callers pass `tip_limit × 8` (or whatever they trust). New admin endpoint `POST /admin/rebuild-tips` exposes the helper for ops intervention; defaults `scan_limit` to `tip_limit × 8` floored at 64 so a brand-new node still scans enough recent blocks to matter, body `{"scan_limit": 0}` opts into a full scan. Wired into `admin_auth_enforcement.rs` (now 37 routes covered). Four unit tests cover happy recovery, blocks-with-children skip, idempotence, and scan-limit honoring. See [crates/pms-storage/src/rocks_store/maintenance.rs](crates/pms-storage/src/rocks_store/maintenance.rs), [crates/pms-server/src/admin.rs](crates/pms-server/src/admin.rs).
- **docs(trust-model)**: New `documentation/trust-model.md` — answers in plain language "à qui dois-tu faire confiance, et pour quoi exactement ?" before launch (audit item 5 of the production-sprint plan). Three audiences: operators (SPOF, RTO/RPO, recovery procedures for VPS loss vs. coordinator-key loss), end users (a CGV-ready paragraph that names the trust assumption explicitly), and regulators (the can/cannot list, side-by-side comparison with Bitcoin and Ethereum). Cross-links wired to existing fiches via Obsidian wikilinks: [[server-engine]], [[features/wallet-encryption]], [[features/block-payloads]], [[features/storage-rocksdb]], [[features/compliance]], [[features/multi-ledger]]. README gets a "Modèle de Confiance" section linking the doc — readers see it before the API reference. MOC.md gets a new "Gouvernance & Sécurité" group so the trust model surfaces at the top of the index instead of being lost among 30+ feature fiches.
- **feat(metrics)**: Operator-facing Prometheus surface gained ten new signals so on-call has something to alert on instead of inspecting logs after the fact (audit item 4 of the production-sprint plan). Three counters declared in `pms-core` so the persist pipeline can drive them from the consumer/producer hot paths without a circular dep on `pms-server`: `pms_persist_retries_total` (every retry attempt in `background_persist`), `pms_persist_failures_total` (terminal failures after exhausting retries — pages immediately), `pms_persist_stall_seconds_total` (cumulative seconds back-pressured on `persist_tx.send().await`, fed by the existing 1-second warn ticker). Six gauges/counters declared in `pms-server` and driven by a new `spawn_metrics_sampler_task` that ticks every 5s: `pms_persist_queue_depth` and `pms_persist_queue_capacity` (per ledger, derived from `mpsc::Sender::{capacity, max_capacity}`), `pms_fee_pool_total` (per ledger, snapshot of `FeePool::total_fees`), `pms_utxo_set_size` (per ledger, from new `NetDagAdapter::utxo_set_size` accessor that hits `ShardedUtxoSet::total_len`), `pms_rocksdb_write_stalled_seconds_total` (sums sampler interval whenever the new `DagStorage::is_write_stopped` reads the canonical `rocksdb.is-write-stopped` property as `true`). Plus the per-output counter `pms_fees_distributed_total{ledger_id, recipient_type ∈ {burn_refund,treasury,node}}` wired into `perform_fee_distribution`. Smoke test `metrics_exposure.rs` asserts every name shows up in `metrics::render()` so a future rename / drop is caught at CI time. See [crates/pms-core/src/metrics.rs](crates/pms-core/src/metrics.rs), [crates/pms-server/src/metrics.rs](crates/pms-server/src/metrics.rs), [crates/pms-server/src/api/tasks.rs](crates/pms-server/src/api/tasks.rs).
- **feat(healthz)**: `/healthz` is no longer the boot-time `_ready` flag (audit finding H-healthz). It now runs four real probes and returns a structured JSON body — `rocksdb_writable` (cheap O(1) DB property), `persist_queue_depth` (current depth vs max capacity, configurable high-water threshold), `last_block_age` (timestamp of the newest persisted block via the new `DagStorage::block_ts_ms` accessor), `disk_free_percent` (libc `statvfs` on the RocksDB volume). HTTP code is `200` when every check passes, `503` with a per-check breakdown when one trips. `/livez` stays trivial (the Kubernetes liveness contract — "is the process answering at all"), so a transient persist-queue spike never restarts the pod. New `[health]` config block exposes `max_last_block_age_seconds`, `persist_queue_high_water`, `min_disk_free_percent` (sane defaults shipped). New trait method `NetDagAdapter::persist_queue_depth` returns `Some((used, capacity))` from the live tokio mpsc; mock adapters keep the default `None`. Three integration tests (`healthz_enriched.rs`) cover JSON shape, HTTP code ↔ aggregate consistency, and `/livez` triviality. See [crates/pms-server/src/api_fn/healthz.rs](crates/pms-server/src/api_fn/healthz.rs).

### Security
- **sec(coordinator-key/at-rest)**: The coordinator's ECDSA private key used to sit on disk as plain 64-hex (`/opt/pms/etc/pms/node.key`). A stolen offsite backup, an inadvertent log of `cat node.key`, or any process that read the file gave the attacker the key directly. v0.7.4 introduces an optional encrypted envelope `node.key.enc`: AES-256-GCM ciphertext keyed by an Argon2id-derived AES-256 key, salt and nonce per file, JSON wire shape so the format can be inspected and versioned. Plaintext is the raw 32 key bytes; AAD is `pms/coordinator-key/v1` to bind the ciphertext to its purpose. New module [`pms-wallet::key_encryption`](crates/pms-wallet/src/key_encryption.rs) implements `encrypt_key` / `decrypt_key` with passphrase zeroized at the boundary. New CLI `tools-cli encrypt-coordinator-key <plain.key> <out.enc>` produces the envelope (passphrase from env var `PMS_COORDINATOR_KEY_PASSPHRASE` or interactive prompt with double-confirm). The server (`bin/main.rs`) now prefers `[secrets].node_identity_key_encrypted_path` when configured AND the file exists, and reads the passphrase from `PMS_COORDINATOR_KEY_PASSPHRASE`; falls back transparently to the legacy plain-hex path so existing dev/testnet deployments boot unchanged. New helper `Wallet::check_key_file_permissions` warns at boot when the key file is group/world-readable, and aborts boot when `[secrets].strict_key_permissions = true`. 9 tests cover envelope round-trip, wrong passphrase, tampered ciphertext, unsupported version, JSON shape, end-to-end disk → `Wallet`, and mismatched-passphrase rejection. Audit finding H-key.

### Security
- **sec(admin-auth)**: Audit finding H-auth closed on five fronts. (A) The two admin middlewares (`require_local_or_admin`, `require_admin_token`) used to compare the admin token with `auth_str == format!("Bearer {token}")` — a direct string compare that can leak via timing. Both now delegate to `helper::is_admin_authorized`, which already runs `subtle::ConstantTimeEq` on the payload. (D) `api_fn::compliance::is_admin_authorized` was a divergent copy (wrong default "allow all on missing token", only checked `Authorization` not `X-Admin-Token`); removed and `use crate::helper::is_admin_authorized` wired in. (E) The global `CorsLayer` shifted from `allow_methods(Any)` / `allow_headers(Any)` to an explicit small set (`GET/POST/PUT/DELETE/OPTIONS`, `Authorization`/`Content-Type`/`Accept`/`X-Api-Key`/`X-Admin-Token`), and the `/admin/*` sub-router intentionally carries no CORS layer of its own — a browser refusing the preflight is an extra defense-in-depth barrier against CSRF targeting an operator with a stored admin token. (F) New `pms_admin_auth_failures_total{reason}` counter lets operators alert on brute-force / token-leak bursts (reasons: `ip_not_allowed`, `missing_token`, `wrong_token`). (C) New integration test `admin_auth_enforcement.rs` fires every known `/admin/*` route (36 at time of writing) from a non-loopback IP with no Authorization header, asserts 401/403 across the board — a regression trap if a future router refactor ever accidentally unplugs the middleware.
- **bump(version)**: Workspace version 0.7.3 → 0.7.4.

---

## [0.7.3] - Unreleased — Persist hot-path clone reduction + atomic encrypted UTXO delta + tips reconcile

### Fixed
- **fix(rocksdb/tips-consistency)**: `trim_tips` used to sort every entry in the `tips` CF by timestamp and evict the oldest over `tip_limit` — which let "zombie" tips (entries that had already gained a child, but whose `remove_tip` call was lost to a crash or a bug) survive because of their recent timestamp, while an actually-active older tip got evicted instead (audit finding H3). The CF would then drift out of sync with `ConcurrentDag::tips`, the real source of truth. Now `trim_tips` cross-checks every candidate against the `children_count` CF: any entry with `children_count > 0` is a zombie and is reclaimed first, then `tip_limit` enforcement runs over the *real* tips only. The fast-path (`estimate <= tip_limit`) is kept for steady-state performance — zombies simply clear on the next trim that actually runs. Three new unit tests lock the semantics. See [crates/pms-storage/src/rocks_store/maintenance.rs](crates/pms-storage/src/rocks_store/maintenance.rs).

### Security
- **sec(encrypted-tx/race)**: The pre-0.7.3 encrypted-TxUtxo flow was a two-step dance on the caller side (`persist_and_broadcast(wb)` then `apply_utxo_delta(inputs, outputs)`), leaving a window where the block was already visible in the RAM DAG and the persist pipeline while the `ShardedUtxoSet` still listed the inputs as spendable — a concurrent handler could re-select the same UTXOs and build a double-spending transaction (audit finding H1). Introduced `NetDagAdapter::persist_block_with_delta(wb, delta)` with a default non-atomic fallback; `CoreAdapter` overrides it so the externally-provided `UtxoDelta` is applied in the same critical section as the block insert (same step as plain payloads). New `tx_helpers::persist_and_broadcast_with_delta` wraps the common bookkeeping. Migrated both encrypted callers (`transaction::prepare_tx` and `wallet_factory::send_simple`). Sender lookup (`get_utxo(first_input)`) is now done BEFORE the persist call since inputs are consumed atomically on the return path. See [crates/pms-interface/src/net_adapter.rs](crates/pms-interface/src/net_adapter.rs), [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs), [crates/pms-server/src/api_fn/tx_helpers/block_ops.rs](crates/pms-server/src/api_fn/tx_helpers/block_ops.rs).

### Performance
- **perf(persist)**: Extracted `block_id` once in `do_persist_block` and moved both the `Block` (into `ConcurrentDag::insert_block`) and the `StoredBlock` (into `PersistJob`) by value instead of cloning them (audit finding H6). The `StoredBlock` clone was the most expensive — it carries `payload_json`, which can be 10-100 KB on encrypted blocks (`LedgerOwnershipTransfer`, wrapped DEK payloads). The reused `block_id` also folds 3 separate `block.id.clone()` calls in the finality path into a single pre-extracted `String`. Net effect at 10 K TPS: ~20 K avoided clones/s of the full `StoredBlock` and `Block` structs, several MB/s less memory pressure on the persist pipeline, no behavioural change. All 150+ branch tests still green. See [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs).

### Changed
- **bump(version)**: Workspace version 0.7.2 → 0.7.3.

---

## [0.7.2] - 2026-04-22 — Security audit sprint (C2, C3, M1, H5, M2, M3+M4, M5+H7, H4+M6)

### Fixed
- **fix(persist/critical)**: `do_persist_block` previously wrapped `persist_tx.send()` in a 5-second `tokio::time::timeout` and returned `PutResult::Inserted` to the caller even when the send timed out or the channel was closed — a silent data-loss bug. In a saturated persist pipeline (RocksDB stall, compaction pressure), the block was in RAM but never queued for disk, and the HTTP client saw a false "Inserted" acknowledgement. Replaced with an unbounded `send().await` that blocks until the queue has room (natural end-to-end back-pressure), emits periodic `tracing::error!` warnings every second while blocked, and returns `Err(anyhow!)` on a closed channel instead of fake success. See [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs).
- **fix(persist/critical)**: `background_persist_task` used to drop an entire batch on a single `append_blocks_batch` failure with only a `tracing::error!`. The task now retries with exponential backoff (100ms → 500ms → 2s → 5s → 15s → 30s) and, if all retries fail, shuts down the background loop so the channel closes and subsequent `send().await` calls return an error to the HTTP caller — no more false acknowledgements for blocks that never reached RocksDB. See [crates/pms-core/src/background_persist.rs](crates/pms-core/src/background_persist.rs).
- **fix(spent-tracking/preventive)**: `ConcurrentDag::is_spent()` only inspects the bounded FIFO RAM tracker, so any caller that trusted it alone would miss outpoints evicted once `max_spent_outpoints` was reached (audit finding C3). The method is now documented as best-effort RAM and a new authoritative helper `ConcurrentDag::is_outpoint_spent_authoritative(&store, txid, index)` consults RAM first and then falls back to the new `DagStorage::is_outpoint_spent` trait method. `RocksStore` implements the fallback by reading the on-disk `utxo_spent` column family — the single source of truth for double-spend detection across restarts and eviction. Today's production flow (`ShardedUtxoSet::get` with RocksDB fallback) was already safe; this change keeps any future caller from re-introducing the FIFO hole. See [crates/pms-core/src/concurrent_dag/spent.rs](crates/pms-core/src/concurrent_dag/spent.rs) and [crates/pms-storage/src/traits.rs](crates/pms-storage/src/traits.rs).
- **fix(wallet/security)**: `Wallet` used to `#[derive(Debug)]`, which meant a stray `dbg!(wallet)` or `println!("{wallet:?}")` would print the ECDSA private key and the full BIP-39 mnemonic (audit finding M1). `Debug` is now implemented manually and redacts both `private_key_b64` and `mnemonic_words` with length-only hints; public material (`public_key_hex`, `x25519_pub_hex`) is kept for debugging. New integration test `debug_redaction.rs` locks the invariant in. See [crates/pms-wallet/src/wallet.rs](crates/pms-wallet/src/wallet.rs).
- **fix(bridge/security)**: `admin_bridge_transfer` parsed `cross_ledger_fee_multiplier: f64` with `Decimal::from_f64_retain(...).unwrap_or(Decimal::from(2))`, so any NaN/±Inf/huge config value silently fell back to a hardcoded `2×` — users could be charged a mystery fee instead of the operator's intended multiplier (audit finding H5). Validation is now routed through a pure helper `validate_cross_ledger_multiplier(f64)` that explicitly rejects NaN, ±Inf, negative values, and values that overflow `Decimal`; rejected configs skip the fee with a `tracing::error!` so operators see their config is wrong. Multiplication uses `checked_mul` to avoid overflow surprises. Eight unit tests cover every edge case. See [crates/pms-server/src/api_fn/bridge.rs](crates/pms-server/src/api_fn/bridge.rs).

### Changed
- **bump(version)**: Workspace version 0.7.1 → 0.7.2.
- **deps(rand)**: Unified all workspace crates on `rand = "0.9.2"` (audit finding M2). Previously `pms-wallet` used 0.8.5, `pms-config` 0.8, and `pms-ledger` was pinned to the pre-release `0.10.0-rc.5` which dragged in `chacha20 = "0.10.0-rc.5"` (an unfinished AEAD crate) as a transitive dependency. Both RC packages are now out of the dependency graph. `pms-wallet/helpers.rs` migrated to the 0.9 API (`rand::distr::weighted::WeightedIndex`, `rand::rng()`). `pms-config` tests pin `rand_core = "0.6"` explicitly for the `CryptoRngCore` trait required by `k256 0.13` (transitive `rand_core 0.6.4` via `ecdsa 0.16.9` is independent of the `rand` version we use). The only remaining non-0.9 `rand` in `Cargo.lock` is `0.8.5` pulled by `nanoid 0.4.0`, used for non-cryptographic ID generation only.

### Security
- **sec(encrypted-payload/preventive)**: `EncryptedPayload`'s AES-GCM body AAD used to bind only `len_hint`, so an attacker holding the raw envelope outside a signed block context could rewrite the `recipients` list, flip a `kid`, or swap `ephem_pub` without invalidating the body tag (audit finding M4). `KEY_VERSION_CURRENT` bumped `1 → 2`; a new `aad.binding` field hashes `scheme`, `key_version`, `ephem_pub`, and the sorted recipient kids, then feeds that hash into the body AAD. Decryption recomputes the binding from the wire envelope and rejects mismatches before even touching the ciphertext. v1 envelopes (`aad.binding = None`, byte-identical wire format via `skip_serializing_if`) still decrypt cleanly so existing testnet blocks stay readable. Audit finding M3 (nonce "reuse" across recipients) was re-analysed and deemed a false positive — a single `(DEK, nonce)` pair encrypts the body once and the shared ciphertext is standard multi-recipient design, not nonce reuse. Seven new tests cover round-trip, backward-compat, and four negative paths (binding tampering, ephem_pub swap, recipient removal, kid substitution). See [crates/pms-types-payload/src/encrypted_payload.rs](crates/pms-types-payload/src/encrypted_payload.rs).

### Performance
- **perf(locks)**: Migrated every `std::sync::Mutex` / `std::sync::RwLock` on a hot path to `parking_lot` equivalents (audit findings M5, H7). Faster acquisition (smaller atomic footprint), no poisoning semantics, smaller lock objects (1 byte vs 16+). Affects `ConcurrentDag::{finality, insertion_order, spent_order}`, `ShardedUtxoSet::{supply_cache, Interner::set}`, `RocksStore::{top_tips_cache, runtime_config_cache}`, `Server::seen_invs`, `TpsTracker::timestamps`. Eliminated every `match lock() { Err(poisoned) => poisoned.into_inner() }` workaround (≈30 lines of defensive code). All 80+ tests on the branch stay green, including `pms-economics` (33 tests on `TpsTracker`).

### Security
- **sec(compliance/race)**: `admin_freeze` / `admin_unfreeze` used to run `is_frozen(addr)` and then forge+persist the Freeze/Unfreeze block without any locking. Two admin requests for the same address could both observe `is_frozen == false`, both proceed, and both end up signed & persisted — producing two distinct audit trails for a single real state transition (audit finding M6). Added `AppState::compliance_lock: Arc<tokio::sync::Mutex<()>>`, acquired at the top of both handlers so the check-then-persist sequence is atomic across concurrent admin requests. Contention is negligible (freezes are rare). New `compliance_lock_tests::concurrent_freeze_sections_are_serialised` asserts that 16 racing "requests" never observe more than one critical section active at a time. See [crates/pms-server/src/api_fn/compliance.rs](crates/pms-server/src/api_fn/compliance.rs).

### Infrastructure
- **ops(fees/treasury)**: Misconfigured treasury fees (`treasury_fee_percent > 0` with no `treasury_addresses` configured anywhere) used to redirect the cut to the node pool with a single quiet `warn!` (audit finding H4). New `fee_distribution::validate_treasury_config` is called at boot and emits a loud `tracing::error!` surfacing exactly what to fix; the runtime fallback now also logs at `error!` instead of `warn!` so the misconfig can't hide in log noise. Funds are never lost — the cut is still safely retained in the node pool if the error slips through — but operators will see the problem immediately. Five new unit tests lock every branch of the validator. See [crates/pms-server/src/fee_distribution/mod.rs](crates/pms-server/src/fee_distribution/mod.rs) and [crates/pms-server/src/fee_distribution/distribute.rs](crates/pms-server/src/fee_distribution/distribute.rs).

---

## [0.7.1] - 2026-03-24 — Fix: OOM crash loop + software_version endpoint

### Fixed
- **fix(rocksdb/critical)**: Engine OOM crash loop on testnet VPS (12 restarts in 24h, exit code 137). Root cause: with 2 ledgers (67 column families) and 20M blocks in DB, RocksDB compaction memory spikes exceeded the 14 GiB Docker limit. Aggressive tuning: `write_buffer_size_mb` 32→16, `max_write_buffer_number` 3→2, `block_cache_size_mb` 512→256, `db_write_buffer_size_mb` 512→256, `max_dag_blocks` 50K→10K. New memtable budget: 2.1 GiB (67 × 2 × 16 MB). Memory follows saw-tooth pattern: trough ~6-7 GiB, peak ~12.4 GiB during compaction. Stable at ~20 tx/s + game activity.
- **fix(persist/critical)**: Background persist channel used `try_send()` which **silently dropped blocks** when the 10K buffer was full — a data loss bug in a financial system. Replaced with `send().await` + 5-second timeout: API callers now block (natural back-pressure) instead of losing data. Timeout prevents indefinite blocking if RocksDB stalls. Both channel-closed and timeout scenarios log errors.
- **fix(api)**: `GET /v1/version` returned `software_version: "0.1.0"` instead of the actual binary version. Root cause: `env!("CARGO_PKG_VERSION")` in `pms-server` crate read that crate's own Cargo.toml version (0.1.0), not the binary version. Fix: introduced `[workspace.package] version` in root Cargo.toml, inherited by `bin` and `pms-server` via `version.workspace = true`. Now all report the correct version.

### Performance
- **perf(persist)**: Reduced background persist channel buffer from 10K to 2K blocks. With back-pressure enabled, the large buffer only consumed RAM without benefit. 2K blocks × ~1 KB = ~2 MB vs ~10 MB, with 31 batches of headroom at MAX_BATCH_SIZE=64.

### Changed
- **refactor(versioning)**: Software version now defined once in `Cargo.toml` workspace (`[workspace.package] version = "0.7.1"`), inherited by `bin` and `pms-server`. Bumping version requires changing only the workspace root.
- **bump(version)**: Software version 0.7.0 → 0.7.1.

### Infrastructure
- **ops(testnet)**: Updated `config.testnet.toml` with aggressive memory tuning for 16 GB VPS: `max_write_buffer_number` 3→2, `block_cache_size_mb` 512→256, `db_write_buffer_size_mb` 512→256, `max_dag_blocks` 50K→10K, `max_utxos` 2M→500K. Memory sizing table updated in CLAUDE.md.
- **ops(testnet)**: Reduced simulator `agents_testnet.toml` from ~1,800 tx/s (original) to ~20 tx/s (30 agents). 16 GB VPS can sustain ~20 PMS tx/s + game activity with saw-tooth compaction pattern staying under 14 GiB limit.

---

## [0.7.0] - 2026-03-24 — Cube Obfuscation, Rarity System & Calibrated Economics

_See git log for full details (commit bf33f90)._

---

## [0.6.8] - 2026-03-23 — Smart Contract Simulation & Sandbox Mode

### Added
- **feat(contracts)**: `POST /admin/contracts/simulate` dry-run endpoint. Accepts a candidate contract + a simulated event (`NftBurn`, `Transfer`, `TokenBurn`), evaluates the contract against an ephemeral in-memory store (existing contracts + candidate), and returns `SimulationResult` with `matched`, `match_reason`, `burn_results`, `transfer_fee_results`, `warnings`, and `existing_contract_matches`. No state is persisted — pure dry-run. Protected by `require_local_or_admin`.
- **feat(contracts)**: Sandbox mode — contracts now default to `enabled: false` on registration. The `RegisterContractRequest` accepts an optional `enabled` field (`#[serde(default)]`). Pass `"enabled": true` to activate immediately, or use `POST /admin/contracts/{id}/toggle` to activate later. Backward-compatible: existing clients passing no `enabled` field get `false`.
- **feat(contracts)**: New simulation types in `pms-contracts`: `SimulationEvent` (enum: `NftBurn`, `Transfer`, `TokenBurn`), `SimulationResult`, `ExistingContractMatch`. All derive `Serialize`/`Deserialize` for JSON API.
- **feat(contracts)**: `simulate_contract()` function in `pms-contracts::engine` — builds ephemeral `InMemoryContractStore`, force-enables candidate, evaluates against event, filters results, detects existing contract matches.
- **test(contracts)**: 8 new unit tests for simulation engine: transfer fee, NFT burn, existing contracts detection, scope mismatch, trigger mismatch, token burn warning, attribute formula, disabled candidate force-evaluation.
- **test(contracts)**: Integration test `test_contract_simulate_endpoint` — full lifecycle: simulate → verify no persistence → register (enabled=false) → toggle → verify enabled.
- **test(contracts)**: Integration test `test_contract_registration_concurrent` — 10 concurrent registrations + 5 concurrent reads, verifies all 10 succeed with `enabled: false`.

### Changed
- **breaking(contracts)**: Contracts now default to `enabled: false` on registration (was `true`). Existing API consumers must pass `"enabled": true` in the registration body to activate immediately.
- **bump(api)**: `API_VERSION` 8 → 9 (new simulate endpoint + sandbox mode default change).
- **bump(version)**: Software version 0.6.7 → 0.6.8.

---

## [0.6.7] - 2026-03-23 — Performance: O(1) token balance, activity backfill, UTXO consolidation

### Performance
- **perf(utxo/critical)**: Token balance queries are now **O(1)** instead of O(k) shard scan. Added `token_balance_cache: DashMap<(Arc<str>, Arc<str>), Decimal>` to `ShardedUtxoSet`, mirroring the existing `native_balance_cache` pattern but keyed by `(address, asset_id)`. Maintained incrementally in `supply_add_compact()` / `supply_sub_compact()` with zero-balance cleanup. Eliminates shard locks + RocksDB fallback for token balance lookups (EDN, custom assets).
- **perf(config)**: Increased `max_utxos` default from 500K to 2M (~64 MB RAM). 500K was too small for production DAGs with millions of UTXOs, causing excessive LRU eviction + RocksDB fallback. 2M covers most production deployments.
- **perf(config)**: Increased `block_cache_size_mb` default from 512 to 1024. With Direct I/O (v0.5.21), RocksDB block cache is the ONLY read cache — 1 GB is the production minimum.

### Added
- **feat(activity)**: Auto activity items backfill on startup. New config flag `rocks.auto_reindex_activity_items` (default: `true`). On startup, waits 30s then calls `reindex_all_activity_items()` in a background `spawn_blocking` task. Ensures 100% of blocks have pre-computed activity items for fast-path queries (1-2ms instead of 10-50ms fallback). Idempotent, safe to run on every restart.
- **feat(admin)**: UTXO consolidation endpoint `POST /admin/consolidate-utxos`. Merges multiple coordinator UTXOs into a single UTXO via self-transfer. Accepts `asset_id` (optional) and `max_inputs` (2-256, default 64). Returns `{ block_id, consolidated_inputs, new_utxo_amount, fee }`. Protected by `require_local_or_admin`. Solves coordinator UTXO proliferation from fee distribution (7200+ UTXOs/hour at 2000 TPS).
- **fix(consolidation)**: Fee output in consolidation endpoint now uses the same `asset_id` as the consolidated inputs. Without this, consolidating custom tokens (EDN) would produce a fee output in PMS, violating UTXO conservation (sum inputs != sum outputs).

---

## [0.6.6] - 2026-03-23 — Fix: Simulator Eden ledger owner keys missing

### Fixed
- **fix(simulator/critical)**: Simulator created Eden ledger without valid `owner_pubkey`/`owner_x25519_pubkey`, preventing fee distribution to creator. Root cause: `GameEngine::setup()` passed `coordinator_address` (bech32m address string) instead of the actual public keys (hex). Solution:
  1. Added `derive_public_keys_from_privkey_hex()` to derive ECDSA k256 + X25519 public keys from the coordinator's secp256k1 private key (same logic as `pms-wallet`).
  2. Modified `GameEngine::setup()` to accept `coordinator_private_key_hex` instead of `coordinator_address`, derive both keys, and pass them as `owner_pubkey`/`owner_x25519_pubkey` when creating the Eden ledger.
  3. Added `derive_address_from_keys()` helper to compute bech32m address for the transfer fee contract.
- **dependencies(simulator)**: Added crypto deps for key derivation: `k256`, `bech32`, `sha2`, `hkdf`, `x25519-dalek` (with `static_secrets` feature).

---

## [0.6.5] - 2026-03-23 — Fix: Custom ledger fees not distributed to owner

### Fixed
- **fix(fees/critical)**: Custom ledger owners (e.g., Eden creator) were not receiving accumulated transaction fees. Root cause: `accumulate_tx_fee()` always credited fees to the coordinator's `node_pk`, but for custom ledgers, fees should be credited to the ledger owner instead. The NodeRegistry was empty, so fees fell back to Treasury. **Solutions**:
  1. Modified `accumulate_tx_fee()` to detect custom ledgers (`ledger_id != "main"`) and credit fees to `ledger.owner_pubkey` instead of `coordinator.node_pk`.
  2. Added auto-registration of custom ledger owners in the NodeRegistry at startup. The owner's wallet address is derived from their Ed25519 + X25519 public keys (same bech32m logic as `Wallet::get_address()`). This enables the periodic fee distribution task to map `owner_pubkey` → `wallet_address` and distribute accumulated fees correctly.
- **fix(fees)**: Added `bech32 = "0.8.1"` dependency to `pms-server` for deriving wallet addresses from public keys in `derive_address_from_keys()` helper.

---

## [0.6.4] - 2026-03-23 — Fix: UTXO double-count destroying supply & address index

### Fixed
- **fix(utxo/critical)**: All plain-payload handlers (`faucet_mint`, `admin_seize`, `admin_reverse`, `create_reward_block`, `perform_fee_distribution`, `perform_daily_inflation_mint`) called `apply_utxo_delta()` or `add_utxo()` AFTER `persist_block()` for plain payloads. Since `persist_block()` already constructs and applies the `UtxoDelta` via `apply_diff()` for plain payloads, the second call caused:
  1. **Supply double-counting**: `supply_add_compact()` called twice per UTXO → supply inflated 2x.
  2. **Address index destruction**: LRU `push()` on existing OutputId evicted the existing entry, then `addr_index_remove(evicted)` deleted the entry that `addr_index_add` just re-added → UTXOs invisible to address queries.
  - Affected: faucet minting, compliance (seize/reverse), fee distribution, inflation minting.
  - Symptom on VPS: simulator agents failed to refuel ("no UTXOs found for address") despite successful faucet calls.

### Infrastructure
- **infra(docker)**: Fixed `Dockerfile.testnet` for `bindgen 0.72+` which no longer uses `clang-sys/runtime` (dlopen). Build scripts now link statically against LLVM. Added `zlib-static`, `llvm18-static`, `ncurses-static` packages and `libstdc++.a` symlink.

---

## [0.6.3] - 2026-03-22 — Fix: OOM crash loop on VPS testnet

### Fixed
- **fix(memory/critical)**: Engine OOM crash loop on 16 GB VPS (RestartCount: 48+, exit code 137). Root cause: glibc ptmalloc2 per-thread arena fragmentation under high-throughput multi-threaded RocksDB workloads caused RSS to grow ~4 GiB/min at 400 TPS. Solution: **jemalloc global allocator** via `tikv-jemallocator` — returns freed pages to OS aggressively, eliminates fragmentation-induced memory bloat.
- **fix(memory)**: `address_index` (DashMap<String, DashSet<OutputId>>) in `ShardedUtxoSet` grew without bound. When LRU cache evicted UTXOs, their `address_index` entries were never cleaned up. Fixed: `add()` and `apply_diff()` now use `push()` instead of `put()` to capture evicted entries and remove them from `address_index`.
- **fix(memory)**: `native_balance_cache` (DashMap<String, Decimal>) never removed zero-balance entries. Fixed: `supply_sub_compact()` now removes entries when balance reaches zero.

### Changed
- **config(testnet)**: Reduced RocksDB memory settings for 16 GB VPS — `write_buffer_size_mb` 32→16, `block_cache_size_mb` 1024→256, `db_write_buffer_size_mb` 512→256, `max_utxos` 2M→250K.
- **config(testnet)**: Reduced simulator TPS from ~2000 to ~300 (snipers 10→3, fast traders 20→8, reduced sends_per_tick across agents).

### Infrastructure
- **infra(docker)**: Overhauled `Dockerfile.testnet` for jemalloc + RocksDB on Alpine/musl:
  - Added `clang18-dev`, `g++`, `linux-headers` build deps for RocksDB C++ compilation + bindgen.
  - Set `RUSTFLAGS="-C target-feature=-crt-static"` — dynamically links musl so build scripts can `dlopen(libclang.so)` (static musl blocks `dlopen`, breaking bindgen's runtime linking).
  - Added `libstdc++`, `libgcc` to runtime Alpine image for dynamically-linked binary.
  - Runtime container uses `user: "1000:1000"` (matching host `pms` user) instead of nonroot UID 65532.

---

## [0.6.2] - 2026-03-22 — Hardening: production-readiness quick wins

### Fixed
- **fix(storage)**: 7x `RwLock::unwrap()` in `InMemoryContractStore` replaced with poison-safe `unwrap_or_else(|p| p.into_inner())` pattern. Prevents cascading panics if a thread panics while holding the contract store lock.
- **fix(validations)**: Removed misleading `// TODO (MVP rapide: stub "Ok(())")` comment on `verify_tx_signatures()` in `check.rs`. Signature verification is fully implemented in `signature.rs` — the outdated comment was dangerous for auditors.
- **fix(tests)**: Re-enabled 2 previously `#[ignore]`'d admin wallet fee tests (`wallet_send_tx_fee_is_materialized_and_zeroed_and_visible_to_admin`, `wallet_send_tx_does_not_duplicate_fee_output_if_already_present`). Root cause: missing change output and manual UTXO persistence in test setup. All 3 wallet fee tests now pass.

### Added
- **feat(metrics)**: Added `pms_api_request_duration_seconds` Prometheus histogram with method/route labels. Buckets: 1ms to 5s. Uses Axum `MatchedPath` for route templates, preventing label cardinality explosion from dynamic URL segments.
- **feat(middleware)**: Added `track_latency` middleware in the global Axum layer stack, recording request duration for all API endpoints.

### Changed
- **config(testnet)**: Reduced `rate_limit_rps` from 50000→10000 and `burst` from 100000→20000 in `config.testnet.toml`. Still 5x the simulator's peak throughput (2000 TPS) but provides basic DoS protection on publicly exposed testnet.

---

## [0.6.1] - 2026-03-21 — Fix: fd exhaustion crash + metrics counter reset + dashboard TPS

### Performance
- **perf(dashboard)**: TPS chart was updating every ~10 seconds instead of every 1 second under high block load. Root cause: `fetchData()` bundled 6 API calls (`/metrics`, `/v1/supply`, `/v1/nodes`, `/v1/peers`, `/admin/ping`, `/v1/tokens`) in a single `Promise.all()` with 2s interval. When the engine was under load, slow endpoints (`/v1/supply`) blocked the lightweight `/metrics` call. Fix: split into two independent polling loops — fast metrics (1s, only `/metrics`) and slow data (5s, supply/nodes/peers/tokens). Added `metricsFetching` guard to prevent overlapping metrics calls. Relaxed `deltaTime < 10` guard to `< 30` so TPS data isn't discarded after brief network hiccups.

### Fixed
- **fix(server/critical)**: Engine crashed silently every ~2-2.5 hours with ExitCode 0 due to **file descriptor exhaustion**. Root cause: Docker default `ulimit -n = 1024` combined with `max_open_files = 1024` in RocksDB config, leaving zero fd headroom for TCP accept, logging, or other I/O. 113K+ "Too many open files" errors occurred before each crash. `listen_tls()` / `listen()` used `?` on `accept()`, causing a single fd error to kill the entire P2P listener and propagate through `srv.run()` to `main()`, which always returned `Ok(())` (exit code 0) regardless of error.
- **fix(server/critical)**: P2P listener `accept()` errors now use retry-with-backoff instead of `?` propagation. A transient OS error (fd exhaustion, EMFILE) no longer kills the server — the listener logs the error and retries after 1s, recovering automatically once fds are freed.
- **fix(main)**: `main()` now calls `std::process::exit(1)` when `srv.run()` returns (either Ok or Err), using `eprintln!` (unbuffered) to guarantee the error message is visible even when tracing can't write due to fd exhaustion. Previously, `main()` always returned `Ok(())` → exit code 0 → Docker reported "clean exit" → misleading diagnostics.
- **fix(metrics/critical)**: `pms_blocks_persisted_total` counter was never initialized for dynamic ledgers (e.g. eden) restored via `load_persisted_ledgers()`. After every engine restart, the dashboard showed a misleading gap (e.g. "DAG Size: 50K" vs "Blocks Persisted: 0") because the Prometheus counter started at 0 while `pms_blocks_total` loaded 50K blocks from RocksDB. Now all dynamic ledgers get their counter seeded from `block_count_estimate()` at startup.
- **fix(persist)**: `try_send()` in the background persist pipeline silently dropped blocks when the channel buffer (10K) was full. Added warning log with drop counter so operators can detect backpressure-induced data loss. Previously, blocks could be lost without any trace in logs.

### Added
- **feat(boot)**: Startup now reads `/proc/self/limits` and logs the fd limit. Warns if `ulimit -n < 8192` with instructions to set Docker ulimits.

### Infrastructure
- **infra(docker)**: `docker-compose.testnet.yml` now sets `ulimits: nofile: { soft: 65536, hard: 65536 }` for the engine container.
- **infra(config)**: `config.testnet.toml` `max_open_files` increased from 1024 to 4096, leaving ample headroom within the new 65536 fd limit.

---

## [0.6.0] - 2026-03-21 — Major structural refactoring: module splits + dead code removal

### Changed
- **refactor(storage)**: Split `rocks_store/store.rs` (2,426 lines) into 5 sub-modules: `activity_index.rs`, `dag_storage_impl.rs`, `maintenance.rs`, `secondary.rs`, and a trimmed `store.rs`. All `pub use` re-exports preserved.
- **refactor(server)**: Split `server.rs` (1,465 lines) into 6 sub-modules: `mod.rs`, `broadcast.rs`, `listener.rs`, `peer.rs`, `sync.rs`, `blocks.rs`.
- **refactor(server)**: Split `api.rs` (1,197 lines) into 7 sub-modules: `mod.rs`, `state.rs`, `middleware.rs`, `routes.rs`, `serve.rs`, `ledger_dispatch.rs`, `tasks.rs`.
- **refactor(server)**: Split `api_fn/activity.rs` (2,334 lines) into 6 sub-modules: `mod.rs`, `cache.rs`, `handler.rs`, `stream.rs`, `classify.rs`, `tests.rs`.
- **refactor(server)**: Split `fee_distribution.rs` (961 lines) into 5 sub-modules: `mod.rs`, `compute.rs`, `distribute.rs`, `inflation.rs`, `tests.rs`.
- **refactor(server)**: Split `api_fn/tx_helpers.rs` (896 lines) into 6 sub-modules: `mod.rs`, `fee_policy.rs`, `coin_selection.rs`, `block_ops.rs`, `fee_accumulation.rs`, `tests.rs`.
- **refactor(core)**: Split `concurrent_dag.rs` (1,829 lines) into 9 sub-modules: `mod.rs`, `core.rs`, `tips.rs`, `spent.rs`, `pruning.rs`, `finality.rs`, `bootstrap.rs`, `forge.rs`, `tests.rs`.
- **refactor(core)**: Split `net_adapter.rs` (1,282 lines) into 6 sub-modules: `mod.rs`, `persist.rs`, `query.rs`, `supply.rs`, `utxo.rs`, `helpers.rs`. Uses delegate-to-helper pattern (Rust constraint: single `impl Trait for Type` per file).
- **refactor(storage)**: Split `helpers.rs` (923 lines) into 5 sub-modules: `mod.rs`, `encoding.rs`, `time_index.rs`, `activity_keys.rs`, `classify.rs`, `tests.rs`.
- **refactor(config)**: Coordinator public key constants (`COORDINATOR_PUBLIC_KEY_MAINNET`, `COORDINATOR_PUBLIC_KEY_TESTNET`) moved from dead `pms-consensus` crate to `pms-config`. Added `NetworkMode::coordinator_public_key()` helper.

### Removed
- **remove(crate)**: Deleted `pms-crypto` — dead code (dummy `add()` function, empty `ed25519.rs`). Zero dependents.
- **remove(crate)**: Deleted `pms-consensus` — dead code (2 unused coordinator key constants, now in `pms-config`). Zero runtime dependents.

### Infrastructure
- **infra**: All 10 file splits preserve public API via `pub use` re-exports. Zero breaking changes.
- **infra**: 8 clippy warnings fixed (empty line after doc comment in server sub-modules).
- **infra**: Activity test imports fixed after module split (`classify::*` explicit import in tests.rs).

---

## [0.5.22] - 2026-03-21 — Memtable OOM fix: multi-ledger memory scaling

### Fixed
- **fix(storage/critical)**: RocksDB memtable OOM from multi-ledger CF explosion. With 2 ledgers (66 CFs), `write_buffer_size_mb=128 × max_write_buffer_number=6 × 66 CFs = 50 GB theoretical max` — heap reached 12 GB (confirmed via `/proc/1/smaps_rollup`) within hours, triggering OOM kills (4 restarts). Fixed by reducing `write_buffer_size_mb` default from 128 to 32 and `max_write_buffer_number` from 6 to 3. New worst-case: `66 × 3 × 32 = 6.3 GB` memtables — safe within 14 GB Docker limit.
- **fix(storage)**: Corrected `db_write_buffer_size_mb` documentation — it is a **flush trigger**, NOT a hard memory cap. Immutable memtables waiting for flush still consume RAM beyond this limit. This misunderstanding was a contributing factor to the v0.5.21 OOM.

### Changed
- **change(config/testnet)**: `write_buffer_size_mb` reduced 128 → 32, `max_write_buffer_number` reduced 6 → 3, `db_write_buffer_size_mb` reduced 1024 → 512. VPS sizing guide rewritten with multi-ledger CF count warnings.
- **change(storage)**: `RocksMemoryConfig` default `write_buffer_size_mb` reduced 128 → 32 for multi-ledger safety. Doc-comments updated with memory scaling formula.

---

## [0.5.21] - 2026-03-21 — Direct I/O: eliminate Docker OOM crashes

### Performance
- **perf(storage/critical)**: RocksDB Direct I/O enabled (`set_use_direct_reads`, `set_use_direct_io_for_flush_and_compaction`). Bypasses kernel page cache entirely, eliminating 4-10 GB of cgroup-accounted memory that caused Docker OOM kills within hours of sustained operation. All SST reads now go exclusively through RocksDB's own block cache. Root cause fix for production crashes: Linux counts page cache towards cgroup `mem_limit`, so Application RSS (2-3 GB) + page cache (4-10 GB) exceeded the 14 GB Docker limit.
- **perf(storage)**: `block_cache_size_mb` default increased 512 → 1024 MB. With Direct I/O, the block cache is the ONLY read cache (kernel page cache bypassed). Larger cache ensures index/filter blocks + hot data blocks remain in RAM.

### Changed
- **change(storage)**: `advise_random_on_open(true)` removed from both `cf_opts_with_bloom()` functions. Direct I/O makes fadvise hints irrelevant — the kernel page cache is no longer used at all.
- **change(config/testnet)**: `block_cache_size_mb` increased 512 → 1024 MB. Memory sizing guide updated for Direct I/O era (cache column doubled in VPS sizing table).

---

## [0.5.20] - 2026-03-21 — RocksDB write stall elimination: sustained high-TPS tuning

### Performance
- **perf(storage/critical)**: RocksDB write stall prevention v2. L0 thresholds doubled again (40/56 → 80/120), pipelined writes enabled (`set_enable_pipelined_write`), background jobs scaled to CPU core count (min 8), sub-compactions increased (3 → 4), memtable merge before flush (`min_write_buffer_number_to_merge = 2`). Eliminates the periodic 20-60 blk/s stalls observed in 75-minute VPS monitoring.
- **perf(storage/critical)**: Multi-block WriteBatch in background persist. New `DagStorage::append_blocks_batch()` batches up to 64 blocks into a single `WriteBatch` + `db.write()` call. Reduces WAL appends and DB mutex acquisitions by up to 64x. RocksStore implementation uses `multi_get_cf` for batch dedup check and single atomic write. Default trait impl falls back to per-block writes for non-RocksDB backends.

### Added
- **feat(storage)**: `DagStorage::append_blocks_batch()` — batch block persistence with default per-block fallback.
- **feat(storage)**: `RocksStore::append_blocks_batch()` — optimized single-WriteBatch implementation for multi-block persistence.

### Changed
- **change(config/testnet)**: `max_utxos` increased 250K → 2M. With simulator generating 2M+ UTXOs, the 250K LRU cache had 87.5% miss rate — every coin selection triggered ~900 RocksDB reads. Memory cost: ~400 MB (acceptable on 16 GB VPS).
- **change(config/testnet)**: `max_write_buffer_number` increased 3 → 6. More memtable buffering before flush stalls under sustained write pressure.

---

## [0.5.19] - 2026-03-20 — Sustained TPS degradation elimination: 4 performance fixes

### Performance
- **perf(server/critical)**: Coin selection O(N log N) → O(1) fast path. `select_utxos()` now collects at most 256 UTXOs via `utxos_for_selection()` with early-exit from the DashSet address index. For coordinator addresses with millions of fee reward UTXOs, this avoids cloning/sorting the entire set. Falls back to full scan + sort only when 256 UTXOs can't cover the target (rare). New `utxos_by_address_for_selection()` method on `ShardedUtxoSet` + `utxos_for_selection()` trait method on `NetDagAdapter`.
- **perf(server/critical)**: Eliminate coordinator UTXO proliferation. All 6 callers of `create_reward_block()` (wallet_send_simple, send_tx, token creation, NFT mint, contract deploy, bridge) now use `accumulate_tx_fee()` which pools fees for periodic consolidated distribution. At 2000 TPS with 10s distribution interval, coordinator UTXO creation drops from 7200/hour to ~360/hour (20x reduction). Root cause fix for sustained TPS degradation.
- **perf(core)**: Background persist batch draining. Consumer loop now drains up to 64 jobs per iteration via non-blocking `try_recv()`. Finality persistence is batched across all jobs in the batch, reducing individual `persist_final()` calls. Reduces channel pressure under high-TPS load.

### Fixed
- **fix(server/critical)**: FeePool race condition — atomic swap eliminates fee loss. `perform_fee_distribution()` previously used read-lock snapshot + later write-lock reset, losing fees accumulated between the two operations. Now uses `std::mem::replace` atomic swap (single write lock, ~1μs). Error recovery via `merge_from()` restores fees to pool on persist failure. Zero fee loss guaranteed.

### Added
- **feat(core)**: `FeePool::merge_from()` — merges another pool's data for error recovery after failed distribution.
- **feat(core)**: `ShardedUtxoSet::utxos_by_address_for_selection()` — early-exit UTXO collection with limit and asset filtering at compact level.
- **feat(interface)**: `NetDagAdapter::utxos_for_selection()` — trait method for limited coin selection with default fallback implementation.
- **feat(server)**: `accumulate_tx_fee()` helper in tx_helpers — unified fee accumulation for all transaction types.

### Changed
- **change(server)**: `create_reward_block()` deprecated in favor of `accumulate_tx_fee()`. Function body preserved for backward compatibility.
- **change(server)**: Fee distribution timing changed from immediate (per-TX Reward block) to periodic (consolidated via `spawn_fee_distributor_task`). Configurable via `distribution_interval_sec` (default 10s on testnet).

---

## [0.5.18] - 2026-03-20 — Fix supply endpoint EDN wallet balances + Eden TPS optimization + deploy resilience

### Added
- **feat(test)**: `test_sustained_tps_stress` — 5-minute sustained TPS stress test in DAG sandbox (80 workers, 300s). Each `send_simple` creates 2 blocks (TX + Reward). Measures TPS in 5s intervals with time-series report (TPS, block count, P50/P95/P99 latencies). Detects degradation by comparing first-minute vs last-minute avg TPS. **Proved: 13,362 avg TPS over 5 min, 4.0M TX, 8.02M blocks (26,712 blk/s), 0 failures, 4.6% degradation — EXCELLENT.** RocksDB `block_count_estimate()` severely undercounts under write pressure (reported 910K vs 8.02M actual) — test uses accurate `TX×2` calculation.

### Fixed
- **fix(infra)**: `deploy-testnet.sh` and `upgrade-testnet.sh` now survive Docker Compose ghost container errors. Root cause: Docker Compose v2 can desync with containerd, leaving phantom container references that cause `"No such container"` errors. Fix: (1) pre-cleanup via `docker compose rm -f -s` + project-label cleanup, (2) `|| true` on `docker compose up` (ghost errors don't abort the script), (3) per-service verification loop — if any service didn't start, it's retried individually. Also added `--remove-orphans` to all `docker compose up` calls.
- **fix(api/critical)**: `GET /v1/supply` wallet balances (`admin_balance`, `node_balance`, `treasury_balance`) always showed PMS native balance, even when `circulating_supply` auto-resolved to edenite on custom ledgers. Dashboard showed "2.36 EDN circulating" but "0 EDN" for all wallets. Root cause: all wallet balance calls used `balance_by_address()` (PMS native only) instead of `balance_by_address_and_asset()` with the resolved asset. Fix: when a custom token is resolved (auto-fallback or explicit `?asset_id=`), wallet balances now use `balance_by_address_and_asset(&addr, resolved_asset)`.

### Added
- **feat(simulator)**: `edn_sends_per_tick` config field for `AgentGameConfig`. Like PMS `sends_per_tick`, allows each agent to send N sequential EDN transfers per tick during Phase 2 instead of 1. Each send re-queries balance for accurate UTXO tracking. Break on error or balance below threshold. Default: 1 (backward compat).
- **feat(test)**: `test_supply_endpoint_edn_balances` — sandbox test validating the supply endpoint fix. Burns 5 cubes → distributes EDN refunds → sends EDN (triggering 5% transfer fee) → queries `/l/eden/v1/supply?asset_id=edenite` → asserts `admin_balance > 0` (was always "0" before the fix). Also validates auto-resolve behavior when PMS native exists on eden.
- **feat(test)**: `get_supply()` helper method on Sandbox struct — queries supply endpoint with optional ledger and asset_id parameters.

### Changed
- **change(simulator)**: Testnet `burn_cooldown_ticks` increased to align with `distribution_interval_sec`: users/miners 10→15, whales 10→5 (×5s=25s), savers 10→3 (×10s=30s). Ensures agents stay in cooldown long enough for `fee_distribution` (now 10s) to deliver EDN UTXOs before Phase 3 remint triggers. Previously, cooldown (10s) expired before distribution (30s) → Phase 2 (send EDN) almost never fired.
- **change(simulator)**: All game-enabled agent groups now have `edn_sends_per_tick` configured: users=5, miners=10, whales=3, savers=5. Estimated Eden TPS boost: ~3→~355 EDN sends/s.
- **change(config)**: Testnet `distribution_interval_sec` reduced 30→10s. Faster EDN UTXO delivery aligns with agent cooldown windows.

### Infrastructure
- **infra(config)**: `agents_testnet.toml` and `agents_dev.toml` updated with new `edn_sends_per_tick` and `burn_cooldown_ticks` values per agent group.

---

## [0.5.17] - 2026-03-19 — EDN lifecycle integration test + diagnostic logging

### Added
- **feat(test)**: `test_pms_throughput_benchmark` — sandbox benchmark proving PMS engine handles ~10,000 TPS (10 workers × 100 tx, 100% success rate). Demonstrates that simulator's ~200 PMS TPS is an agent config bottleneck (1 tx/tick/agent), not an engine limitation. Engine headroom: ~50x.
- **feat(simulator)**: `sends_per_tick` config field for `AgentBehavior::Random` and `Coordinator`. Allows each agent to send N sequential PMS transactions per tick instead of 1. Solves PMS TPS disparity vs Eden (200 vs 2000) by multiplying each agent's throughput. Each send is sequential within a wallet (UTXO chain dependency), but concurrent across agents.
- **feat(test)**: `test_edn_transfer_fee_flow` — comprehensive sandbox test validating the COMPLETE Edenite lifecycle: cube NFT burn → EDN refund (AccumulateRefund) → FeePool distribution → EDN UTXOs → send EDN → 5% TransferFee → coordinator receives EDN. Includes 7 assertions with conservation check (remaining + sent + fee = refund). Validates the exact flow the VPS simulator runs.
- **feat(test)**: Sandbox helpers: `get_utxos()`, `get_asset_balance()`, `register_contract()`, `mint_nft()`, `burn_nft_simple()`, `send_asset()` — reusable building blocks for future integration tests.
- **feat(simulator)**: Diagnostic logging for Phase 2 detection in `RandomAgent::game_tick()` — logs EDN balance checks (non-zero), query failures, and Phase 2 entry events. Helps diagnose why agents may never enter the EDN transfer phase on the VPS.
- **feat(simulator)**: Diagnostic logging in `GameEngine::send_edenite()` — pre-send (amount, recipient, asset) and post-send (block_id, gas_fee, transfer_fee) log lines.
- **feat(simulator)**: `transfer_fee` field added to `SendResponse` struct in `types.rs` for visibility into smart contract transfer fees.
- **feat(contracts)**: Debug logging in `evaluate_transfer()` — logs when no transfer contracts found and when evaluating contracts (count, amount, asset, ledger).

### Fixed
- **fix(test)**: `boot_sandbox()` restructured to load admin wallet FIRST, then write coordinator keys into config file BEFORE `LedgerManager::bootstrap()`. Critical fix: `CoreAdapter::new()` calls `load_config()` internally — programmatic settings modifications were ignored for coordinator key, causing NFT burn validation to reject coordinator-signed burns.

### Performance
- **perf(simulator)**: Lock-free game engine pattern — `GameEngine` write lock no longer held during HTTP calls. Previously, 65 agents competed for a single `RwLock<GameEngine>` write lock held for 1-2 seconds each during Phase 1 (burn) and Phase 3 (remint 80-120 cubes), serializing all game operations. New static methods (`execute_burn_batch()`, `generate_mint_specs()`, `execute_mints_parallel()`) run HTTP calls without any lock. Write lock held only for brief HashMap registry operations (`drain_cubes()`, `restore_cubes()`, `register_minted()`).
- **perf(simulator)**: Cached game client — `RandomAgent` caches `(DagClient, edenite_asset_id)` on first game_tick instead of acquiring a read lock every tick. Phase 2 (send EDN) and EDN balance queries now run entirely lock-free.

### Changed
- **change(simulator)**: `sends_per_tick` added to `AgentBehavior::Random` (default: 1). Agents now send N PMS transactions per tick in a sequential loop. Configured values: snipers=25 (1250 tx/s), fast=15 (480 tx/s), users=5 (60 tx/s), whales=3, savers=5. Theoretical testnet total: ~1793 PMS tx/s (was ~95 with sends_per_tick=1).
- **change(simulator)**: `burn_cooldown_ticks` added to `AgentGameConfig` (default: 10 ticks). After burning cubes, agents wait N ticks before reminting, giving `fee_distribution` time to deliver EDN UTXOs. Without this, agents cycled burn→remint faster than the 600s distribution interval, so Phase 2 (send EDN → trigger TransferFee) was almost never reached. During cooldown, agents check EDN balance each tick and enter Phase 2 immediately when EDN arrives.
- **change(simulator)**: All agent config files (`agents_dev.toml`, `agents_testnet.toml`) updated with `burn_cooldown_ticks = 10` and `sends_per_tick` tuned per agent group.

### Infrastructure
- **infra(docker)**: `docker-compose.testnet.yml` healthcheck timing adjusted: `interval=10s` (was 5s), `timeout=5s` (was 3s), `retries=10` (was 15), `start_period=300s` (was 120s). Prevents premature unhealthy status during initial bootstrap with large block counts.

---

## [0.5.16] - 2026-03-19 — Revert TPS regression + TPS logger + deploy automation + DAG sandbox

### Added
- **feat(server)**: TPS logger — periodic throughput recording for production diagnostics. Spawns a background task that writes a JSONL line every 10 minutes to `{data_dir}/tps_log.jsonl`. Each deployment gets a unique UUID so operators can distinguish restarts from sustained runs. Fields: `ts`, `epoch_ms`, `deployment_id`, `ledger`, `tps_60s`, `block_count`, `circulating_supply`, `total_burned`, `node_pk`, `uptime_min`.
- **feat(deploy)**: `deploy-testnet.sh` now supports `--yes`/`-y` non-interactive mode for CI/CD and AI-driven deploys. Auto-answers prompts with defaults, reuses admin token from backup, skips macOS file picker dialog. Backup saved to external drive (`/Volumes/.../pms-key/`) in auto mode.
- **feat(test)**: DAG Sandbox (`dag_sandbox.rs`) — production-like in-process PMS engine for integration tests. Boots full LedgerManager + EventBus + ContractListener + fee distribution on a random port with tempdir RocksDB. Reusable `boot_sandbox()` returns a `Sandbox` struct with helpers: `create_ledger()`, `deposit_gas_pool()`, `faucet_mint()`, `send_simple()`, `get_balance()`, `distribute_fees()`. First test: `test_coordinator_receives_eden_fees` verifies coordinator receives fee revenue from eden transactions via immediate Reward blocks.
- **feat(test)**: Local TPS Benchmark (`local_bench.rs`) — in-process benchmark simulating 6 vCores (VPS constraint). Achieves ~9,000 TPS locally vs 2,000 TPS on VPS. Dev mode config generation (no coordinator key enforcement).
- **change(api)**: `FeePoolRefundSink` made `pub` in `api.rs` for integration test wiring.

### Fixed
- **fix(server/critical)**: UTXOs endpoint (`GET /v1/wallet/{address}/utxos`) was missing `asset_id` field in the response. `UtxoFlatItem` dropped `asset_id` from `TxOutput` during conversion — clients filtering by asset always saw 0 balance (e.g., EDN). Added `asset_id: Option<String>` to `UtxoFlatItem`.
- **fix(simulator)**: `UtxoEntry` field names (`txid`/`index`) didn't match server's camelCase format (`txId`/`outIdx`). Deserialization failed silently → balance always 0. Added `#[serde(alias)]` to accept both formats.
- **fix(server)**: Activity cache eviction was broken — cache could grow unboundedly. Fixed: two-phase eviction.
- **fix(server)**: Node registry `cleanup_stale()` was defined but never called. Fixed: piggyback cleanup on `register()`.

### Reverted
- **revert(server/critical)**: Restored `add_utxo()`/`apply_utxo_delta()` calls after `persist_block()` in 6 code paths: `fee_distribution.rs` (Mint + Reward), `wallet_factory.rs` (faucet mint), `tx_helpers.rs` (per-tx reward), `token.rs` (token mint), `compliance.rs` (Seize + Reverse). Removing these caused the 22x TPS regression (2000→90). The `add_utxo` calls make UTXOs immediately available in RAM for subsequent transactions; without them, the system stalled waiting for `persist_block`'s async delta propagation.
- **revert(storage)**: Removed `use_direct_io_for_flush_and_compaction(true)` (does not help, made newly-flushed SSTs cold). Kept `advise_random_on_open(true)` — essential to prevent OOM on 8 GB Docker cgroups. The TPS=90 was misattributed to `advise_random`; the true cause was the `add_utxo` removals above.

### Performance
- **perf(server/critical)**: `internal_health()` endpoint called `all_block_ids()`, loading ALL block IDs into a `Vec<String>`. At 15M blocks, this allocated **~1.2 GB per call**. Replaced with `block_count_estimate()` (O(1), 0 bytes).
- **perf(main)**: `block_count()` at startup replaced with `block_count_estimate()` — eliminates a full RocksDB table scan (O(N) with N=15M) during boot.
- **perf(wallet)**: `gather_wallet_utxos_dec()` and `gather_address_utxos_dec()` no longer call `all_block_ids()` for large `scan_limit` values. Now always uses bounded `recent_ids(scan_limit)`.

### Changed
- **change(api)**: `API_VERSION` bumped 6 → 7 (UTXOs endpoint now includes `asset_id` field).

---

## [0.5.14] - 2026-03-18 — Fix custom ledger path routing + balance endpoint enhancement

### Fixed
- **fix(server/critical)**: All per-ledger endpoints with path parameters (`/l/{id}/v1/wallet/{address}/utxos`, `/v1/nft/{token_id}`, `/v1/blocks/{id}`, `/v1/wallet/{address}/nfts`, `/v1/wallet/{address}/activity`, `/v1/tokens/{asset_id}`) returned **500 Internal Server Error** on custom ledgers. Root cause: Axum's outer `Path` extraction (`ledger_id`, `rest`) leaked into the inner router via request extensions, causing handlers expecting 1 path param to receive 3. Fix: reset `parts.extensions` before forwarding to inner router. **Impact**: This bug silently broke the simulator's EDN balance queries — agents could never see their EDN → never triggered Phase 2 (EDN transfers) → TransferFee contract never fired → coordinator received 0 fees.

### Added
- **feat(api)**: `POST /v1/balance` now accepts optional `ledger_id` and `asset_id` fields. Clients can query any ledger's balance from the main endpoint (e.g., `{"address":"8e1...", "ledger_id":"eden", "asset_id":"edenite"}`). Response echoes back `ledger_id` and `asset_id` for clarity.
- **feat(interface)**: Added `balance_by_address_and_asset()` to `NetDagAdapter` trait — supports asset-filtered balance queries (PMS native O(1) cache, custom tokens via shard scan).

### Changed
- **change(api)**: `API_VERSION` bumped 5 → 6 (new balance endpoint fields, path routing fix).

---

## [0.5.13] - 2026-03-18 — Fix bootstrap OOM: chronological loading + ghost cleanup

### Performance
- **perf(core/critical)**: Bootstrap now loads blocks **chronologically** via `by_time` CF reverse iterator instead of lexicographically. Block IDs are hashes, so lexicographic "newest N" selects random blocks — causing 99.8% orphan tips (49,904/50,000 on Eden with 13.2M blocks). Chronological loading preserves parent-child locality, reducing orphans to ~2-5%.
- **perf(ledger/critical)**: Eliminated ~1.16 GB allocation in `LedgerInstance::bootstrap()`. `all_block_ids()` loaded all 13.2M IDs into a Vec just to check `.is_empty()`. Replaced with `is_empty()` — a single RocksDB iterator seek (O(1), 0 bytes).
- **perf(core)**: `block_count_estimate()` uses RocksDB `estimate-num-keys` property (O(1)) instead of full table scan for diagnostic logging.
- **perf(core)**: Ghost entry cleanup after bootstrap. Parent IDs referenced by loaded blocks but outside the loaded window created phantom entries in `children_count`/`children_idx` DashMaps (~100-800 MB). `cleanup_ghost_entries()` removes them in a single pass.
- **perf(server)**: `get_block_parents()` fallback in `tx_helpers.rs` replaced `all_block_ids()` (full scan) with `recent_ids(1)` (O(1)).

### Added
- **feat(storage)**: 3 new `DagStorage` trait methods with optimized `RocksStore` overrides:
  - `is_empty()` — O(1) single iterator seek on `idx_blocks` CF
  - `newest_block_ids_by_time(n)` — reverse iterator on `by_time` CF for chronological ordering, with fallback to lexicographic when `by_time` is empty (e.g. after `import_json`)
  - `block_count_estimate()` — O(1) via RocksDB `rocksdb.estimate-num-keys` property
- **feat(core)**: `ConcurrentDag::cleanup_ghost_entries()` — post-bootstrap pass that removes DashMap entries for parent block IDs not in the loaded block set.

### Changed
- **change(core)**: Bootstrap `insertion_order` now uses the loading order directly (oldest-first from chronological iterator) instead of re-sorting lexicographically. This means `prune_oldest()` evicts truly oldest blocks first.
- **change(core)**: Bootstrap diagnostic log changed from `warn` to `info` level and reports "post-ghost-cleanup" counts.

---

## [0.5.12] - 2026-03-18 — Fix PMS fee bootstrap deadlock on custom ledgers

### Fixed
- **fix(server/critical)**: Custom asset transfers (e.g., EDN on eden) failed with "insufficient PMS for fee" because agents had no PMS on the custom ledger. This created a chicken-and-egg deadlock: PMS fees required PMS to exist, but PMS could only appear via fee distribution which required successful transfers. Now, when PMS is unavailable for the protocol fee on custom asset transfers, the fee is gracefully waived. Smart contract transfer fees (in the custom asset) still apply, providing fee revenue to the ledger creator.
- **fix(server)**: `create_reward_block()` silently swallowed errors (returned `None` without logging). Added structured logging for forge failures, persist rejections, and persist errors — makes debugging fee distribution issues on custom ledgers visible in production logs.
- **fix(server)**: `perform_fee_distribution()` used `state._cfg.network.network_id` (ServerConfig) instead of `state.settings.network.network_id` (Settings). While functionally equivalent today (both load global config), `_cfg` is an internal field not intended for fee distribution. Switched to the canonical `settings` field for consistency and future-proofing.

---

## [0.5.11] - 2026-03-17 — Fix bootstrap OOM on large ledgers

### Performance
- **perf(core/critical)**: Bootstrap no longer loads all block IDs into memory. Added `newest_block_ids(n)` to `DagStorage` trait — uses a **reverse RocksDB iterator** to read only the N newest IDs. For Eden (7.8M blocks), this reduces bootstrap memory from ~500 MB (full `Vec<String>`) to ~1.6 MB (25K IDs only). Eliminates the primary cause of OOM kills on 8 GB VPS.
- **perf(storage)**: `RocksStore::newest_block_ids()` override uses `IteratorMode::End` to read N keys in reverse order, then reverses for ascending lex order. O(N) instead of O(total_blocks).

### Changed
- **change(docker/testnet)**: Engine memory limit raised from 6g→7g to provide headroom on 8 GB VPS.
- **change(config/testnet)**: Added RAM scaling guide (8/16/32 GB) as comments in `[rocks]` section for easy tuning.

---

## [0.5.10] - 2026-03-17 — Fix transfer fees broken on custom ledgers

### Fixed
- **fix(server/critical)**: Transfer fees (smart contract `OnTransfer` trigger) never applied on custom ledgers. `evaluate_transfer()` in `prepare_tx()` and `wallet_send_simple()` queried the per-ledger store (empty `contracts` CF) instead of the main store where contracts are registered. Added `contract_store: Arc<dyn ContractStorage>` to `AppState` — always points to main RocksDB. Burn refunds were unaffected (used separate `main_store_for_contracts`).
- **fix(tests)**: Updated all test files constructing `Settings` inline to include fields added in v0.5.7–v0.5.9 (`Rocks::max_open_files`, `P2pConfig` scaling limits).

---

## [0.5.9] - 2026-03-17 — Configurable P2P scaling limits

### Added
- **feat(config)**: 6 new `[p2p]` TOML settings for P2P resource limits: `max_connections` (default 256), `per_peer_queue_cap` (default 2000), `max_orphans` (default 2000), `max_inflight_requests` (default 10000), `max_parent_deps` (default 5000), `max_peer_retries` (default 20).
- **feat(server)**: `Server` struct stores P2P limits from config instead of reading hardcoded constants. `api_only()` uses `P2pConfig::default()` values.

### Changed
- **change(server)**: All P2P resource limits (`MAX_PEER_CONNECTIONS`, `PER_PEER_Q_CAP`, `MAX_INFLIGHT_GETBLOCK`, `MAX_ORPHANS`, `MAX_PARENT_DEPS`) replaced with configurable fields read from `[p2p]` TOML section. No recompilation needed for scaling.
- **change(main)**: `MAX_PEER_RETRIES` hardcoded constant removed — now reads `max_peer_retries` from `[p2p]` config.
- **change(config/testnet)**: Added P2P limits documentation to testnet config with recommended values for 8 GB VPS.

---

## [0.5.8] - 2026-03-17 — Pre-production stability audit (crash prevention)

### Fixed
- **fix(storage/critical)**: RocksDB `max_open_files` now configurable (default 512). Previously unlimited — with 66+ CFs on VPS (ulimit=1024), FD exhaustion caused crashes.
- **fix(fee_distribution/critical)**: Replaced all `[0]` index accesses on treasury wallet lists with `.first()` / `.cloned()`. Empty treasury config no longer panics.
- **fix(storage/critical)**: Activity pagination iterator `list_wallet_activity_paginated()` no longer panics on empty/exhausted iterators. Defensive `Option` handling replaces `.unwrap()` chain.
- **fix(storage/critical)**: `from_be_i64()`, `be_to_ts()`, `le_to_u64()` now return 0 for malformed input instead of panicking on non-8-byte slices.
- **fix(ledger/critical)**: `ensure_schema()` failure is now fatal (`bail!`) instead of silently logged as `warn!`. Prevents operating on outdated/corrupted schema.
- **fix(server)**: TLS config `.unwrap()` replaced with proper error message when `api_tls_enabled=true` but `[tls]` section missing.
- **fix(bridge)**: `disable_bridge()` `.expect()` replaced with `anyhow::bail!` to handle race condition where link is deleted between disable and get.
- **fix(economics)**: `TpsTracker` mutex lock uses `unwrap_or_else(|e| e.into_inner())` to recover from poison instead of cascading panics.

### Added
- **feat(main/critical)**: Graceful SIGTERM/SIGINT shutdown handler. On Docker stop: flushes RocksDB WAL for all ledgers before exiting. Prevents WAL corruption from mid-write kills.
- **feat(server)**: P2P connection semaphore (max 256 concurrent inbound connections). Prevents OOM from connection bombs.
- **feat(config)**: `max_open_files` field in `[rocks]` config section and `RocksMemoryConfig` struct.
- **feat(validation)**: `skip_utxo_checks=true` now emits `tracing::error!` audit log. Flags accidental bypass of double-spend detection in production.

### Changed
- **change(main)**: Peer retry loop now uses exponential backoff (5s→60s) with max 20 attempts instead of retrying forever. Prevents leaked tasks for unreachable peers.
- **change(limits)**: Reduced P2P memory constants for VPS: `PER_PEER_Q_CAP` 10K→2K, `MAX_INFLIGHT_GETBLOCK` 100K→10K, `MAX_ORPHANS` 10K→2K, `MAX_PARENT_DEPS` 20K→5K. Saves ~240 MB under load.

---

## [0.5.7] - 2026-03-17 — Configurable RocksDB memory tuning (OOM prevention)

### Added
- **feat(config)**: 4 new `[rocks]` settings for RocksDB memory control: `write_buffer_size_mb` (per-CF memtable, default 128), `max_write_buffer_number` (per-CF, default 3), `block_cache_size_mb` (shared LRU, default 512), `db_write_buffer_size_mb` (global memtable cap, default 512).
- **feat(storage)**: `RocksMemoryConfig` struct — encapsulates RocksDB memory tuning parameters, passed to `new()` and `open_db_multi_prefix()`. `Default` impl preserves backward-compatible values.
- **feat(storage)**: Global memtable budget via `set_db_write_buffer_size()` — caps total memtable memory across ALL column families. Critical for multi-ledger setups where N×33 CFs can spike and OOM.
- **feat(storage)**: Startup log line showing applied memory tuning (`write_buffer_mb`, `max_write_buffers`, `block_cache_mb`, `db_write_buffer_mb`).

### Changed
- **change(storage)**: `apply_db_tuning()` now accepts `&RocksMemoryConfig` instead of using hardcoded values. All 4 memory pools are configurable.
- **change(storage)**: `RocksStore::new()` and `open_db_multi_prefix()` now require a `&RocksMemoryConfig` parameter.
- **change(config/testnet)**: Testnet config tuned for 8 GB VPS with multiple ledgers: `write_buffer_size_mb=64`, `block_cache_size_mb=256`, `db_write_buffer_size_mb=512`.

---

## [0.5.6] - 2026-03-16 — Multi-wallet TransferFee splits + Ledger ownership transfer

### Added
- **feat(contracts)**: `TransferFeeSplit` struct — each split has `address: String` and `share_bps: u32` (basis points out of 10,000). `ContractAction::TransferFee` now uses `splits: Vec<TransferFeeSplit>` instead of a single `beneficiary_address`. Dust-free rounding: last split gets `total - sum(previous)`.
- **feat(contracts)**: `ContractAction::validate()` method — validates TransferFee splits sum to 10,000, non-empty, positive shares, non-empty addresses.
- **feat(contracts)**: Contract update endpoint `PUT /admin/contracts/{contract_id}` — partial update of scope, actions, enabled. Auto-bumps contract version. Validates TransferFee splits on update.
- **feat(storage)**: `update_contract()` method on `ContractStorage` trait + RocksDB and InMemory implementations.
- **feat(storage)**: `LedgerDefStorage` trait — `get_ledger_def()`, `put_ledger_def()`, `list_ledger_defs()`, `update_owner()`. RocksDB implementation in new `ledger_defs` column family.
- **feat(storage)**: Schema migration 8→9 — adds `ledger_defs` column family for persisting ledger definitions.
- **feat(ledger)**: Ledger ownership transfer via DAG block — `POST /admin/ledgers/{ledger_id}/transfer-ownership` creates an encrypted `LedgerOwnershipTransfer` block in the DAG for full traceability, then applies state change to RocksDB + RAM.
- **feat(ledger)**: `owner_pubkey` and `owner_x25519_pubkey` fields in `CreateLedgerRequest` — specify ownership and encryption key at creation time.
- **feat(ledger)**: Ledger definition persistence — dynamically created ledgers and ownership changes survive restarts. `load_persisted_ledgers()` called at startup.
- **feat(ledger)**: `LedgerManager::update_def()` — hot-swap a ledger's definition in-memory without restart.
- **feat(payload)**: New `PlainPayload::LedgerOwnershipTransfer` variant (#18) — records ledger ownership changes in the DAG. Contains cleartext `ledger_id` for routing/validation + `EncryptedPayload` with `OwnershipTransferData` (new_owner_pubkey, reason). Encrypted for coordinator + current owner + new owner (X25519+AES-256-GCM).
- **feat(config)**: `owner_x25519_pubkey: Option<String>` on `LedgerDef` — stores the owner's X25519 public key for encrypted DAG blocks.

### Changed
- **change(contracts)**: `ContractAction::TransferFee` now uses `splits: Vec<TransferFeeSplit>` instead of single `beneficiary_address: String`. Breaking change for contract registration payloads.
- **change(config)**: Added `Serialize` derive to `LedgerDef`, `LedgerFeesOverride`, `LedgerValidationOverride` (needed for RocksDB JSON persistence).
- **change(server)**: `API_VERSION` 4 → 5 (contract update endpoint + TransferFee splits format + ownership transfer).
- **change(storage)**: `CURRENT_VER` 8 → 9 (new `ledger_defs` column family).
- **change(simulator)**: Updated `ContractActionSim::TransferFee` to use `TransferFeeSplitSim` splits format.
- **change(ledger)**: Ownership transfer refactored from direct RocksDB write to DAG-block-first pattern (encrypted `LedgerOwnershipTransfer` block → persist → apply state). Follows blockchain convention: all state mutations go through the DAG.

---

## [0.5.5] - 2026-03-16 — Smart contract transfer fees (deductive, per-ledger)

### Added
- **feat(contracts)**: New `OnTransfer` trigger in `ContractTrigger` — fires on UTXO token transfers. Supports `asset_id` filter (None = any asset, Some("edenite") = specific).
- **feat(contracts)**: New `TransferFee` action in `ContractAction` — routes a fee to a fixed `beneficiary_address`. Uses `TransferFeeFormula` (PercentageBps or FixedAmount).
- **feat(contracts)**: New `TransferFeeFormula` enum — `PercentageBps { rate_bps }` (fee = amount * bps / 10000) and `FixedAmount { amount }` (flat fee per transfer).
- **feat(contracts)**: `evaluate_transfer()` in `pms-contracts/engine.rs` — evaluates transfer fee contracts at TX preparation time. Returns `Vec<TransferFeeResult>` with beneficiary + fee amount.
- **feat(storage)**: `find_transfer_contracts()` method on `ContractStorage` trait + RocksDB and InMemory implementations. Filters by `asset_id`, `ledger_id`, scope, and enabled status.
- **feat(server)**: Transfer fee outputs added to `prepare_tx()` and `wallet_send_simple()`. The fee is an additional `TxOutput` in the transaction (deductive: sender pays amount + fee). No minting — pure UTXO output.
- **feat(server)**: `transfer_fee` field added to `PrepareTxResponse` and `SendSimpleResponse` — clients can display the total cost breakdown.
- **feat(simulator)**: `GameEngine::setup()` now registers a 5% transfer fee contract on the game ledger, routing fees to the coordinator wallet (ledger creator revenue).

### Changed
- **change(server)**: `API_VERSION` 3 → 4 (tx/prepare and send_simple responses now include `transfer_fee` field).

---

## [0.5.4] - 2026-03-16 — Fix backup path writing to container layer instead of volume

### Fixed
- **fix(storage/critical)**: RocksDB checkpoints (backups) were written to `./backups/pms` (relative CWD), which in Docker resolves to the container's writable layer instead of the mounted volume. On testnet with hourly checkpoints and 7 retained copies, this filled the entire 237 GB disk. Backup path now derived from the DB path itself (`db_path.parent()/backups/pms`), guaranteeing checkpoints land on the same volume as the data.

### Changed
- **change(storage)**: Added `db_path: PathBuf` field to `RocksStore` struct. Populated from the actual DB path in all constructors (`new()`, `from_shared_db()`, `open_read_only()`, `open_secondary()`).
- **change(storage)**: Reduced checkpoint rotation from 7 to 3 retained copies (75 GB → 75 GB max instead of 175 GB).

---

## [0.5.3] - 2026-03-16 — Fix EventBus routing: burns on custom ledgers now reach ContractListener

### Fixed
- **fix(contracts/critical)**: Burns on custom ledgers (eden, etc.) emitted `NftBurnProcessed` on the **per-ledger** EventBus, but the `ContractListener` was subscribed to the **main** EventBus only. Events never reached the listener → zero EDN distributed. Fixed by adding `contract_event_bus: Option<EventBus>` to `AppState`, always pointing to the main adapter's bus. `emit_nft_burn_processed()` now uses this shared bus regardless of which ledger the burn occurs on.

---

## [0.5.2] - 2026-03-15 — Extract contract engine into pms-contracts crate + EventBus decoupling

### Changed
- **refactor(contracts)**: Extracted contract evaluation engine into new `pms-contracts` crate. `contract_engine.rs` moved from `pms-server` to `pms-contracts/src/engine.rs`. The `ContractResult` type, `evaluate_nft_burn()`, formula evaluation, and all 9 unit tests moved intact.
- **refactor(contracts)**: Decoupled contract evaluation from NFT burn handlers via EventBus. The 3 direct calls to `evaluate_contracts_after_burn()` in `nft.rs` are replaced by `NftBurnProcessed` event emissions. A new `ContractListener` subscribes to these events and evaluates contracts asynchronously.
- **refactor(contracts)**: Introduced `RefundSink` trait in `pms-contracts` to decouple refund accumulation from `pms-server`'s `FeePoolRegistry`. `FeePoolRefundSink` in `api.rs` bridges the two.
- **refactor(server)**: Removed `contract_store` field from `AppState` — the contract listener receives its own `Arc<dyn ContractStorage>` at startup, always pointing to the main RocksDB.

### Added
- **feat(event)**: New `PmsEvent::NftBurnProcessed` variant carrying `block_id`, `ledger_id`, `burner_address`, `token_ids`, and pre-fetched `NftMetadata`. Emitted by burn handlers BEFORE `apply_action()` (which destroys metadata references).
- **feat(contracts)**: `pms-contracts` crate — dedicated crate for contract engine + EventBus listener. Contains `engine.rs` (evaluation), `listener.rs` (subscriber + `RefundSink` trait).

---

## [0.5.1] - 2026-03-15 — Fix EDN burn refunds not distributed on custom ledgers

### Fixed
- **fix(contracts/critical)**: Smart contracts registered on the main ledger were invisible to NFT burn handlers on custom ledgers (e.g. eden). `evaluate_contracts_after_burn()` used `state.store` (per-ledger RocksDB) for contract lookups, but contracts are only stored in the **main** RocksDB. Burns produced zero refunds → agents never received EDN. Fixed by adding `contract_store` field to `AppState` that always points to the main store, and using it for contract lookups regardless of which ledger the burn occurs on.

---

## [0.5.0] - 2026-03-15 — Service Status Monitoring + Deploy Fixes

### Added
- **feat(gateway)**: Background infrastructure health checker. Polls Engine, Prometheus, Simulator, and Caddy every 20s with concurrent requests and caches the results. New endpoint `GET /services/status` returns a JSON snapshot with service name, status (`up`/`down`/`degraded`), latency, and optional detail (e.g. block count for Engine). Configurable via `SERVICES_MONITOR` and `SERVICES_CHECK_INTERVAL` env vars.
- **feat(dashboard)**: Service status bar in pms-dashboard (Svelte). Displays colored dots (green/red/orange) with service names in the header top-left area. Self-contained component with 30s polling to `/services/status`. Responsive: hides names on mobile, shows only dots.
- **feat(simulator)**: Credential validation with backoff retry. `SimConfig::validate_credentials()` checks all required secrets (API key, admin token, coordinator key/address). Startup loop retries up to 10 times with increasing delays (30s, 45s, 60s, ... +15s per attempt). Exits with code 0 after exhaustion so `on-failure` restart policy stops.

### Fixed
- **fix(deploy)**: Fix TOML config corruption in `deploy-testnet.sh` and `upgrade-testnet.sh`. SSH `sed` commands with double-quoted TOML values stripped the `"` chars. Fixed by switching to heredocs.
- **fix(deploy)**: Fix deploy starting simulator before API key exists. Core services start first, then API key is created, then simulator starts only if all credentials are present.
- **fix(deploy)**: Fix SCP "No space left on device" — services are now stopped before uploading images to free disk space.

### Infrastructure
- **docker-compose**: Added `SERVICES_MONITOR` and `SERVICES_CHECK_INTERVAL` env vars to gateway service.
- **docker-compose**: Changed simulator restart policy from `unless-stopped` to `on-failure`.
- **dashboard**: Added Vite dev proxy `/services` → gateway (8443) for local development.

---

## [0.4.4] - 2026-03-15 — OOM Fix + Containerd Cleanup

### Fixed
- **infra(critical)**: Fix engine OOM-kill at 4GB container limit. With 632K accumulated blocks, 500K UTXO cache, and 512MB RocksDB block cache, the engine exceeded the 4GB memory cap — triggering 108 container restarts and generating 200GB+ of containerd snapshots.
- **deploy(critical)**: Fix TOML config corruption in `deploy-testnet.sh` and `upgrade-testnet.sh`. Config value restoration used `ssh "sed ..."` where double quotes from TOML values (e.g. `key = "02abc..."`) broke SSH shell quoting — the `"` were stripped, producing invalid TOML (`key = 02abc...`). Engine crashed on restart with a parse error. Fixed by switching all SSH `sed` commands to heredocs (`<< EOF`) where `"` is always literal.
- **simulator(critical)**: Fix crash-loop when `PMS_API_KEY`, `PMS_COORDINATOR_KEY`, or `PMS_COORDINATOR_ADDR` are missing. Simulator now validates all required credentials at startup with a backoff retry loop (30s, 45s, 60s, ... +15s per attempt, max 10 attempts ~16 min). After 10 failed attempts, exits gracefully (code 0) so Docker `on-failure` restart policy does NOT restart it. Previously, the simulator crash-looped indefinitely on missing env vars, filling the disk with containerd snapshots.
- **deploy**: Fix deploy script starting simulator before API key exists. Core services (Engine, Gateway, Caddy, Prometheus) are now started first, then the SDK API key is created, then the simulator is started with all credentials. The simulator is not started at all if any credential is missing.

### Infrastructure
- **docker-compose**: Bumped engine `mem_limit` from 4GB to 6GB (`memswap_limit` too) to prevent OOM kills with large block histories.
- **docker-compose**: Changed simulator restart policy from `unless-stopped` to `on-failure`. The simulator exits with code 0 after exhausting startup retries (missing credentials), so Docker won't restart it endlessly. Operational crashes (exit 1) still trigger restarts.
- **config**: Reduced `max_utxos` from 500,000 to 250,000 in testnet config to lower memory footprint.
- **deploy**: Added containerd snapshot prune documentation and `docker image prune` to deploy script.
- **deploy**: Deploy script now stops all services BEFORE uploading images (prevents crash-loop from filling disk during SCP transfer).
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
| 0.7.4 | 2026-04-25 | Production hardening sprint: admin-auth timing-safe + CSRF defense, coordinator key encrypted-at-rest (AES-256-GCM + Argon2id), enriched /healthz (4 real checks), 10 new Prometheus metrics + 5s sampler, trust model documentation, curative tips rebuild (H3), 4 chaos recovery tests, in-band coordinator key rotation |
| 0.7.3 | 2026-04-23 | trim_tips zombie eviction (H3), atomic encrypted UTXO delta (H1), persist hot-path clone reduction (H6) |
| 0.7.2 | 2026-04-22 | Security audit sprint: persist back-pressure, spent-tracking storage fallback, AAD binding, Wallet Debug redaction, bridge multiplier validation, rand unification (no RC), parking_lot migration, treasury misconfig surfacing, freeze/unfreeze race fix |
| 0.3.0 | Unreleased | Economics system (fee burn, gas pools, dynamic fees) |
| 0.2.7 | 2026-03-14 | Pin L0 index/filter + 512MB cache |
| 0.2.6 | 2026-03-14 | Bloom filters on all 31 CFs |
| 0.2.5 | 2026-03-13 | Fix RocksDB L0 write stall (120→20 TPS cliff) |
| 0.2.4 | 2026-03-13 | Fix missing CF crash at bootstrap |
| 0.2.3 | 2026-03-13 | 12 hot-path optimizations |
| 0.2.1 | 2026-03-13 | Smart contracts + TPS fix at scale |
| 0.2.0 | 2026-03-12 | Architecture rewrite, features, 4028 TPS |
| 0.1.0 | 2025-12-28 | Initial DAG implementation |
