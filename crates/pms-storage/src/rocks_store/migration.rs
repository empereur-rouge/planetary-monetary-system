use crate::helpers::{
    extract_involved_addresses, extract_involved_with_category, key_addr_activity,
    key_addr_type_activity, key_time_index, ts_to_be,
};
use crate::rocks_store::store::RocksStore;
use crate::{CURRENT_VER, DagStorage, MigError, StoredBlock};
use anyhow::anyhow;
use pms_types_payload::PayloadEnvelope;
use rocksdb::BoundColumnFamily;
use std::sync::Arc;

impl RocksStore {
    // --- helpers internes versionning ------------------------------------

    fn cf_ver(&self) -> Arc<BoundColumnFamily<'_>> {
        self.cf("ver")
    }

    pub async fn get_version(&self) -> anyhow::Result<i64> {
        if let Some(v) = self.db.get_cf(&self.cf_ver(), b"ver")? {
            let s = String::from_utf8(v)?;
            let parsed: i64 = s.parse().unwrap_or(0);
            Ok(parsed)
        } else {
            Ok(0)
        }
    }

    async fn set_version(&self, v: i64) -> anyhow::Result<()> {
        let s = v.to_string();
        self.db.put_cf(&self.cf_ver(), b"ver", s.as_bytes())?;
        Ok(())
    }

    // --- DAG version (SemVer) stockée dans CF "ver" -----------------------

    /// Lit la version DAG depuis RocksDB. Retourne "1.0.0" si absente (DB existante).
    pub async fn get_dag_version(&self) -> anyhow::Result<String> {
        if let Some(v) = self.db.get_cf(&self.cf_ver(), b"dag_version")? {
            let s = String::from_utf8(v)?;
            Ok(s)
        } else {
            Ok("1.0.0".to_string())
        }
    }

    /// Écrit la version DAG dans RocksDB.
    pub async fn set_dag_version(&self, version: &str) -> anyhow::Result<()> {
        self.db
            .put_cf(&self.cf_ver(), b"dag_version", version.as_bytes())?;
        Ok(())
    }

    // --- API publique équivalente à RedisStore::ensure_schema  -----------

    pub async fn ensure_schema(&self) -> Result<(), MigError> {
        // lire version courante (défaut = 0)
        let mut v = self.get_version().await.map_err(MigError::Any)?;

        while v < CURRENT_VER {
            match v {
                0 => self.mig_0_to_1().await?,
                1 => self.mig_1_to_2().await?,
                2 => self.mig_2_to_3().await?,
                3 => self.mig_3_to_4().await?,
                4 => self.mig_4_to_5().await?,
                5 => self.mig_5_to_6().await?,
                6 => self.mig_6_to_7().await?,
                7 => self.mig_7_to_8().await?,
                8 => self.mig_8_to_9().await?,
                9 => self.mig_9_to_10().await?,
                _ => return Err(MigError::Unexpected(v)),
            }
            v += 1;

            // si migration OK → on persiste la nouvelle version
            self.set_version(v).await.map_err(MigError::Any)?;
        }

        Ok(())
    }

    // --- migrations ------------------------------------------------------

    // Migration 0 -> 1 :
    // Redis faisait un SADD/SREM "__init__" pour forcer l'init de l'index des blocks.
    // Ici on simule la même chose en écrivant puis supprimant une clé factice
    // dans la CF idx_blocks. Idempotent et cheap.
    async fn mig_0_to_1(&self) -> std::result::Result<(), MigError> {
        let cf_idx = self.cf("idx_blocks");

        // Avant: .map_err(MigError::Any)?
        // Après: closure qui convertit rocksdb::Error -> anyhow::Error -> MigError
        self.db
            .put_cf(&cf_idx, b"__init__", b"")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        self.db
            .delete_cf(&cf_idx, b"__init__")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        Ok(())
    }

    // Migration 1 -> 2 :
    //
    // Objectif identique à Redis:
    //  - reconstruire l'état "tips" en considérant tous les blocs persistés
    //  - retirer les parents des tips
    //
    // Algo:
    //  for each block B:
    //     add_tip(B.id)
    //     for each parent P in B.parents:
    //         remove_tip(P)
    //
    // On NE reconstruit PAS ici children_count / children_set pour éviter
    // les doubles-incréments si la migration est rejouée (même choix que Redis).
    async fn mig_1_to_2(&self) -> std::result::Result<(), MigError> {
        let ids = self.all_block_ids().await.map_err(MigError::Any)?;
        let total = ids.len();
        tracing::info!("Migration 1→2: rebuilding tips for {} blocks", total);

        for (i, id) in ids.iter().enumerate() {
            if i > 0 && i % 10_000 == 0 {
                tracing::info!(
                    "Migration 1→2: processed {}/{} blocks ({:.1}%)",
                    i,
                    total,
                    (i as f64 / total as f64) * 100.0
                );
            }
            let maybe_b = self.get_block(id).await.map_err(MigError::Any)?;
            let b = match maybe_b {
                Some(b) => b,
                None => continue,
            };

            self.add_tip(&b.id).await.map_err(MigError::Any)?;

            for p in &b.parents {
                let _ = self.remove_tip(p).await;
            }
        }

        tracing::info!("Migration 1→2: completed ({} blocks processed)", total);
        Ok(())
    }

    // Migration 2 -> 3 :
    //
    // Rebuild `by_time` and `id2ts` entries that were lost due to `trim_by_time()`.
    // Previous versions trimmed these CFs to `tip_limit` entries after every block insert,
    // which prevented the Activity API from scanning full DAG history.
    //
    // For each block ID in `idx_blocks` that is missing from `id2ts`, we write a
    // synthetic monotonically-increasing timestamp (starting at 1). These synthetic
    // timestamps sort before any real wall-clock timestamps (~1.7e12), so migrated
    // blocks will appear as "oldest" in reverse-chronological scans — which is correct
    // since they are the blocks that were trimmed (i.e., older blocks).
    //
    // Idempotent: skips blocks that already have an `id2ts` entry.
    async fn mig_2_to_3(&self) -> std::result::Result<(), MigError> {
        let cf_idx = self.cf("idx_blocks");
        let cf_i2t = self.cf("id2ts");
        let cf_time = self.cf("by_time");

        // Count total blocks for progress reporting
        let mut total = 0usize;
        let mut missing = 0usize;
        let mut synthetic_ts: i64 = 1;

        tracing::info!("Migration 2→3: scanning idx_blocks to rebuild by_time/id2ts...");

        for kv in self.db.iterator_cf(&cf_idx, rocksdb::IteratorMode::Start) {
            let (k, _) = kv.map_err(|e| MigError::Any(anyhow!(e)))?;
            total += 1;

            let id = String::from_utf8(k.to_vec()).map_err(|e| MigError::Any(anyhow!(e)))?;

            // Skip sentinel key from migration 0→1
            if id == "__init__" {
                continue;
            }

            // Check if id2ts already has this block
            if self
                .db
                .get_cf(&cf_i2t, id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
                .is_some()
            {
                continue;
            }

            // Write synthetic timestamp
            let time_key = key_time_index(synthetic_ts, &id);
            self.db
                .put_cf(&cf_time, &time_key, b"")
                .map_err(|e| MigError::Any(anyhow!(e)))?;
            self.db
                .put_cf(&cf_i2t, id.as_bytes(), ts_to_be(synthetic_ts))
                .map_err(|e| MigError::Any(anyhow!(e)))?;

            synthetic_ts += 1;
            missing += 1;

            if missing > 0 && missing.is_multiple_of(10_000) {
                tracing::info!(
                    "Migration 2→3: rebuilt {missing} entries so far (scanned {total})..."
                );
            }
        }

        tracing::info!(
            "Migration 2→3: completed — rebuilt {missing} missing entries out of {total} blocks"
        );
        Ok(())
    }

    // Migration 3 -> 4 :
    //
    // Backfill the `addr_activity` column family for all existing blocks.
    // For each block in `by_time` (which has all blocks thanks to migration 2→3),
    // parse its payload, extract involved addresses, and write entries.
    //
    // Idempotent: addr_activity entries are keyed by (addr, ts, block_id),
    // so re-writing is harmless (same key = same value).
    async fn mig_3_to_4(&self) -> std::result::Result<(), MigError> {
        let cf_time = self.cf("by_time");
        let cf_blocks = self.cf("blocks");
        let cf_i2t = self.cf("id2ts");
        let cf_aa = self.cf("addr_activity");

        let mut total = 0usize;
        let mut indexed = 0usize;

        tracing::info!("Migration 3→4: backfilling addr_activity index...");

        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::Start) {
            let (time_key, _) = kv.map_err(|e| MigError::Any(anyhow!(e)))?;
            total += 1;

            // Parse the time index key to get (ts, block_id)
            let Some((_ts, block_id)) = crate::helpers::parse_time_index_key(&time_key) else {
                continue;
            };

            // Fetch the block
            let Some(block_bytes) = self
                .db
                .get_cf(&cf_blocks, block_id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
            else {
                continue;
            };

            let Ok(sb) = serde_json::from_slice::<StoredBlock>(&block_bytes) else {
                continue;
            };

            // Parse payload
            let Some(pjson) = &sb.payload_json else {
                continue;
            };
            let Ok(env) = serde_json::from_str::<PayloadEnvelope>(pjson) else {
                continue;
            };
            let PayloadEnvelope::Plain(ref plain) = env else {
                continue;
            };

            let addrs = extract_involved_addresses(plain);
            if addrs.is_empty() {
                continue;
            }

            // Get the block's timestamp from id2ts
            let ts = match self
                .db
                .get_cf(&cf_i2t, block_id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
            {
                Some(v) if v.len() == 8 => {
                    let mut be = [0u8; 8];
                    be.copy_from_slice(&v);
                    u64::from_be_bytes(be) as i64
                }
                _ => continue,
            };

            // Write addr_activity entries
            for addr in &addrs {
                let key = key_addr_activity(addr, ts, &block_id);
                self.db
                    .put_cf(&cf_aa, &key, b"")
                    .map_err(|e| MigError::Any(anyhow!(e)))?;
            }

            indexed += 1;

            if indexed > 0 && indexed.is_multiple_of(10_000) {
                tracing::info!(
                    "Migration 3→4: indexed {indexed} blocks so far (scanned {total})..."
                );
            }
        }

        tracing::info!(
            "Migration 3→4: completed — indexed {indexed} blocks with addresses out of {total} total"
        );
        Ok(())
    }

    // Migration 4 -> 5 :
    //
    // Backfill the `addr_type_activity` column family for all existing blocks.
    // Iterates `by_time` CF (which has all blocks), parses payload, extracts
    // (address, category) pairs and writes typed index entries.
    //
    // Idempotent: entries are keyed by (addr, cat, ts, block_id),
    // so re-writing is harmless.
    async fn mig_4_to_5(&self) -> std::result::Result<(), MigError> {
        let cf_time = self.cf("by_time");
        let cf_blocks = self.cf("blocks");
        let cf_i2t = self.cf("id2ts");
        let cf_ata = self.cf("addr_type_activity");

        let mut total = 0usize;
        let mut indexed = 0usize;

        tracing::info!("Migration 4→5: backfilling addr_type_activity index...");

        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::Start) {
            let (time_key, _) = kv.map_err(|e| MigError::Any(anyhow!(e)))?;
            total += 1;

            let Some((_ts, block_id)) = crate::helpers::parse_time_index_key(&time_key) else {
                continue;
            };

            let Some(block_bytes) = self
                .db
                .get_cf(&cf_blocks, block_id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
            else {
                continue;
            };

            let Ok(sb) = serde_json::from_slice::<StoredBlock>(&block_bytes) else {
                continue;
            };

            let Some(pjson) = &sb.payload_json else {
                continue;
            };
            let Ok(env) = serde_json::from_str::<PayloadEnvelope>(pjson) else {
                continue;
            };
            let PayloadEnvelope::Plain(ref plain) = env else {
                continue;
            };

            let typed = extract_involved_with_category(plain);
            if typed.is_empty() {
                continue;
            }

            // Get the block's timestamp from id2ts
            let ts = match self
                .db
                .get_cf(&cf_i2t, block_id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
            {
                Some(v) if v.len() == 8 => {
                    let mut be = [0u8; 8];
                    be.copy_from_slice(&v);
                    u64::from_be_bytes(be) as i64
                }
                _ => continue,
            };

            for (addr, cat) in &typed {
                let key = key_addr_type_activity(addr, cat.as_byte(), ts, &block_id);
                self.db
                    .put_cf(&cf_ata, &key, b"")
                    .map_err(|e| MigError::Any(anyhow!(e)))?;
            }

            indexed += 1;

            if indexed > 0 && indexed.is_multiple_of(10_000) {
                tracing::info!(
                    "Migration 4→5: indexed {indexed} blocks so far (scanned {total})..."
                );
            }
        }

        tracing::info!(
            "Migration 4→5: completed — indexed {indexed} blocks with typed categories out of {total} total"
        );
        Ok(())
    }

    // Migration 5 -> 6 :
    //
    // Re-index TxUtxo blocks whose fee output was previously indexed under
    // `ActivityCategory::Transfer` (byte 2) instead of `ActivityCategory::Fee` (byte 3).
    //
    // For each TxUtxo block, re-extract (address, category) pairs using the updated
    // `extract_involved_with_category` which now detects fee outputs (output.amount == tx.fee).
    // Also re-computes `activity_items` so pre-cached items reflect the corrected type.
    //
    // Idempotent: writing the same key with the same value is harmless.
    async fn mig_5_to_6(&self) -> std::result::Result<(), MigError> {
        let cf_time = self.cf("by_time");
        let cf_blocks = self.cf("blocks");
        let cf_i2t = self.cf("id2ts");
        let cf_ata = self.cf("addr_type_activity");
        let cf_items = self.cf("activity_items");

        let mut total = 0usize;
        let mut reindexed = 0usize;

        tracing::info!("Migration 5→6: reindexing TxUtxo fee outputs (Transfer→Fee)...");

        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::Start) {
            let (time_key, _) = kv.map_err(|e| MigError::Any(anyhow!(e)))?;
            total += 1;

            let Some((_ts, block_id)) = crate::helpers::parse_time_index_key(&time_key) else {
                continue;
            };

            let Some(block_bytes) = self
                .db
                .get_cf(&cf_blocks, block_id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
            else {
                continue;
            };

            let Ok(sb) = serde_json::from_slice::<StoredBlock>(&block_bytes) else {
                continue;
            };

            let Some(pjson) = &sb.payload_json else {
                continue;
            };
            let Ok(env) = serde_json::from_str::<PayloadEnvelope>(pjson) else {
                continue;
            };
            let PayloadEnvelope::Plain(ref plain) = env else {
                continue;
            };

            // Only process TxUtxo blocks with a positive fee
            let pms_types_payload::PlainPayload::TxUtxo(tx) = plain else {
                continue;
            };
            let fee = tx
                .fee
                .parse::<rust_decimal::Decimal>()
                .unwrap_or_default();
            if fee <= rust_decimal::Decimal::ZERO {
                continue;
            }

            // Check if any output is a fee output — skip block if not
            let has_fee_output = tx
                .outputs
                .iter()
                .any(|o| crate::helpers::is_fee_output_only(tx, &o.address));
            if !has_fee_output {
                continue;
            }

            // Get timestamp
            let ts = match self
                .db
                .get_cf(&cf_i2t, block_id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
            {
                Some(v) if v.len() == 8 => {
                    let mut be = [0u8; 8];
                    be.copy_from_slice(&v);
                    u64::from_be_bytes(be) as i64
                }
                _ => continue,
            };

            // Re-write typed index entries with corrected categories
            let typed = extract_involved_with_category(plain);
            for (addr, cat) in &typed {
                let key = key_addr_type_activity(addr, cat.as_byte(), ts, &block_id);
                self.db
                    .put_cf(&cf_ata, &key, b"")
                    .map_err(|e| MigError::Any(anyhow!(e)))?;
            }

            // Delete stale Transfer entries for fee-output addresses
            // (they now have Fee entries instead)
            for o in &tx.outputs {
                if crate::helpers::is_fee_output_only(tx, &o.address) {
                    let old_key = key_addr_type_activity(
                        &o.address,
                        crate::helpers::ActivityCategory::Transfer as u8,
                        ts,
                        &block_id,
                    );
                    let _ = self.db.delete_cf(&cf_ata, &old_key);
                }
            }

            // Re-compute activity_items for affected addresses
            let addrs = crate::helpers::extract_involved_addresses(plain);
            let items_map = crate::helpers::precompute_all_items(plain, &addrs, None);
            for (addr, items) in &items_map {
                if !items.is_empty() {
                    let key = crate::helpers::key_addr_activity(addr, ts, &block_id);
                    if let Ok(val) = serde_json::to_vec(items) {
                        let _ = self.db.put_cf(&cf_items, &key, &val);
                    }
                }
            }

            reindexed += 1;

            if reindexed > 0 && reindexed.is_multiple_of(10_000) {
                tracing::info!(
                    "Migration 5→6: reindexed {reindexed} TxUtxo blocks so far (scanned {total})..."
                );
            }
        }

        tracing::info!(
            "Migration 5→6: completed — reindexed {reindexed} TxUtxo blocks with fee outputs out of {total} total"
        );
        Ok(())
    }

    // Migration 6 -> 7 :
    // Ajout du Column Family "contracts" pour le système de smart contracts déclaratifs.
    // Le CF est créé automatiquement par `create_missing_column_families(true)` dans open_db.
    // Cette migration est un no-op — la CF existe déjà grâce à CF_NAMES.
    // On écrit une clé factice pour vérifier que le CF est accessible.
    async fn mig_6_to_7(&self) -> std::result::Result<(), MigError> {
        let cf_contracts = self.cf("contracts");

        // Vérification d'accès au CF : écriture + suppression d'une clé sentinelle
        self.db
            .put_cf(&cf_contracts, b"__init__", b"")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        self.db
            .delete_cf(&cf_contracts, b"__init__")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        tracing::info!("Migration 6→7: contracts CF verified — smart contract system ready");
        Ok(())
    }

    // Migration 7 -> 8 :
    // Ajout des Column Families "gas_pools" et "ledger_subscriptions"
    // pour le système économique (gas pool anti-spam + abonnements de ledgers).
    // Les CFs sont créés automatiquement par `create_missing_column_families(true)`.
    // Cette migration vérifie l'accessibilité des CFs avec une clé sentinelle.
    async fn mig_7_to_8(&self) -> std::result::Result<(), MigError> {
        // Verify gas_pools CF
        let cf_gas = self.cf("gas_pools");
        self.db
            .put_cf(&cf_gas, b"__init__", b"")
            .map_err(|e| MigError::Any(anyhow!(e)))?;
        self.db
            .delete_cf(&cf_gas, b"__init__")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        // Verify ledger_subscriptions CF
        let cf_subs = self.cf("ledger_subscriptions");
        self.db
            .put_cf(&cf_subs, b"__init__", b"")
            .map_err(|e| MigError::Any(anyhow!(e)))?;
        self.db
            .delete_cf(&cf_subs, b"__init__")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        tracing::info!("Migration 7→8: gas_pools + ledger_subscriptions CFs verified — economics system ready");
        Ok(())
    }

    /// Migration 8 → 9 : Column family `ledger_defs` pour la persistence des LedgerDef.
    ///
    /// Permet de sauvegarder les definitions de ledgers créés dynamiquement et
    /// de survivre aux redémarrages. Stocke aussi les changements d'ownership.
    async fn mig_8_to_9(&self) -> std::result::Result<(), MigError> {
        let cf = self.cf("ledger_defs");
        self.db
            .put_cf(&cf, b"__init__", b"")
            .map_err(|e| MigError::Any(anyhow!(e)))?;
        self.db
            .delete_cf(&cf, b"__init__")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        tracing::info!("Migration 8→9: ledger_defs CF verified — ledger persistence ready");
        Ok(())
    }

    /// Migration 9 → 10 (audit item 8, v0.7.4).
    /// Ensure the new `coordinator_key_history` CF is reachable and pinned
    /// for stores that already exist. The CF is created at open-time by
    /// the CF list reconciliation; this migration just touches it so we
    /// catch any creation issue here rather than later when a rotation
    /// block tries to land.
    async fn mig_9_to_10(&self) -> std::result::Result<(), MigError> {
        let cf = self.cf("coordinator_key_history");
        self.db
            .put_cf(&cf, b"__init__", b"")
            .map_err(|e| MigError::Any(anyhow!(e)))?;
        self.db
            .delete_cf(&cf, b"__init__")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        tracing::info!("Migration 9→10: coordinator_key_history CF ready");
        Ok(())
    }
}

#[cfg(test)]
mod data_preservation_tests {
    //! Audit gap E1 (v0.9.3) : avant, aucun test n'écrivait des données à une
    //! version N, ne lançait la migration N→CURRENT_VER, et ne vérifiait que les
    //! données survivent ET que les index reconstruits sont repeuplés. Les tests
    //! existants ne vérifiaient que le NUMÉRO de version. Pour un moteur bancaire,
    //! une migration qui corromprait silencieusement les balances était un trou.
    //!
    //! Ce test simule une vieille DB pré-index (CFs by_time/id2ts/addr_activity/
    //! addr_type_activity vidés), redescend la version, relance `ensure_schema`,
    //! et prouve :
    //!   1. le CF `blocks` (source de vérité) est INTACT — aucun bloc perdu ;
    //!   2. la version finale == CURRENT_VER ;
    //!   3. mig_2→3 reconstruit by_time/id2ts ;
    //!   4. mig_3→4 reconstruit addr_activity à partir des payloads des blocs.

    use super::*;
    use crate::rocks_store::store::{RocksMemoryConfig, RocksStore};
    use pms_types::TxOutput;
    use pms_types_payload::{PayloadEnvelope, PlainPayload};

    /// Compte les entrées d'un CF (hors clé sentinelle `__init__`).
    fn cf_entry_count(store: &RocksStore, cf_name: &str) -> usize {
        let cf = store.cf(cf_name);
        store
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start)
            .filter_map(|kv| kv.ok())
            .filter(|(k, _)| k.as_ref() != b"__init__")
            .count()
    }

    /// Vide entièrement un CF (simule une DB antérieure aux migrations d'index).
    fn wipe_cf(store: &RocksStore, cf_name: &str) {
        let cf = store.cf(cf_name);
        let keys: Vec<Vec<u8>> = store
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start)
            .filter_map(|kv| kv.ok().map(|(k, _)| k.to_vec()))
            .collect();
        for k in keys {
            store.db.delete_cf(&cf, &k).unwrap();
        }
    }

    async fn write_mint(store: &RocksStore, id: &str, parent: &str, addr: &str, amount: &str) {
        let payload = PlainPayload::Mint {
            outputs: vec![TxOutput::new(addr.to_string(), amount.to_string(), None)],
        };
        let sb = StoredBlock {
            id: id.to_string(),
            parents: vec![parent.to_string()],
            payload_json: Some(
                serde_json::to_string(&PayloadEnvelope::Plain(payload)).unwrap(),
            ),
            nonce: 0,
            network_id: "pms:test".to_string(),
            protocol_version: 1,
            signer_pk_hex: String::new(),
            signature_hex: String::new(),
            metadata: None,
        };
        store.append_block_atomic(&sb).await.unwrap();
    }

    #[tokio::test]
    async fn migration_preserves_blocks_and_rebuilds_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RocksStore::new(
            tmp.path().to_str().unwrap(),
            256,
            "pms:test",
            None,
            &RocksMemoryConfig::default(),
        )
        .await
        .unwrap();
        store.ensure_schema().await.unwrap();
        assert_eq!(store.get_version().await.unwrap(), CURRENT_VER);

        // Écrit 5 blocs Mint vers des adresses distinctes (peuple blocks + index).
        for i in 0..5 {
            let parent = if i == 0 {
                "genesis".to_string()
            } else {
                format!("M{}", i - 1)
            };
            write_mint(
                &store,
                &format!("M{i}"),
                &parent,
                &format!("8eaddr{i}"),
                "100.0",
            )
            .await;
        }

        let ids_before = store.all_block_ids().await.unwrap();
        let by_time_before = cf_entry_count(&store, "by_time");
        let addr_before = cf_entry_count(&store, "addr_activity");
        println!(
            "before migration: blocks={}, by_time={}, addr_activity={}",
            ids_before.len(),
            by_time_before,
            addr_before
        );
        assert!(ids_before.len() >= 5, "5 mints must be stored");
        assert!(by_time_before >= 5, "write path must index by_time");
        assert!(addr_before >= 5, "write path must index addr_activity");

        // SIMULE une vieille DB pré-migration : on vide les CF d'index reconstructibles.
        for cf in ["by_time", "id2ts", "addr_activity", "addr_type_activity"] {
            wipe_cf(&store, cf);
        }
        assert_eq!(cf_entry_count(&store, "by_time"), 0, "by_time wiped");
        assert_eq!(cf_entry_count(&store, "addr_activity"), 0, "addr_activity wiped");

        // Redescend la version → ensure_schema rejoue 1→CURRENT_VER.
        store.set_version(1).await.unwrap();
        assert_eq!(store.get_version().await.unwrap(), 1);

        // LANCE LES MIGRATIONS.
        store.ensure_schema().await.unwrap();

        // 1) Aucun bloc perdu (le CF `blocks` n'est jamais touché par une migration).
        let ids_after = store.all_block_ids().await.unwrap();
        println!("after migration: blocks={}", ids_after.len());
        assert_eq!(
            ids_after.len(),
            ids_before.len(),
            "migration must not lose any block"
        );
        for id in &ids_before {
            let b = store.get_block(id).await.unwrap();
            assert!(b.is_some(), "block {id} must survive migration");
            // payload intact (toujours un Mint plain déserialisable)
            let env: PayloadEnvelope =
                serde_json::from_str(b.unwrap().payload_json.as_deref().unwrap()).unwrap();
            assert!(matches!(env, PayloadEnvelope::Plain(PlainPayload::Mint { .. })));
        }

        // 2) Version remontée à CURRENT_VER.
        assert_eq!(store.get_version().await.unwrap(), CURRENT_VER);

        // 3) mig_2→3 a reconstruit by_time/id2ts depuis idx_blocks.
        let by_time_after = cf_entry_count(&store, "by_time");
        let id2ts_after = cf_entry_count(&store, "id2ts");
        println!("after migration: by_time={by_time_after}, id2ts={id2ts_after}");
        assert!(
            by_time_after >= 5,
            "mig_2→3 must rebuild by_time (got {by_time_after})"
        );
        assert!(id2ts_after >= 5, "mig_2→3 must rebuild id2ts");

        // 4) mig_3→4 a reconstruit addr_activity depuis les payloads des blocs.
        let addr_after = cf_entry_count(&store, "addr_activity");
        println!("after migration: addr_activity={addr_after}");
        assert!(
            addr_after >= 5,
            "mig_3→4 must rebuild addr_activity from block payloads (got {addr_after})"
        );
    }
}
