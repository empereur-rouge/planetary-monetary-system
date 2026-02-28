use crate::helpers::{extract_involved_addresses, key_addr_activity, key_time_index, ts_to_be};
use crate::rocks_store::store::RocksStore;
use crate::{CURRENT_VER, DagStorage, MigError, StoredBlock};
use anyhow::anyhow;
use pms_types_payload::PayloadEnvelope;
use std::sync::Arc;
use rocksdb::BoundColumnFamily;

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
                tracing::info!("Migration 1→2: processed {}/{} blocks ({:.1}%)", i, total, (i as f64 / total as f64) * 100.0);
            }
            let maybe_b = self.get_block(&id).await.map_err(MigError::Any)?;
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

            let id = String::from_utf8(k.to_vec())
                .map_err(|e| MigError::Any(anyhow!(e)))?;

            // Skip sentinel key from migration 0→1
            if id == "__init__" {
                continue;
            }

            // Check if id2ts already has this block
            if self.db.get_cf(&cf_i2t, id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))?
                .is_some()
            {
                continue;
            }

            // Write synthetic timestamp
            let time_key = key_time_index(synthetic_ts, &id);
            self.db.put_cf(&cf_time, &time_key, b"")
                .map_err(|e| MigError::Any(anyhow!(e)))?;
            self.db.put_cf(&cf_i2t, id.as_bytes(), ts_to_be(synthetic_ts))
                .map_err(|e| MigError::Any(anyhow!(e)))?;

            synthetic_ts += 1;
            missing += 1;

            if missing > 0 && missing % 10_000 == 0 {
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
            let Some(block_bytes) = self.db.get_cf(&cf_blocks, block_id.as_bytes())
                .map_err(|e| MigError::Any(anyhow!(e)))? else {
                continue;
            };

            let Ok(sb) = serde_json::from_slice::<StoredBlock>(&block_bytes) else {
                continue;
            };

            // Parse payload
            let Some(pjson) = &sb.payload_json else { continue };
            let Ok(env) = serde_json::from_str::<PayloadEnvelope>(pjson) else { continue };
            let PayloadEnvelope::Plain(ref plain) = env else { continue };

            let addrs = extract_involved_addresses(plain);
            if addrs.is_empty() {
                continue;
            }

            // Get the block's timestamp from id2ts
            let ts = match self.db.get_cf(&cf_i2t, block_id.as_bytes())
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
                self.db.put_cf(&cf_aa, &key, b"")
                    .map_err(|e| MigError::Any(anyhow!(e)))?;
            }

            indexed += 1;

            if indexed > 0 && indexed % 10_000 == 0 {
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
}
