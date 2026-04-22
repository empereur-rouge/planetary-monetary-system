//! Core RocksStore struct definition, construction, and column family management.
//!
//! The `RocksStore` implementation is split across multiple files:
//! - `store.rs` (this file): struct, construction, CF management
//! - `activity_index.rs`: per-address activity indexing and pagination
//! - `dag_storage_impl.rs`: `DagStorage` trait implementation
//! - `maintenance.rs`: tip trimming, background tasks, checkpoint rotation
//! - `secondary.rs`: read-only and secondary DB instances
//! - `atomic.rs`: atomic block append with DAG indices
//! - `compliance_registry.rs`: frozen/seized address management
//! - `nft_storage.rs`: NFT ownership tracking
//! - `token_registry.rs`: token metadata
//! - `node_rewards_storage.rs`: node reward tracking
//! - `gas_pool_storage.rs`: per-ledger gas pool
//! - `ledger_storage.rs`: ledger definition persistence
//! - `config_storage.rs`: runtime configuration storage
//! - `contract_storage.rs`: smart contract persistence

use crate::{PutResult, StoredBlock};
use anyhow::{Context, Result};
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamilyDescriptor, DBWithThreadMode,
    MultiThreaded, Options,
};

/// Thread-safe DB handle usable with `Arc<PmsDb>`.
/// `MultiThreaded` mode allows `create_cf(&self, ...)` (no `&mut self` needed),
/// enabling dynamic column family creation for new ledgers at runtime.
pub type PmsDb = DBWithThreadMode<MultiThreaded>;

use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::time::Duration;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReindexStats {
    pub total_blocks: usize,
    pub indexed: usize,
    pub skipped_encrypted: usize,
    pub skipped_no_payload: usize,
}

pub struct RocksStore {
    /// handle RocksDB partagé (MultiThreaded for dynamic CF creation)
    pub db: Arc<PmsDb>,
    /// nombre max de tips qu'on garde (même rôle que RedisStore.tip_limit)
    pub tip_limit: usize,
    /// namespace logique (équivalent prefix Redis)
    pub prefix: String,
    /// Intervalle de checkpoint
    pub checkpoint_interval: Duration,
    /// Absolute path to the RocksDB data directory.
    /// Used to derive the backup path (sibling `backups/` directory) so that
    /// checkpoints always land on the same volume as the data itself.
    pub db_path: PathBuf,
    /// Approximate count of tips in the CF. Used to skip expensive `trim_tips`
    /// full scans when we're clearly under the limit. Updated atomically on
    /// every tip add/remove; the actual trim logic still does a full scan when
    /// the estimate exceeds `tip_limit`.
    pub(crate) tip_count_estimate: std::sync::atomic::AtomicUsize,
    /// Cached result of `top_tips()` with a short TTL (500ms). Avoids repeated
    /// full scans of the tips CF when called frequently (fee distribution, parent
    /// selection). The Mutex critical section is very short (no I/O inside).
    pub(crate) top_tips_cache: parking_lot::Mutex<Option<(std::time::Instant, Vec<String>)>>,
    /// Pre-computed "prefix:cf_short_name" strings. Eliminates `format!()`
    /// allocation on every `cf()` call (~11 calls per block in hot path).
    pub(crate) cf_names: HashMap<String, String>,
    /// Cached RuntimeConfig with 500ms TTL. Avoids RocksDB read + JSON deser
    /// on every `persist_block()` call. Write-through on `set_runtime_config()`.
    pub(crate) runtime_config_cache: parking_lot::Mutex<Option<(std::time::Instant, pms_config::RuntimeConfig)>>,
    /// In-memory set of frozen addresses. Populated at bootstrap from the
    /// `compliance_frozen` CF. Updated on freeze/unfreeze. Turns O(N) RocksDB
    /// reads per TxUtxo into O(N) DashSet lookups (lock-free, no I/O).
    pub(crate) frozen_set: dashmap::DashSet<String>,
    /// Counter of block persists. Used to amortize `trim_tips()` calls —
    /// only runs every 64 blocks instead of on every single persist.
    pub(crate) persist_counter: std::sync::atomic::AtomicU64,
}

/// RocksDB memory tuning parameters, extracted from `[rocks]` config.
///
/// Controls the three main memory pools:
/// - **Write buffers (memtables)**: `write_buffer_size_mb` × `max_write_buffer_number` per CF
/// - **Block cache**: `block_cache_size_mb` shared LRU across all CFs
/// - **Global memtable budget**: `db_write_buffer_size_mb` caps total memtable RAM
///
/// With N ledgers × 33 CFs each, the default per-CF settings can cause OOM.
/// Use `db_write_buffer_size_mb` to cap total memtable memory globally.
#[derive(Debug, Clone)]
pub struct RocksMemoryConfig {
    /// Write buffer (memtable) size per column family, in MB. Default: 32.
    ///
    /// **CRITICAL for multi-ledger**: Total memtable RAM ≈ `num_CFs × max_write_buffer_number × write_buffer_size_mb`.
    /// With 2 ledgers (66 CFs), 128 MB × 3 × 66 = 25 GB → OOM. Use 32 MB for safety.
    pub write_buffer_size_mb: usize,
    /// Max memtables kept in memory per CF before stalling writes. Default: 3.
    pub max_write_buffer_number: i32,
    /// Shared LRU block cache in MB (all CFs). Default: 1024.
    /// With Direct I/O (v0.5.21), this is the ONLY read cache — size generously.
    pub block_cache_size_mb: usize,
    /// Global memtable flush trigger in MB. When total memtable across all CFs exceeds
    /// this, RocksDB triggers flushes. **NOT a hard memory cap** — immutable memtables
    /// waiting for flush still consume RAM beyond this limit. Default: 512.
    pub db_write_buffer_size_mb: usize,
    /// Maximum open file descriptors for RocksDB. -1 = unlimited. Default: 512.
    pub max_open_files: i32,
}

impl Default for RocksMemoryConfig {
    fn default() -> Self {
        Self {
            write_buffer_size_mb: 32,
            max_write_buffer_number: 3,
            block_cache_size_mb: 1024,
            db_write_buffer_size_mb: 512,
            max_open_files: 512,
        }
    }
}

impl RocksStore {
    /// Common DB-level tuning applied to BOTH `new()` and `open_db_multi_prefix()`.
    /// Centralised here to guarantee identical settings on every code path.
    pub(crate) fn apply_db_tuning(db_opts: &mut Options, mem: &RocksMemoryConfig) {
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        // Parallelism: one background thread per CPU core (min 8)
        let ncpu = num_cpus::get() as i32;
        db_opts.increase_parallelism(ncpu);

        // Background jobs: scale with CPU cores (min 8).
        // More threads = better flush+compaction overlap under sustained write load.
        // On 8-core VPS at 600+ blk/s, 6 threads couldn't drain L0 fast enough.
        db_opts.set_max_background_jobs(ncpu.max(8));

        db_opts.set_level_compaction_dynamic_level_bytes(true);

        // Memory tuning — configurable via [rocks] section in config.toml
        db_opts.set_write_buffer_size(mem.write_buffer_size_mb * 1024 * 1024);
        db_opts.set_max_write_buffer_number(mem.max_write_buffer_number);
        db_opts.set_target_file_size_base(64 * 1024 * 1024); // 64 MB per SSTable

        // Merge 2 memtables before flushing to L0: halves L0 file count at the
        // cost of slightly larger flushes. Net effect: fewer L0 files → fewer
        // write stalls under sustained load.
        db_opts.set_min_write_buffer_number_to_merge(2);

        // Global memtable budget: caps TOTAL memtable memory across all CFs.
        // Critical for multi-ledger setups (N × 33 CFs can spike without a cap).
        if mem.db_write_buffer_size_mb > 0 {
            db_opts.set_db_write_buffer_size(mem.db_write_buffer_size_mb * 1024 * 1024);
        }

        // File descriptor limit: prevents FD exhaustion with many CFs + deep LSM trees.
        // Default OS ulimit is typically 1024. With 66+ CFs, unlimited FDs can crash.
        db_opts.set_max_open_files(mem.max_open_files);

        // === WRITE PIPELINE (v0.5.20) ===
        //
        // Pipelined writes overlap WAL append and memtable insert into two stages,
        // allowing the next writer to start its WAL append while the previous one
        // inserts into the memtable. At 600+ blk/s with 10+ CF writes per block,
        // this reduces write latency by ~30-40%.
        db_opts.set_enable_pipelined_write(true);

        tracing::info!(
            write_buffer_mb = mem.write_buffer_size_mb,
            max_write_buffers = mem.max_write_buffer_number,
            block_cache_mb = mem.block_cache_size_mb,
            db_write_buffer_mb = mem.db_write_buffer_size_mb,
            max_open_files = mem.max_open_files,
            background_jobs = ncpu.max(8),
            direct_io = true,
            "RocksDB memory tuning applied (Direct I/O enabled)"
        );

        // === WRITE STALL PREVENTION (v0.5.20: thresholds doubled again) ===
        //
        // v0.5.16 raised from 20/24 to 40/56. Still insufficient at 600+ blk/s
        // sustained: L0 accumulates ~1 file every 3-5s across 33 CFs, hitting
        // the slowdown threshold after 40-60 minutes → TPS cliff to 20 blk/s.
        //
        // New thresholds: 80/120 (4x original defaults).
        // Tradeoff: more L0 files = slightly slower point lookups, but bloom
        // filters mitigate this. Write throughput stability >> read micro-latency.
        db_opts.set_level_zero_file_num_compaction_trigger(4); // start compaction early (default)
        db_opts.set_level_zero_slowdown_writes_trigger(80); // 40→80: 4x default headroom
        db_opts.set_level_zero_stop_writes_trigger(120); // 56→120: hard stop raised proportionally
        db_opts.set_max_subcompactions(4); // 3→4: more parallelism per compaction job

        // === DIRECT I/O (v0.5.21) ===
        //
        // **Root cause of production OOM crashes**: Linux counts kernel page cache
        // against the Docker cgroup memory limit. RocksDB SST file reads are cached
        // by the kernel (4-10 GB for a 15M+ block DB with 33 CFs), so:
        //   Application RSS (2-3 GB) + page cache (4-10 GB) > mem_limit (14 GB) → OOM kill
        //
        // `advise_random_on_open(true)` (v0.5.16) only disabled readahead but still
        // used the page cache for individual reads. Direct I/O bypasses the kernel
        // page cache ENTIRELY — all reads go through RocksDB's own block cache
        // (`block_cache_size_mb`), giving us deterministic memory usage.
        //
        // Memory budget with Direct I/O on 16 GB VPS (mem_limit=14g, 2 ledgers = 66 CFs):
        //   - Memtables: 66 CFs × 3 × 32 MB = 6.3 GB worst-case (~3 GB average)
        //   - Block cache: 1 GB (shared LRU, sole read cache)
        //   - UTXO RAM cache: ~400 MB (2M UTXOs)
        //   - Bloom filters + indexes: ~200 MB (pinned in block cache)
        //   - Application + runtime: ~500 MB
        //   - Total: ~5.1 GB average, ~8.4 GB worst — safe within 14 GB
        //
        // WARNING (v0.5.22): db_write_buffer_size_mb is a FLUSH TRIGGER, not a hard cap.
        // With 66 CFs × 128 MB buffers (v0.5.21), actual heap was 12 GB → OOM.
        // Keep write_buffer_size_mb ≤ 32 for multi-ledger deployments.
        //
        // Tradeoff: slightly higher read latency for cold data (no OS page cache
        // warmup), but the block cache covers the hot working set. For a write-heavy
        // DAG engine doing 2000+ blk/s, write throughput stability >> cold read latency.
        db_opts.set_use_direct_reads(true);
        db_opts.set_use_direct_io_for_flush_and_compaction(true);

        // Compaction readahead: with Direct I/O enabled, RocksDB performs its own
        // buffered sequential reads during compaction. 2 MB readahead amortizes
        // the cost of aligned I/O for compaction's sequential access pattern.
        db_opts.set_compaction_readahead_size(2 * 1024 * 1024);
    }

    /// Build a map of short CF name → full "prefix:name" string.
    /// Called once at construction to eliminate `format!()` on every `cf()` call.
    pub(crate) fn build_cf_names(prefix: &str) -> HashMap<String, String> {
        let mut map = HashMap::with_capacity(Self::CF_NAMES.len() + 8);
        for &name in Self::CF_NAMES {
            map.insert(name.to_string(), format!("{prefix}:{name}"));
        }
        map
    }

    pub async fn new(
        path: &str,
        tip_limit: usize,
        prefix: impl Into<String>,
        checkpoint_interval_secs: Option<u64>,
        mem: &RocksMemoryConfig,
    ) -> Result<Self> {
        let prefix = prefix.into();
        // Défaut 24h si None
        let checkpoint_interval =
            Duration::from_secs(checkpoint_interval_secs.unwrap_or(24 * 3600));

        let path = PathBuf::from(path);
        tracing::info!(path = %path.display(), "RocksDB init");

        std::fs::create_dir_all(&path)
            .with_context(|| format!("create_dir_all({})", path.display()))?;

        // ==============
        // 1) Options DB globales (SSD-friendly, write-stall prevention)
        // ==============
        let mut db_opts = Options::default();
        Self::apply_db_tuning(&mut db_opts, mem);

        // ==============
        // 2) CF attendues (préfixées)
        // ==============
        let required: BTreeSet<String> = [
            "blocks",
            "idx_blocks",
            "by_time",
            "id2ts",
            "final",
            "last_ms",
            "children_count",
            "tips",
            "children_set",
            "ver",
            "utxo",
            "utxo_spent",
            "tx_applied",
            "nft_ownership",     // NFT ownership tracking: token_id -> owner_address
            "nfts_by_owner",     // Reverse index: owner_address -> list of token_ids (JSON)
            "nft_block_ids", // NFT block references: token_id -> block_id (encrypted metadata in DAG)
            "runtime_config", // Current runtime config (single key "current")
            "config_history", // History of config changes (block_id -> entry)
            "node_block_counts", // Block count per node: node_pk -> count
            "node_fee_pool", // Fee pool: single key "pool" -> amount (u64)
            "node_reward_addresses", // Reward addresses: node_pk -> address
            "token_registry", // Token registry: asset_id -> TokenMetadata (JSON)
            "compliance_frozen", // Frozen addresses: address -> FrozenEntry (JSON)
            "compliance_log", // Compliance audit trail: block_id -> ComplianceLogEntry (JSON)
            "addr_activity", // Per-address activity index: [addr][0x00][ts:8][block_id] -> ""
            "addr_type_activity", // Per-address-per-type index: [addr][0x00][cat:1][ts:8][block_id] -> ""
            "activity_items", // Pre-computed activity items: same key as addr_activity -> JSON(Vec<StoredActivityItem>)
            "contracts",     // Declarative smart contracts: contract_id -> Contract (JSON)
            "gas_pools",     // Gas pools per ledger: ledger_id -> GasPool (JSON)
            "ledger_subscriptions", // Ledger annual subscriptions: ledger_id -> LedgerSubscription (JSON)
            "ledger_defs",   // Persisted ledger definitions: ledger_id -> LedgerDef (JSON)
        ]
        .into_iter()
        .map(|s| format!("{prefix}:{s}"))
        .collect();

        // 3) Si le dossier existe déjà, on valide les CF existantes
        if Path::new(&path).exists() {
            let existing = PmsDb::list_cf(&db_opts, &path).unwrap_or_default();
            let existing_prefixed: BTreeSet<String> = existing
                .iter()
                .filter(|cf| *cf != "default")
                .cloned()
                .collect();

            // a) CF avec un autre prefix → DB incohérente
            let wrong_prefix = existing_prefixed
                .iter()
                .any(|cf| !cf.starts_with(&format!("{prefix}:")));
            if wrong_prefix {
                anyhow::bail!(
                    "Incohérence: DB contient d'autres prefixes. prefix='{prefix}', existantes={existing_prefixed:?}"
                );
            }

            // b) S'il manque des CF, RocksDB les créera grâce à create_missing_column_families(true).
        }

        // ==============
        // 4) Helper pour CF options (bloom pour index)
        // ==============
        // Shared LRU block cache across all CFs.
        // Must be large enough to hold index+filter blocks for all CFs
        // without evicting hot data blocks.
        let shared_cache = Cache::new_lru_cache(mem.block_cache_size_mb * 1024 * 1024);

        fn cf_opts_with_bloom(cache: &Cache) -> Options {
            let mut opts = Options::default();
            opts.set_optimize_filters_for_hits(true);
            // advise_random_on_open removed in v0.5.21: Direct I/O (set at DB level)
            // bypasses the kernel page cache entirely, making fadvise hints irrelevant.

            let mut table_opts = BlockBasedOptions::default();
            table_opts.set_bloom_filter(10.0, false);
            table_opts.set_block_cache(cache);
            table_opts.set_cache_index_and_filter_blocks(true);
            // Pin L0 index+filter blocks so they're never evicted from cache.
            // Without this, N CFs compete for cache space and L0 blocks
            // get evicted → every point lookup needs 2+ disk reads → TPS→0.
            table_opts.set_pin_l0_filter_and_index_blocks_in_cache(true);
            opts.set_block_based_table_factory(&table_opts);
            opts
        }

        // 5) Construire les CF descriptors
        let mut cf_descs = Vec::with_capacity(required.len() + 1);

        // CF "default" basique (peu utilisée)
        cf_descs.push(ColumnFamilyDescriptor::new(
            "default".to_string(),
            Options::default(),
        ));

        for name in &required {
            // Bloom filter + shared block cache on ALL CFs.
            // Without bloom, point lookups degrade as LSM levels grow
            // (was only 7/31 CFs → progressive TPS decline over time).
            let mut opts = cf_opts_with_bloom(&shared_cache);
            opts.create_if_missing(true);

            cf_descs.push(ColumnFamilyDescriptor::new(name.clone(), opts));
        }

        // 6) Ouverture DB + CF (MultiThreaded for dynamic CF creation support)
        let db = PmsDb::open_cf_descriptors(&db_opts, &path, cf_descs)
            .with_context(|| format!("open RocksDB at {}", path.display()))?;

        let cf_names = Self::build_cf_names(&prefix);
        // Canonicalize the DB path so backup derivation is always absolute.
        let db_path = std::fs::canonicalize(&path)
            .unwrap_or_else(|_| path.clone());
        Ok(Self {
            db: Arc::new(db),
            tip_limit,
            prefix,
            checkpoint_interval,
            db_path,
            tip_count_estimate: std::sync::atomic::AtomicUsize::new(0),
            top_tips_cache: parking_lot::Mutex::new(None),
            cf_names,
            runtime_config_cache: parking_lot::Mutex::new(None),
            frozen_set: dashmap::DashSet::new(),
            persist_counter: std::sync::atomic::AtomicU64::new(0),
        })
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Multi-Ledger: shared DB with multiple prefixes
    // ═══════════════════════════════════════════════════════════════════════

    /// Liste des column families de base requises par chaque prefix/ledger.
    pub const CF_NAMES: &[&str] = &[
        "blocks",
        "idx_blocks",
        "by_time",
        "id2ts",
        "final",
        "last_ms",
        "children_count",
        "tips",
        "children_set",
        "ver",
        "utxo",
        "utxo_spent",
        "tx_applied",
        "nft_ownership",
        "nfts_by_owner",
        "nft_block_ids",
        "runtime_config",
        "config_history",
        "node_block_counts",
        "node_fee_pool",
        "node_reward_addresses",
        "token_registry",
        "bridge_consumed",
        "bridge_links",
        "compliance_frozen",
        "compliance_log",
        "addr_activity",
        "addr_type_activity",
        "activity_items",
        "contracts",
        "gas_pools",
        "ledger_subscriptions",
        "ledger_defs",
    ];

    /// Ouvre un RocksDB avec les column families de **plusieurs prefixes** à la fois.
    /// Retourne un `Arc<DB>` partageable entre N `RocksStore` instances.
    pub async fn open_db_multi_prefix(
        path: &str,
        prefixes: &[String],
        mem: &RocksMemoryConfig,
    ) -> Result<Arc<PmsDb>> {
        let path = PathBuf::from(path);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create_dir_all({})", path.display()))?;

        let mut db_opts = Options::default();
        Self::apply_db_tuning(&mut db_opts, mem);

        // Shared LRU block cache across all CFs.
        let shared_cache = Cache::new_lru_cache(mem.block_cache_size_mb * 1024 * 1024);

        fn cf_opts_with_bloom(cache: &Cache) -> Options {
            let mut opts = Options::default();
            opts.set_optimize_filters_for_hits(true);
            // advise_random_on_open removed in v0.5.21: Direct I/O (set at DB level)
            // bypasses the kernel page cache entirely, making fadvise hints irrelevant.
            let mut table_opts = BlockBasedOptions::default();
            table_opts.set_bloom_filter(10.0, false);
            table_opts.set_block_cache(cache);
            table_opts.set_cache_index_and_filter_blocks(true);
            table_opts.set_pin_l0_filter_and_index_blocks_in_cache(true);
            opts.set_block_based_table_factory(&table_opts);
            opts
        }

        // Collect all required CFs across all prefixes
        let mut cf_descs = vec![ColumnFamilyDescriptor::new(
            "default".to_string(),
            Options::default(),
        )];

        for prefix in prefixes {
            for &cf_name in Self::CF_NAMES {
                let full = format!("{prefix}:{cf_name}");
                // Bloom filter + shared block cache on ALL CFs.
                // Without bloom, point lookups degrade as LSM levels grow.
                let mut opts = cf_opts_with_bloom(&shared_cache);
                opts.create_if_missing(true);
                cf_descs.push(ColumnFamilyDescriptor::new(full, opts));
            }
        }

        // Also include any existing CFs that might belong to other prefixes
        if path.exists() {
            let existing = PmsDb::list_cf(&db_opts, &path).unwrap_or_default();
            // Re-collect declared names properly
            let mut declared_names: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            declared_names.insert("default".to_string());
            for prefix in prefixes {
                for &cf_name in Self::CF_NAMES {
                    declared_names.insert(format!("{prefix}:{cf_name}"));
                }
            }

            for cf in existing {
                if cf != "default" && !declared_names.contains(&cf) {
                    let mut opts = cf_opts_with_bloom(&shared_cache);
                    opts.create_if_missing(true);
                    cf_descs.push(ColumnFamilyDescriptor::new(cf, opts));
                }
            }
        }

        let db = PmsDb::open_cf_descriptors(&db_opts, &path, cf_descs)
            .with_context(|| format!("open multi-prefix RocksDB at {}", path.display()))?;

        Ok(Arc::new(db))
    }

    /// Crée un `RocksStore` à partir d'un `Arc<DB>` déjà ouvert (multi-ledger).
    /// Le prefix doit correspondre à des column families déjà créées dans la DB.
    pub fn from_shared_db(
        db: Arc<PmsDb>,
        prefix: impl Into<String>,
        tip_limit: usize,
        checkpoint_interval_secs: Option<u64>,
    ) -> Self {
        let checkpoint_interval =
            Duration::from_secs(checkpoint_interval_secs.unwrap_or(24 * 3600));
        let prefix = prefix.into();
        let cf_names = Self::build_cf_names(&prefix);
        let db_path = db.path().to_path_buf();
        Self {
            db,
            tip_limit,
            prefix,
            checkpoint_interval,
            db_path,
            tip_count_estimate: std::sync::atomic::AtomicUsize::new(0),
            top_tips_cache: parking_lot::Mutex::new(None),
            cf_names,
            runtime_config_cache: parking_lot::Mutex::new(None),
            frozen_set: dashmap::DashSet::new(),
            persist_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub async fn put_block(&self, b: &StoredBlock) -> Result<PutResult> {
        let cf_blocks = self.cf("blocks");
        let cf_idx = self.cf("idx_blocks");

        let key = b.id.as_bytes();

        // 1. check existence
        if self.db.get_cf(&cf_blocks, key)?.is_some() {
            // déjà là → Always register in idx_blocks just in case (idempotent)
            self.db.put_cf(&cf_idx, key, b"")?;
            return Ok(PutResult::AlreadyExists);
        }

        // 2. serialize StoredBlock en JSON (même format que Redis)
        let json = serde_json::to_vec(b)?;

        // 3. store
        self.db.put_cf(&cf_blocks, key, json)?;
        self.db.put_cf(&cf_idx, key, b"")?;

        Ok(PutResult::Inserted)
    }

    /// Ensure all column families from `CF_NAMES` exist for this store's prefix.
    /// Creates any missing CFs at runtime (MultiThreaded mode supports this).
    /// Called at bootstrap to handle schema upgrades where new CFs were added.
    pub fn ensure_column_families(&self) -> anyhow::Result<()> {
        let mut created = 0usize;
        for &cf_name in Self::CF_NAMES {
            let full_name = self
                .cf_names
                .get(cf_name)
                .cloned()
                .unwrap_or_else(|| format!("{}:{}", self.prefix, cf_name));
            if self.db.cf_handle(&full_name).is_none() {
                self.db
                    .create_cf(&full_name, &rocksdb::Options::default())
                    .with_context(|| format!("creating missing CF '{full_name}'"))?;
                created += 1;
            }
        }
        if created > 0 {
            tracing::info!(
                prefix = %self.prefix,
                created,
                "Created missing column families during bootstrap"
            );
        }
        Ok(())
    }

    /// Load frozen addresses from RocksDB into the in-memory DashSet.
    /// Called once at bootstrap. After this, `is_frozen()` never hits RocksDB.
    pub fn load_frozen_cache(&self) -> anyhow::Result<()> {
        let cf = self.cf("compliance_frozen");
        let mut count = 0usize;
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (k, _) = kv?;
            let address = String::from_utf8(k.to_vec())?;
            self.frozen_set.insert(address);
            count += 1;
        }
        if count > 0 {
            tracing::info!("Frozen address cache loaded: {} addresses", count);
        }
        Ok(())
    }
}
