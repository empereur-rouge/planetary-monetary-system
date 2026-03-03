// src/atomic.rs

use crate::StoredBlock;
use crate::activity_item::StoredActivityItem;
use crate::helpers::{key_time_index, le_to_u64, now_ms_i64, ts_to_be, u64_to_le};
use crate::rocks_store::store::RocksStore;
use anyhow::Result;
use std::collections::HashMap;

use pms_types::Block;
use pms_wire::WireMeta;
use rocksdb::WriteBatch;

impl RocksStore {
    pub async fn append_block_atomic(&self, b: &StoredBlock) -> Result<bool> {
        use rocksdb::WriteBatch;

        // 0. existe déjà ?
        let cf_blocks = self.cf("blocks");
        if self.db.get_cf(&cf_blocks, b.id.as_bytes())?.is_some() {
            return Ok(false);
        }
        // timestamp pour index_by_time & tip
        let now_ts = now_ms_i64();
        let time_key = key_time_index(now_ts, &b.id);

        let cf_idx = self.cf("idx_blocks");
        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");
        let cf_tips = self.cf("tips");
        let cf_count = self.cf("children_count");
        let cf_childset = self.cf("children_set");

        // On construit le batch
        let mut batch = WriteBatch::default();

        // blocks[id] = StoredBlock JSON
        let json = serde_json::to_vec(b)?;
        batch.put_cf(&cf_blocks, b.id.as_bytes(), &json);

        // idx_blocks[id] = ""
        batch.put_cf(&cf_idx, b.id.as_bytes(), b"");

        // by_time[ time_key ] = ""
        batch.put_cf(&cf_time, &time_key, b"");

        // id2ts[id] = ts_be(8)
        batch.put_cf(&cf_i2t, b.id.as_bytes(), ts_to_be(now_ts));

        // tips: new block becomes tip
        batch.put_cf(&cf_tips, b.id.as_bytes(), ts_to_be(now_ts));

        // tips: remove its parents from tips (they're no longer tips)
        for p in &b.parents {
            batch.delete_cf(&cf_tips, p.as_bytes());
        }

        // children_count[parent]++ ; children_set[parent|0x00|child] = ""
        for p in &b.parents {
            // read old count
            let old = self.db.get_cf(&cf_count, p.as_bytes())?;
            let newcount = match old {
                Some(v) if v.len() == 8 => le_to_u64(&v) + 1,
                _ => 1u64,
            };
            batch.put_cf(&cf_count, p.as_bytes(), u64_to_le(newcount));

            let mut edge_key = Vec::with_capacity(p.len() + 1 + b.id.len());
            edge_key.extend_from_slice(p.as_bytes());
            edge_key.push(0);
            edge_key.extend_from_slice(b.id.as_bytes());
            batch.put_cf(&cf_childset, edge_key, b"");
        }

        // Per-address activity indexes (addr_activity + addr_type_activity + activity_items)
        self.apply_addr_activity_indices(&mut batch, b, now_ts, None)?;

        // write atomiquement
        self.db.write(batch)?;

        // trim_tips: keep bounded tips for DAG parent selection (consensus-critical).
        // NOTE: by_time/id2ts are NOT trimmed — they must grow unbounded
        // for the activity/history API to scan full DAG history.
        self.trim_tips()?;

        Ok(true)
    }

    pub async fn persist_genesis(&self, g: &Block, meta: &WireMeta) -> Result<()> {
        // Construit un StoredBlock équivalent
        let sb = StoredBlock {
            id: g.id.clone(),
            parents: vec![],
            payload_json: serde_json::to_string(&g.payload).ok(),
            nonce: g.nonce,

            // 🔥 nouveau : champs "réseau + signature"
            network_id: meta.network_id.clone(),
            protocol_version: meta.protocol_version as u16,
            signer_pk_hex: "GENESIS".to_string(), // marqueur spécial
            signature_hex: String::new(),         // pas de signature pour l'instant
            metadata: g.metadata.clone(),
        };

        // Passe par l'append atomique (Rocks) pour rester cohérent
        self.append_block_atomic(&sb)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    pub(crate) fn apply_dag_indices(&self, batch: &mut WriteBatch, b: &StoredBlock) -> Result<()> {
        let now_ts = now_ms_i64();
        let time_key = key_time_index(now_ts, &b.id);

        let cf_blocks = self.cf("blocks");
        let cf_idx = self.cf("idx_blocks");
        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");
        let cf_tips = self.cf("tips");
        let cf_count = self.cf("children_count");
        let cf_childset = self.cf("children_set");

        let json = serde_json::to_vec(b)?;
        batch.put_cf(&cf_blocks, b.id.as_bytes(), &json);
        batch.put_cf(&cf_idx, b.id.as_bytes(), b"");
        batch.put_cf(&cf_time, &time_key, b"");
        batch.put_cf(&cf_i2t, b.id.as_bytes(), ts_to_be(now_ts));
        batch.put_cf(&cf_tips, b.id.as_bytes(), ts_to_be(now_ts));

        for p in &b.parents {
            batch.delete_cf(&cf_tips, p.as_bytes());
        }

        for p in &b.parents {
            let old = self.db.get_cf(&cf_count, p.as_bytes())?;
            let newcount = match old {
                Some(v) if v.len() == 8 => le_to_u64(&v) + 1,
                _ => 1u64,
            };
            batch.put_cf(&cf_count, p.as_bytes(), u64_to_le(newcount));

            let mut edge_key = Vec::with_capacity(p.len() + 1 + b.id.len());
            edge_key.extend_from_slice(p.as_bytes());
            edge_key.push(0);
            edge_key.extend_from_slice(b.id.as_bytes());
            batch.put_cf(&cf_childset, edge_key, b"");
        }

        Ok(())
    }

    /// Write per-address activity indexes (addr_activity + addr_type_activity + activity_items)
    /// into the provided WriteBatch.
    ///
    /// If `precomputed` is `Some`, those pre-classified items are written to the
    /// `activity_items` CF. Otherwise, items are auto-computed from the plain payload
    /// (with `sender_addr = None` for TxUtxo).
    pub(crate) fn apply_addr_activity_indices(
        &self,
        batch: &mut WriteBatch,
        b: &StoredBlock,
        ts: i64,
        precomputed: Option<&HashMap<String, Vec<StoredActivityItem>>>,
    ) -> Result<()> {
        let Some(pjson) = &b.payload_json else {
            return Ok(());
        };
        let Ok(pms_types_payload::PayloadEnvelope::Plain(ref plain)) =
            serde_json::from_str::<pms_types_payload::PayloadEnvelope>(pjson)
        else {
            return Ok(());
        };

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
                let key = crate::helpers::key_addr_type_activity(addr, cat.as_byte(), ts, &b.id);
                batch.put_cf(&cf_ata, &key, b"");
            }
        }

        // Pre-computed activity items (activity_items CF)
        let cf_items = self.cf("activity_items");
        if let Some(items_map) = precomputed {
            for (addr, items) in items_map {
                if !items.is_empty() {
                    let key = crate::helpers::key_addr_activity(addr, ts, &b.id);
                    let val = serde_json::to_vec(items)?;
                    batch.put_cf(&cf_items, &key, &val);
                }
            }
        } else {
            // Auto-compute from payload (sender=None for TxUtxo — best-effort)
            let items_map =
                crate::helpers::precompute_all_items(plain, &addrs, None);
            for (addr, items) in &items_map {
                if !items.is_empty() {
                    let key = crate::helpers::key_addr_activity(addr, ts, &b.id);
                    let val = serde_json::to_vec(items)?;
                    batch.put_cf(&cf_items, &key, &val);
                }
            }
        }

        Ok(())
    }
}
