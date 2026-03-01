use crate::UtxoDelta;
use crate::checkpoint_rocks::rotate_checkpoints;
use crate::helpers::{be_to_i64, key_time_index};
use crate::helpers::{be_to_ts, le_to_u64, now_ms_i64, parse_time_index_key, ts_to_be, u64_to_le};
use crate::{DagStorage, PutResult, StoredBlock};
use anyhow::{Context, Result};
use pms_wire::WireBlock;
use rocksdb::{BlockBasedOptions, Cache, ColumnFamilyDescriptor, DBWithThreadMode, Direction, IteratorMode, MultiThreaded, Options};

/// Thread-safe DB handle usable with `Arc<PmsDb>`.
/// `MultiThreaded` mode allows `create_cf(&self, ...)` (no `&mut self` needed),
/// enabling dynamic column family creation for new ledgers at runtime.
pub type PmsDb = DBWithThreadMode<MultiThreaded>;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::{Duration, MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

pub struct RocksStore {
    /// handle RocksDB partagé (MultiThreaded for dynamic CF creation)
    pub db: Arc<PmsDb>,
    /// nombre max de tips qu'on garde (même rôle que RedisStore.tip_limit)
    pub tip_limit: usize,
    /// namespace logique (équivalent prefix Redis)
    pub prefix: String,
    /// Intervalle de checkpoint
    pub checkpoint_interval: Duration,
}

impl RocksStore {
    pub async fn new(
        path: &str,
        tip_limit: usize,
        prefix: impl Into<String>,
        checkpoint_interval_secs: Option<u64>,
    ) -> Result<Self> {
        let prefix = prefix.into();
        // Défaut 24h si None
        let checkpoint_interval =
            Duration::from_secs(checkpoint_interval_secs.unwrap_or(24 * 3600));

        let path = PathBuf::from(path);
        eprintln!("[rocks] init at {}", path.display());

        std::fs::create_dir_all(&path)
            .with_context(|| format!("create_dir_all({})", path.display()))?;

        // ==============
        // 1) Options DB globales (SSD-friendly)
        // ==============
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        // Un peu d’optimisation générique
        db_opts.increase_parallelism(num_cpus::get() as i32);
        db_opts.set_max_background_jobs(4);
        db_opts.set_level_compaction_dynamic_level_bytes(true);

        // Tunings mémoire “raisonnables” pour dev/prod
        db_opts.set_write_buffer_size(64 * 1024 * 1024); // 64 MB
        db_opts.set_max_write_buffer_number(3);
        db_opts.set_target_file_size_base(64 * 1024 * 1024); // 64 MB par sstable

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
            "token_registry",        // Token registry: asset_id -> TokenMetadata (JSON)
            "compliance_frozen",     // Frozen addresses: address -> FrozenEntry (JSON)
            "compliance_log",        // Compliance audit trail: block_id -> ComplianceLogEntry (JSON)
            "addr_activity",         // Per-address activity index: [addr][0x00][ts:8][block_id] -> ""
            "addr_type_activity",    // Per-address-per-type index: [addr][0x00][cat:1][ts:8][block_id] -> ""
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

            // b) S’il manque des CF, RocksDB les créera grâce à create_missing_column_families(true).
        }

        // ==============
        // 4) Helper pour CF options (bloom pour index)
        // ==============
        fn cf_opts_with_bloom() -> Options {
            let mut opts = Options::default();
            opts.set_optimize_filters_for_hits(true);

            let mut table_opts = BlockBasedOptions::default();
            // ~10 bits / key → compromis entre mémoire et perf
            table_opts.set_bloom_filter(10.0, false);
            // 256 MB LRU block cache — réduit les I/O disque pour les hot data
            let cache = Cache::new_lru_cache(256 * 1024 * 1024);
            table_opts.set_block_cache(&cache);
            table_opts.set_cache_index_and_filter_blocks(true);
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
            // On met un bloom sur les CF typées index / lookup
            let mut opts = if name.ends_with(":blocks")
                || name.ends_with(":id2ts")
                || name.ends_with(":idx_blocks")
                || name.ends_with(":tips")
                || name.ends_with(":utxo")
                || name.ends_with(":utxo_spent")
            {
                cf_opts_with_bloom()
            } else {
                Options::default()
            };

            // Ces CF doivent aussi être créées si manquantes
            opts.create_if_missing(true);

            cf_descs.push(ColumnFamilyDescriptor::new(name.clone(), opts));
        }

        // 6) Ouverture DB + CF (MultiThreaded for dynamic CF creation support)
        let db = PmsDb::open_cf_descriptors(&db_opts, &path, cf_descs)
            .with_context(|| format!("open RocksDB at {}", path.display()))?;

        Ok(Self {
            db: Arc::new(db),
            tip_limit,
            prefix,
            checkpoint_interval,
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
    ];

    /// Ouvre un RocksDB avec les column families de **plusieurs prefixes** à la fois.
    /// Retourne un `Arc<DB>` partageable entre N `RocksStore` instances.
    pub async fn open_db_multi_prefix(
        path: &str,
        prefixes: &[String],
    ) -> Result<Arc<PmsDb>> {
        let path = PathBuf::from(path);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create_dir_all({})", path.display()))?;

        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        db_opts.increase_parallelism(num_cpus::get() as i32);
        db_opts.set_max_background_jobs(4);
        db_opts.set_level_compaction_dynamic_level_bytes(true);
        db_opts.set_write_buffer_size(64 * 1024 * 1024);
        db_opts.set_max_write_buffer_number(3);
        db_opts.set_target_file_size_base(64 * 1024 * 1024);

        fn cf_opts_with_bloom() -> Options {
            let mut opts = Options::default();
            opts.set_optimize_filters_for_hits(true);
            let mut table_opts = BlockBasedOptions::default();
            table_opts.set_bloom_filter(10.0, false);
            let cache = Cache::new_lru_cache(256 * 1024 * 1024);
            table_opts.set_block_cache(&cache);
            table_opts.set_cache_index_and_filter_blocks(true);
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
                let mut opts = if cf_name == "blocks"
                    || cf_name == "id2ts"
                    || cf_name == "idx_blocks"
                    || cf_name == "tips"
                    || cf_name == "utxo"
                    || cf_name == "utxo_spent"
                {
                    cf_opts_with_bloom()
                } else {
                    Options::default()
                };
                opts.create_if_missing(true);
                cf_descs.push(ColumnFamilyDescriptor::new(full, opts));
            }
        }

        // Also include any existing CFs that might belong to other prefixes
        if path.exists() {
            let existing = PmsDb::list_cf(&db_opts, &path).unwrap_or_default();
            // Re-collect declared names properly
            let mut declared_names: std::collections::HashSet<String> = std::collections::HashSet::new();
            declared_names.insert("default".to_string());
            for prefix in prefixes {
                for &cf_name in Self::CF_NAMES {
                    declared_names.insert(format!("{prefix}:{cf_name}"));
                }
            }

            for cf in existing {
                if cf != "default" && !declared_names.contains(&cf) {
                    let mut opts = Options::default();
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
        Self {
            db,
            tip_limit,
            prefix: prefix.into(),
            checkpoint_interval,
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

    // helper privé appelé après append_block_atomic
    pub(crate) fn trim_by_time(&self) -> Result<()> {
        if self.tip_limit == 0 {
            return Ok(());
        }

        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");

        // Collect newest first (exactly tip_limit à garder)
        let mut newest: Vec<Vec<u8>> = Vec::new();
        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::End) {
            let (k, _v) = kv?;
            newest.push(k.to_vec());
            if newest.len() >= self.tip_limit {
                break;
            } // ✅ >= au lieu de >
        }

        // Si on a ≤ tip_limit, rien à faire
        if newest.len() < self.tip_limit {
            return Ok(());
        }

        // Construis le set des clés à garder
        use std::collections::HashSet;
        let keep: HashSet<Vec<u8>> = newest.iter().cloned().collect();

        // Supprime toutes celles qui ne sont PAS dans keep
        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::Start) {
            let (k, _v) = kv?;
            let kvec = k.to_vec();
            if !keep.contains(&kvec) {
                self.db.delete_cf(&cf_time, &kvec)?;
                if let Some((_ts, bid)) = parse_time_index_key(&kvec) {
                    self.db.delete_cf(&cf_i2t, bid.as_bytes())?;
                }
            }
        }

        Ok(())
    }

    pub(crate) fn trim_tips(&self) -> anyhow::Result<()> {
        if self.tip_limit == 0 {
            return Ok(());
        }

        let cf_tips = self.cf("tips");

        // 1. Collecte toutes les tips : (id, ts)
        let mut tips: Vec<(String, i64)> = Vec::new();
        for kv in self.db.iterator_cf(&cf_tips, rocksdb::IteratorMode::Start) {
            let (k, v) = kv?;
            let id = String::from_utf8(k.to_vec())?;
            let ts = be_to_i64(&v)?; // on va écrire ce helper juste après
            tips.push((id, ts));
        }

        // 2. Trie par ts DESC (plus récent d'abord).
        tips.sort_by_key(|(_, ts)| Reverse(*ts));

        // 3. Si on est déjà <= tip_limit, rien à faire.
        if tips.len() <= self.tip_limit {
            return Ok(());
        }

        // 4. Construit un set des IDs qu'on garde.

        // 5. Supprime les autres dans cf_tips.
        for (id, _) in tips.into_iter().skip(self.tip_limit) {
            self.db.delete_cf(&cf_tips, id.as_bytes())?;
        }

        Ok(())
    }

    /// Récupère les timestamps (ms) pour une liste d'ids via la CF `id2ts`.
    pub async fn ts_for_ids(&self, ids: &[String]) -> Result<HashMap<String, i64>> {
        let cf_i2t = self.cf("id2ts");
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            if let Some(raw) = self.db.get_cf(&cf_i2t, id.as_bytes())? {
                if raw.len() == 8 {
                    let mut be = [0u8; 8];
                    be.copy_from_slice(&raw);
                    let ts = u64::from_be_bytes(be) as i64;
                    out.insert(id.clone(), ts);
                }
            }
        }
        Ok(out)
    }

    /// Paginated reverse-chronological scan of the `addr_activity` CF for a
    /// single address.  Returns `(block_ids, next_cursor)`.
    ///
    /// The cursor is `(ts, block_id, has_more)` — same shape as
    /// `recent_ids_by_time` so the activity endpoint can reuse pagination logic.
    pub async fn recent_ids_by_address(
        &self,
        addr: &str,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        use crate::helpers::{key_addr_activity, parse_addr_activity_key, prefix_addr_activity};

        let cf_aa = self.cf("addr_activity");
        let addr_len = addr.len();
        let prefix = prefix_addr_activity(addr);

        // Build the seek key (must outlive the iterator)
        let seek_key = if let (Some(ts), Some(id)) = (after_ts, after_id.as_deref()) {
            key_addr_activity(addr, ts, id)
        } else {
            // Seek just past the end of this address's prefix so that reverse
            // iteration starts at the newest entry.  The prefix is
            // [addr_bytes][0x00], so replacing 0x00 with 0x01 puts us one past.
            let mut end_key = prefix.clone();
            if let Some(last) = end_key.last_mut() {
                *last = 0x01;
            }
            end_key
        };

        let mut ids = Vec::with_capacity(limit + 1);
        let iter = self.db.iterator_cf(
            &cf_aa,
            IteratorMode::From(&seek_key, Direction::Reverse),
        );
        let mut skipped_cursor = false;

        for item in iter {
            let (k, _) = item?;

            // Stop if we've left this address's prefix
            if k.len() < prefix.len() || &k[..prefix.len()] != prefix.as_slice() {
                break;
            }

            let Some((ts, block_id)) = parse_addr_activity_key(&k, addr_len) else {
                continue;
            };

            // Skip the exact cursor entry
            if !skipped_cursor && after_ts.is_some() && after_id.is_some() {
                if Some(ts) == after_ts && Some(&block_id) == after_id.as_ref() {
                    skipped_cursor = true;
                    continue;
                }
                skipped_cursor = true;
            }

            ids.push(block_id);
            if ids.len() > limit {
                break;
            }
        }

        let has_more = ids.len() > limit;
        if has_more {
            ids.pop();
        }

        let next_cursor = if has_more {
            ids.last().and_then(|last_id| {
                // Look up ts from id2ts
                let cf_i2t = self.cf("id2ts");
                self.db
                    .get_cf(&cf_i2t, last_id.as_bytes())
                    .ok()
                    .flatten()
                    .and_then(|v| {
                        if v.len() == 8 {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(&v);
                            let ts = u64::from_be_bytes(b) as i64;
                            Some((ts, last_id.clone(), true))
                        } else {
                            None
                        }
                    })
            })
        } else {
            None
        };

        Ok((ids, next_cursor))
    }

    /// Write addr_activity index entries for a block with pre-computed addresses.
    /// Used for Encrypted payloads where addresses are known by the caller
    /// (e.g. the coordinator) but can't be extracted from the stored payload.
    pub fn write_addr_activity_entries(&self, block_id: &str, addresses: &[String]) -> Result<()> {
        if addresses.is_empty() {
            return Ok(());
        }
        let cf_aa = self.cf("addr_activity");
        let ts = crate::helpers::now_ms_i64();
        let mut batch = rocksdb::WriteBatch::default();
        for addr in addresses {
            let key = crate::helpers::key_addr_activity(addr, ts, block_id);
            batch.put_cf(&cf_aa, &key, b"");
        }
        self.db.write(batch)?;
        Ok(())
    }

    /// Paginated reverse-chronological scan of the `addr_type_activity` CF for a
    /// single address filtered by one or more activity categories.
    ///
    /// When a single category is given, it's a simple prefix scan.
    /// When multiple categories are given, we do a k-way merge across category
    /// prefixes (k ≤ 9) picking the newest entry each round.
    pub async fn recent_ids_by_address_and_categories(
        &self,
        addr: &str,
        categories: &[u8],
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        use crate::helpers::{
            key_addr_type_activity, parse_addr_type_activity_key, prefix_addr_type_activity,
        };

        if categories.is_empty() {
            return Ok((vec![], None));
        }

        let cf_ata = self.cf("addr_type_activity");
        let addr_len = addr.len();

        // For a single category, use a simple prefix scan (common case)
        if categories.len() == 1 {
            let cat = categories[0];
            let prefix = prefix_addr_type_activity(addr, cat);

            let seek_key =
                if let (Some(ts), Some(ref id)) = (after_ts, after_id.as_deref()) {
                    key_addr_type_activity(addr, cat, ts, id)
                } else {
                    let mut end_key = prefix.clone();
                    // [addr][0x00][cat] → bump last byte to go past the prefix
                    if let Some(last) = end_key.last_mut() {
                        *last = cat.wrapping_add(1);
                    }
                    end_key
                };

            let mut ids = Vec::with_capacity(limit + 1);
            let iter = self.db.iterator_cf(
                &cf_ata,
                IteratorMode::From(&seek_key, Direction::Reverse),
            );
            let mut skipped_cursor = false;

            for item in iter {
                let (k, _) = item?;
                if k.len() < prefix.len() || &k[..prefix.len()] != prefix.as_slice() {
                    break;
                }
                let Some((_cat, ts, block_id)) =
                    parse_addr_type_activity_key(&k, addr_len)
                else {
                    continue;
                };

                if !skipped_cursor && after_ts.is_some() && after_id.is_some() {
                    if Some(ts) == after_ts && Some(&block_id) == after_id.as_ref() {
                        skipped_cursor = true;
                        continue;
                    }
                    skipped_cursor = true;
                }

                ids.push(block_id);
                if ids.len() > limit {
                    break;
                }
            }

            return self.finalize_ids_cursor(ids, limit).await;
        }

        // Multi-category: k-way merge across category prefixes
        // Build one iterator per category, each positioned at the right start
        struct CatIter<'a> {
            prefix: Vec<u8>,
            iter: rocksdb::DBIteratorWithThreadMode<
                'a,
                DBWithThreadMode<MultiThreaded>,
            >,
            current: Option<(i64, String)>, // (ts, block_id) of the peeked entry
            addr_len: usize,
        }

        let mut iters: Vec<CatIter<'_>> = Vec::with_capacity(categories.len());

        for &cat in categories {
            let prefix = prefix_addr_type_activity(addr, cat);
            let seek_key =
                if let (Some(ts), Some(ref id)) = (after_ts, after_id.as_deref()) {
                    key_addr_type_activity(addr, cat, ts, id)
                } else {
                    let mut end_key = prefix.clone();
                    if let Some(last) = end_key.last_mut() {
                        *last = cat.wrapping_add(1);
                    }
                    end_key
                };

            let iter = self.db.iterator_cf(
                &cf_ata,
                IteratorMode::From(&seek_key, Direction::Reverse),
            );

            let mut ci = CatIter {
                prefix,
                iter,
                current: None,
                addr_len,
            };
            // Advance to first valid entry (skip exact cursor if needed)
            ci.advance(after_ts, after_id.as_deref());
            if ci.current.is_some() {
                iters.push(ci);
            }
        }

        impl CatIter<'_> {
            fn advance(&mut self, skip_ts: Option<i64>, skip_id: Option<&str>) {
                loop {
                    let Some(Ok((k, _))) = self.iter.next() else {
                        self.current = None;
                        return;
                    };
                    if k.len() < self.prefix.len()
                        || &k[..self.prefix.len()] != self.prefix.as_slice()
                    {
                        self.current = None;
                        return;
                    }
                    let Some((_cat, ts, block_id)) =
                        parse_addr_type_activity_key(&k, self.addr_len)
                    else {
                        continue;
                    };
                    // Skip exact cursor entry
                    if let (Some(sts), Some(sid)) = (skip_ts, skip_id) {
                        if ts == sts && block_id == sid {
                            continue;
                        }
                    }
                    self.current = Some((ts, block_id));
                    return;
                }
            }
            fn advance_next(&mut self) {
                self.advance(None, None);
            }
        }

        // Merge: pick the iterator with the highest timestamp each round
        let mut ids = Vec::with_capacity(limit + 1);
        while !iters.is_empty() && ids.len() <= limit {
            // Find iterator with the newest entry
            let best_idx = iters
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    let (ts_a, id_a) = a.current.as_ref().unwrap();
                    let (ts_b, id_b) = b.current.as_ref().unwrap();
                    ts_a.cmp(ts_b).then_with(|| id_a.cmp(id_b))
                })
                .map(|(i, _)| i)
                .unwrap();

            let (_, block_id) = iters[best_idx].current.clone().unwrap();
            ids.push(block_id);

            iters[best_idx].advance_next();
            if iters[best_idx].current.is_none() {
                iters.swap_remove(best_idx);
            }
        }

        self.finalize_ids_cursor(ids, limit).await
    }

    /// Shared logic for building the pagination cursor from a collected ids vec.
    async fn finalize_ids_cursor(
        &self,
        mut ids: Vec<String>,
        limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        let has_more = ids.len() > limit;
        if has_more {
            ids.pop();
        }
        let next_cursor = if has_more {
            ids.last().and_then(|last_id| {
                let cf_i2t = self.cf("id2ts");
                self.db
                    .get_cf(&cf_i2t, last_id.as_bytes())
                    .ok()
                    .flatten()
                    .and_then(|v| {
                        if v.len() == 8 {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(&v);
                            let ts = u64::from_be_bytes(b) as i64;
                            Some((ts, last_id.clone(), true))
                        } else {
                            None
                        }
                    })
            })
        } else {
            None
        };
        Ok((ids, next_cursor))
    }

    pub fn bootstrap_once_for_production(&self) -> anyhow::Result<()> {
        self.db.flush()?; // CF prêtes
        self.db.compact_range::<&[u8], &[u8]>(None, None); // compact au boot (optionnel)
        Ok(())
    }

    /// Lance une tâche de maintenance en arrière-plan :
    /// - flush WAL périodique
    /// - compaction périodique
    /// - log des stats RocksDB
    /// - checkpoint + rotation (snapshots) 1 fois / 24h
    ///
    /// Elle s'arrête proprement quand `cancel.cancel()` est appelé.
    pub fn spawn_background_maintenance(
        self: Arc<Self>,
        cancel: CancellationToken,
        compact_every: Duration,
        flush_every: Duration,
        stats_every: Duration,
    ) -> JoinHandle<()> {
        // Interval pour les checkpoints (celui configuré)
        let checkpoint_every = self.checkpoint_interval;
        // Dossier de backup :
        // - en prod tu mettras typiquement /var/backups/pms
        // - en dev: ./backups/pms
        let backup_root = std::env::var("PMS_BACKUP_ROOT").unwrap_or_else(|_| {
            let p = std::path::Path::new("./crates");
            if p.exists() && p.is_dir() {
                // On est à la racine du workspace
                "./backups/pms".to_string()
            } else if std::path::Path::new("../../crates").exists() {
                // On est probablement dans crates/pms-server
                "../../backups/pms".to_string()
            } else {
                // Fallback
                "./backups/pms".to_string()
            }
        });

        tokio::spawn(async move {
            // Timers périodiques
            let mut flush_tick = interval(flush_every);
            flush_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            let mut compact_tick = interval(compact_every);
            compact_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            let mut stats_tick = interval(stats_every);
            stats_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            let mut checkpoint_tick = interval(checkpoint_every);
            checkpoint_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            eprintln!(
                "[rocks] background maintenance started (flush={:?}, compact={:?}, stats={:?}, checkpoint={:?}, backup_root={})",
                flush_every, compact_every, stats_every, checkpoint_every, backup_root,
            );

            loop {
                tokio::select! {
                    // Signal d’arrêt propre (Server::run appelle cancel.cancel())
                    _ = cancel.cancelled() => {
                        eprintln!("[rocks] background maintenance cancelled, exiting");
                        break;
                    }

                    // Flush WAL (évite d’avoir un WAL trop gros, améliore la durabilité)
                    _ = flush_tick.tick() => {
                        if let Err(e) = self.flush_wal().await {
                            eprintln!("[rocks] flush_wal failed: {e:#}");
                        }
                    }

                    // Compaction de toutes les CF (réduction fragmentation, taille disque)
                    _ = compact_tick.tick() => {
                        if let Err(e) = self.compact_all().await {
                            eprintln!("[rocks] compact_all failed: {e:#}");
                        }
                    }

                    // Log de stats RocksDB (diagnostic: taille, compaction, etc.)
                    _ = stats_tick.tick() => {
                        if let Err(e) = self.log_stats().await {
                            eprintln!("[rocks] log_stats failed: {e:#}");
                        }
                    }

                    // Checkpoint + rotation (snapshots de sécurité)
                    _ = checkpoint_tick.tick() => {
                        if let Err(e) = self.create_checkpoint(&backup_root) {
                            eprintln!("[rocks] create_checkpoint failed: {e:#}");
                        } else if let Err(e) = rotate_checkpoints(&backup_root, 7) {
                            eprintln!("[rocks] rotate_checkpoints failed: {e:#}");
                        }
                    }
                }
            }

            eprintln!("[rocks] background maintenance stopped");
        })
    }

    pub async fn open_read_only(
        path: &str,
        tip_limit: usize,
        _prefix: &str,
    ) -> anyhow::Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(false);

        // false => pas d'erreur si des WAL existent, pas de lock exclusif
        let db = PmsDb::open_for_read_only(&opts, path, false)
            .map_err(|e| anyhow::anyhow!("open_read_only: {e}"))?;

        Ok(Self {
            db: std::sync::Arc::new(db),
            tip_limit,
            prefix: "".to_string(),
            checkpoint_interval: Duration::from_secs(24 * 3600),
        })
    }

    pub fn refresh_from_primary(&self) -> anyhow::Result<()> {
        self.db
            .try_catch_up_with_primary()
            .map_err(|e| anyhow::anyhow!("catch_up: {e}"))
    }

    pub async fn open_secondary(
        primary_path: &str,
        secondary_path: &str,
        tip_limit: usize,
        prefix: impl Into<String>,
    ) -> Result<Self> {
        let prefix = prefix.into();

        let primary = PathBuf::from(primary_path);
        let secondary = PathBuf::from(secondary_path);

        std::fs::create_dir_all(&secondary)
            .with_context(|| format!("create_dir_all({})", secondary.display()))?;

        let mut db_opts = Options::default();
        db_opts.create_if_missing(false);
        db_opts.create_missing_column_families(false);

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
            "nft_ownership",
            "nfts_by_owner",
            "nft_block_ids",
            "compliance_frozen",
            "compliance_log",
        ]
        .into_iter()
        .map(|s| format!("{prefix}:{s}"))
        .collect();

        // Juste les noms de CF
        let mut cf_names: Vec<String> = Vec::with_capacity(required.len() + 1);
        cf_names.push("default".to_string());
        cf_names.extend(required.into_iter());

        let db = PmsDb::open_cf_as_secondary(&db_opts, &primary, &secondary, &cf_names).with_context(
            || {
                format!(
                    "open RocksDB secondary at {} (primary={})",
                    secondary.display(),
                    primary.display()
                )
            },
        )?;

        Ok(Self {
            db: Arc::new(db),
            tip_limit,
            prefix,
            checkpoint_interval: Duration::from_secs(24 * 3600),
        })
    }
}

#[async_trait::async_trait]
impl DagStorage for RocksStore {
    async fn put_block(&self, b: &StoredBlock) -> Result<PutResult> {
        self.put_block(b).await
    }

    async fn get_block(&self, id: &str) -> Result<Option<StoredBlock>> {
        let cf_blocks = self.cf("blocks");
        if let Some(v) = self.db.get_cf(&cf_blocks, id.as_bytes())? {
            let sb: StoredBlock = serde_json::from_slice(&v)?;
            Ok(Some(sb))
        } else {
            Ok(None)
        }
    }

    async fn add_child_edge(&self, parent: &str, child: &str) -> Result<()> {
        let cf_count = self.cf("children_count");
        let cf_set = self.cf("children_set");

        // 1. bump count
        let key_parent = parent.as_bytes();
        let cur = self.db.get_cf(&cf_count, key_parent)?;
        let newcount = match cur {
            Some(v) if v.len() == 8 => {
                let n = le_to_u64(&v);
                n + 1
            }
            _ => 1u64,
        };
        self.db.put_cf(&cf_count, key_parent, u64_to_le(newcount))?;

        // 2. record edge parent->child
        // concat key: parent || 0x00 || child
        let mut edge_key = Vec::with_capacity(parent.len() + 1 + child.len());
        edge_key.extend_from_slice(parent.as_bytes());
        edge_key.push(0);
        edge_key.extend_from_slice(child.as_bytes());
        self.db.put_cf(&cf_set, edge_key, b"")?;

        Ok(())
    }
    async fn children_count(&self, id: &str) -> Result<u64> {
        let cf_count = self.cf("children_count");
        if let Some(v) = self.db.get_cf(&cf_count, id.as_bytes())? {
            if v.len() == 8 {
                return Ok(le_to_u64(&v));
            }
        }
        Ok(0)
    }

    async fn add_tip(&self, id: &str) -> Result<()> {
        let cf_tips = self.cf("tips");
        let ts = now_ms_i64();
        self.db.put_cf(&cf_tips, id.as_bytes(), ts_to_be(ts))?;
        self.trim_tips()?;
        Ok(())
    }

    async fn remove_tip(&self, id: &str) -> Result<()> {
        let cf_tips = self.cf("tips");
        self.db.delete_cf(&cf_tips, id.as_bytes())?;
        Ok(())
    }

    async fn top_tips(&self, limit: usize) -> Result<Vec<String>> {
        let cf_tips = self.cf("tips");
        // collect all tips
        let mut v: Vec<(String, i64)> = Vec::new();
        for kv in self.db.iterator_cf(&cf_tips, rocksdb::IteratorMode::Start) {
            let (k, val) = kv?;
            let id = String::from_utf8(k.to_vec())?;
            let ts = if val.len() == 8 { be_to_ts(&val) } else { 0 };
            v.push((id, ts));
        }
        // sort desc by ts
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
        // take limit
        Ok(v.into_iter().take(limit).map(|(id, _)| id).collect())
    }

    async fn all_block_ids(&self) -> Result<Vec<String>> {
        let cf_idx = self.cf("idx_blocks");
        let mut out = Vec::new();
        for kv in self.db.iterator_cf(&cf_idx, rocksdb::IteratorMode::Start) {
            let (k, _v) = kv?;
            out.push(String::from_utf8(k.to_vec())?);
        }
        Ok(out)
    }

    async fn block_count(&self) -> Result<u64> {
        let cf_idx = self.cf("idx_blocks");
        let count = self
            .db
            .iterator_cf(&cf_idx, rocksdb::IteratorMode::Start)
            .count();
        Ok(count as u64)
    }

    async fn export_json(&self) -> anyhow::Result<String> {
        let cf_idx = self.cf("idx_blocks");
        let cf_blocks = self.cf("blocks");

        // 1. collect all ids from idx_blocks CF
        let mut ids = Vec::new();
        for kv in iter_cf_all(&self.db, &cf_idx) {
            let (k, _v) = kv?;
            let id = String::from_utf8(k.to_vec())?;
            ids.push(id);
        }

        // 2. fetch StoredBlock for each id
        let mut out: Vec<StoredBlock> = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(raw) = self.db.get_cf(&cf_blocks, id.as_bytes())? {
                let sb: StoredBlock = serde_json::from_slice(&raw)?;
                out.push(sb);
            }
        }

        // 3. pretty JSON
        Ok(serde_json::to_string_pretty(&out)?)
    }

    async fn export_namespace(&self) -> anyhow::Result<String> {
        let cf_blocks = self.cf("blocks");
        let mut all_blocks = Vec::new();

        for kv in iter_cf_all(&self.db, &cf_blocks) {
            let (_k, v) = kv?;
            let sb: StoredBlock = serde_json::from_slice(&v)?;
            all_blocks.push(sb);
        }

        Ok(serde_json::to_string(&all_blocks)?)
    }

    /// Importe un dump JSON (issu de `export_json`) :
    /// - pour chaque block :
    ///   - `put_block` (persist + index)
    ///   - `add_tip` (pour qu’il soit éligible à la sélection de parents)
    ///   - pour chaque parent :
    ///       - `add_child_edge(parent, id)` (reconstruit les compteurs)
    ///       - `remove_tip(parent)` (un parent référencé n’est plus un tip)
    ///
    /// *Idempotence*: réimporter un même dump écrase juste la valeur du block et reconstruit les index.
    async fn import_json(&self, dump: &str) -> anyhow::Result<()> {
        // 1. parse
        let blocks: Vec<StoredBlock> = serde_json::from_str(dump)?;

        let cf_blocks = self.cf("blocks");
        let cf_idx = self.cf("idx_blocks");
        let cf_tips = self.cf("tips");
        let cf_children_cnt = self.cf("children_count");
        let cf_children_set = self.cf("children_set");

        // Sanity: ensure CF exist (they should, since new() created them).
        let _ = (&cf_blocks, &cf_idx, &cf_tips, &cf_children_cnt, &cf_children_set);

        // 2. insert / update idx
        for b in &blocks {
            // réutilise la logique put_block()
            let _ = self.put_block(b).await?;
        }

        // 3. rebuild parent->child edges & child counters
        //    (idempotent : on overwrite counters by recomputing)
        //
        //    ATTENTION: contrairement à Redis, là si tu ré-importes
        //    plusieurs fois tu vas re-incrémenter.
        //
        //    Pour coller à Redis "reconstruit les index", on veut repartir de 0.
        //    => On wipe children_count + children_set avant.
        //
        {
            // wipe children_count CF
            for kv in iter_cf_all(&self.db, &self.cf("children_count")) {
                let (k, _) = kv?;
                self.db.delete_cf(&self.cf("children_count"), &k)?;
            }
            // wipe children_set CF
            for kv in iter_cf_all(&self.db, &self.cf("children_set")) {
                let (k, _) = kv?;
                self.db.delete_cf(&self.cf("children_set"), &k)?;
            }
        }

        for b in &blocks {
            for p in &b.parents {
                self.add_child_edge(p, &b.id).await?;
            }
        }

        // 4. rebuild tips set:
        //    wipe tips CF, puis:
        //    - add_tip(child) pour tous les blocs
        //    - remove_tip(parent) pour chaque parent
        {
            // wipe tips
            for kv in iter_cf_all(&self.db, &cf_tips) {
                let (k, _) = kv?;
                self.db.delete_cf(&cf_tips, &k)?;
            }

            // tous en tip
            for b in &blocks {
                self.add_tip(&b.id).await?;
            }
            // puis retire les parents
            for b in &blocks {
                for p in &b.parents {
                    self.remove_tip(p).await?;
                }
            }
        }

        // NOTE: on NE reconstruit PAS ici:
        // - by_time (ordre insertion)
        // - id2ts (timestamp pour recent_ids_by_time)
        // - final / last_ms
        //
        // c'est pareil que Redis import_json(): il ne touchait pas la finalité,
        // ni les ZSET temporels.

        Ok(())
    }

    async fn append_block_atomic(&self, b: &StoredBlock) -> Result<bool> {
        // délégation directe → évite la récursion infinie car on appelle
        // la méthode inhérente (même nom, mais contexte différent)
        RocksStore::append_block_atomic(self, b).await
    }

    async fn load_final(&self) -> Result<Vec<String>> {
        let cf_final = self.cf("final");
        let mut out = Vec::new();
        for kv in self.db.iterator_cf(&cf_final, rocksdb::IteratorMode::Start) {
            let (k, _v) = kv?;
            out.push(String::from_utf8(k.to_vec())?);
        }
        Ok(out)
    }

    async fn load_last_milestone(&self) -> Result<Option<String>> {
        let cf_ms = self.cf("last_ms");
        if let Some(v) = self.db.get_cf(&cf_ms, b"last")? {
            Ok(Some(String::from_utf8(v.to_vec())?))
        } else {
            Ok(None)
        }
    }

    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>> {
        let cf_time = self.cf("by_time");
        let mut out = Vec::new();

        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::End) {
            let (k, _v) = kv?;
            if let Some((_ts, id)) = parse_time_index_key(&k) {
                out.push(id);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    async fn recent_ids_by_time(
        &self,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> anyhow::Result<(Vec<String>, Option<(i64, String, bool)>)> {
        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");

        // Point de départ pour l’itérateur (Reverse).
        let start_mode = if let (Some(ts), Some(id)) = (after_ts, after_id.clone()) {
            let k = key_time_index(ts, &id);
            IteratorMode::From(&k.clone(), Direction::Reverse)
        } else {
            IteratorMode::End
        };

        let mut ids = Vec::with_capacity(limit + 1);
        let mut iter = self.db.iterator_cf(&cf_time, start_mode);

        // Si on a un curseur, la 1re entrée peut être exactement (ts,id) → sauter
        if after_ts.is_some() && after_id.is_some() {
            if let Some(Ok((k, _))) = iter.next() {
                if let Some((_ts, kid)) = parse_time_index_key(&k) {
                    if Some(kid) != after_id {
                        // on a commencé "avant" la clé exacte, garder cet élément
                        // sinon on l’a skip en consommant déjà l’item égal
                        ids.push(parse_time_index_key(&k).unwrap().1);
                    }
                }
            }
        }

        // Poursuivre jusqu’à limit+1 (pour savoir s’il y a une page suivante)
        while ids.len() < limit + 1 {
            match iter.next() {
                Some(Ok((k, _))) => {
                    if let Some((_ts, id)) = parse_time_index_key(&k) {
                        ids.push(id);
                    }
                }
                _ => break,
            }
        }

        let has_more = ids.len() > limit;
        if has_more {
            ids.pop();
        } // garder exactement `limit`

        // Construire next_cursor depuis le dernier id renvoyé
        let next_cursor = ids.last().and_then(|last_id| {
            self.db
                .get_cf(&cf_i2t, last_id.as_bytes())
                .ok()
                .flatten()
                .and_then(|v| {
                    if v.len() == 8 {
                        let mut b = [0u8; 8];
                        b.copy_from_slice(&v);
                        let ts = u64::from_be_bytes(b) as i64;
                        Some((ts, last_id.clone(), has_more))
                    } else {
                        None
                    }
                })
        });

        Ok((ids, next_cursor))
    }

    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>> {
        let cf_blocks = self.cf("blocks");
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(v) = self.db.get_cf(&cf_blocks, id.as_bytes())? {
                if let Ok(sb) = serde_json::from_slice::<StoredBlock>(&v) {
                    out.push(WireBlock {
                        id: sb.id,
                        parents: sb.parents,
                        payload_json: sb.payload_json,
                        nonce: sb.nonce,
                        network_id: sb.network_id,
                        protocol_version: sb.protocol_version,
                        signer_pk_hex: sb.signer_pk_hex,
                        signature_hex: sb.signature_hex,
                        metadata: sb.metadata,
                    });
                }
            }
        }
        Ok(out)
    }

    async fn persist_final(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let cf_final = self.cf("final");
        for id in ids {
            self.db.put_cf(&cf_final, id.as_bytes(), b"")?;
        }
        Ok(())
    }

    async fn persist_last_milestone(&self, id: &str) -> Result<()> {
        let cf_ms = self.cf("last_ms");
        self.db.put_cf(&cf_ms, b"last", id.as_bytes())?;
        Ok(())
    }

    async fn append_block_atomic_with_utxo(
        &self,
        b: &StoredBlock,
        delta: Option<&UtxoDelta>,
    ) -> Result<bool> {
        use rocksdb::WriteBatch;

        // 0) bloc déjà là ? → idempotent
        let cf_blocks = self.cf("blocks");
        if self.db.get_cf(&cf_blocks, b.id.as_bytes())?.is_some() {
            return Ok(false);
        }

        // 1) prépare le batch global
        let mut batch = WriteBatch::default();

        // 1.a) UTXO Delta
        if let Some(d) = delta {
            let cf_utxo = self.cf("utxo");
            let cf_utxo_spent = self.cf("utxo_spent");

            // SPENDS
            for (txid, idx) in &d.spend {
                let key = make_utxo_key(txid, *idx);
                batch.delete_cf(&cf_utxo, &key);
                batch.put_cf(&cf_utxo_spent, &key, b.id.as_bytes());
            }

            // CREATES
            for (txid, idx, addr, amt, asset_id) in &d.create {
                let key = make_utxo_key(txid, *idx);

                #[derive(Serialize)]
                struct OutVal<'a> {
                    addr: &'a str,
                    amt: &'a str,
                    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ast")]
                    asset_id: Option<&'a str>,
                }

                let val = OutVal { addr, amt, asset_id: asset_id.as_deref() };
                let json = serde_json::to_vec(&val)?;
                batch.put_cf(&cf_utxo, &key, &json);
            }
        }

        // 1.b) Indices DAG
        self.apply_dag_indices(&mut batch, b)?;

        // 1.c) Per-address activity indexes (addr_activity + addr_type_activity)
        if let Some(pjson) = &b.payload_json {
            if let Ok(env) = serde_json::from_str::<pms_types_payload::PayloadEnvelope>(pjson) {
                if let pms_types_payload::PayloadEnvelope::Plain(ref plain) = env {
                    let ts = crate::helpers::now_ms_i64();

                    // Untyped index (addr_activity)
                    let addrs = crate::helpers::extract_involved_addresses(plain);
                    if !addrs.is_empty() {
                        let cf_aa = self.cf("addr_activity");
                        for addr in &addrs {
                            let key = crate::helpers::key_addr_activity(addr, ts, &b.id);
                            batch.put_cf(&cf_aa, &key, b"");
                        }
                    }

                    // Typed index (addr_type_activity)
                    let typed = crate::helpers::extract_involved_with_category(plain);
                    if !typed.is_empty() {
                        let cf_ata = self.cf("addr_type_activity");
                        for (addr, cat) in &typed {
                            let key = crate::helpers::key_addr_type_activity(
                                addr,
                                cat.as_byte(),
                                ts,
                                &b.id,
                            );
                            batch.put_cf(&cf_ata, &key, b"");
                        }
                    }
                }
            }
        }

        // 2) write atomique
        self.db.write(batch)?;

        // 3) trim tips only (by_time/id2ts grow unbounded for activity API)
        self.trim_tips()?;

        Ok(true)
    }

    async fn recent_ids_by_address(
        &self,
        addr: &str,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        // Delegate to the inherent method
        self.recent_ids_by_address(addr, after_ts, after_id, limit)
            .await
    }

    async fn recent_ids_by_address_and_categories(
        &self,
        addr: &str,
        categories: &[u8],
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        self.recent_ids_by_address_and_categories(addr, categories, after_ts, after_id, limit)
            .await
    }
}

fn make_utxo_key(txid: &str, index: u32) -> Vec<u8> {
    format!("{txid}#{index}").into_bytes()
}

fn iter_cf_all<'a>(
    db: &'a PmsDb,
    cf: &impl rocksdb::AsColumnFamilyRef,
) -> impl Iterator<Item = anyhow::Result<(Box<[u8]>, Box<[u8]>)>> + 'a {
    db.iterator_cf(cf, IteratorMode::Start).map(|res| {
        let (k, v) = res?;
        Ok((k, v))
    })
}
