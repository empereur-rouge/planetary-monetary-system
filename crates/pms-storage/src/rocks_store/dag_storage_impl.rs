//! Implementation of the `DagStorage` trait for `RocksStore`.
//!
//! This module contains the complete `DagStorage` trait implementation,
//! covering block persistence, tip management, child edges, finality,
//! import/export, and batch operations.

use crate::helpers::{be_to_ts, key_time_index, le_to_u64, now_ms_i64, parse_time_index_key, ts_to_be, u64_to_le};
use crate::rocks_store::activity_index::make_utxo_key;
use crate::rocks_store::store::RocksStore;
use crate::{DagStorage, PutResult, StoredBlock, UtxoDelta};
use anyhow::Result;
use pms_wire::WireBlock;
use rocksdb::{Direction, IteratorMode};

use super::activity_index::iter_cf_all;

/// Extract the `lock_block_id` claimed by a `BridgeMint` block, if any.
///
/// Used to record the durable bridge-mint anti-replay marker (`bridge_consumed`
/// CF) in the SAME atomic `WriteBatch` as the block itself, so the consumed-lock
/// record can never diverge from the mint it backs (audit rang 3, B3). Returns
/// `None` for any non-`BridgeMint` block (including encrypted payloads, which a
/// bridge mint never is — it is always a Plain coordinator-signed block).
fn bridge_mint_lock_id(b: &StoredBlock) -> Option<String> {
    let pjson = b.payload_json.as_ref()?;
    match serde_json::from_str::<pms_types_payload::PayloadEnvelope>(pjson) {
        Ok(pms_types_payload::PayloadEnvelope::Plain(
            pms_types_payload::PlainPayload::BridgeMint { lock_block_id, .. },
        )) => Some(lock_block_id),
        _ => None,
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
        self.tip_count_estimate
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.trim_tips()?;
        Ok(())
    }

    async fn remove_tip(&self, id: &str) -> Result<()> {
        let cf_tips = self.cf("tips");

        // SAFETY: Never remove the last tip. An empty tips CF causes
        // top_tips() to return empty → fee distribution silently blocked.
        // Count current tips; if this is the only one, keep it.
        let tip_count = self
            .db
            .iterator_cf(&cf_tips, rocksdb::IteratorMode::Start)
            .take(2) // only need to know if count <= 1
            .count();
        if tip_count <= 1 {
            // Check if the only tip IS the one we want to remove
            if let Some(raw) = self.db.get_cf(&cf_tips, id.as_bytes())? {
                if !raw.is_empty() {
                    tracing::warn!(
                        tip_id = &id[..16.min(id.len())],
                        "remove_tip: BLOCKED — refusing to remove last remaining tip"
                    );
                    return Ok(());
                }
            }
        }

        self.db.delete_cf(&cf_tips, id.as_bytes())?;
        self.tip_count_estimate
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    async fn top_tips(&self, limit: usize) -> Result<Vec<String>> {
        // Stale-while-revalidate cache:
        // - < 500ms  → fresh, serve directly
        // - 500ms-5s → stale but usable, serve immediately (eliminates stampede)
        // - > 5s     → force refresh
        if let Some((ts, ref tips)) = *self.top_tips_cache.lock() {
            if tips.len() >= limit {
                let age = ts.elapsed();
                if age < std::time::Duration::from_secs(5) {
                    return Ok(tips[..limit].to_vec());
                }
            }
        }

        // Refresh: scan tips CF (only one caller gets here at a time in practice
        // because the stale window serves concurrent callers immediately)
        let cf_tips = self.cf("tips");
        let mut v: Vec<(String, i64)> = Vec::new();
        for kv in self.db.iterator_cf(&cf_tips, rocksdb::IteratorMode::Start) {
            let (k, val) = kv?;
            let id = String::from_utf8(k.to_vec())?;
            let ts = if val.len() == 8 { be_to_ts(&val) } else { 0 };
            v.push((id, ts));
        }
        // sort desc by ts
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
        // Cache the full sorted result (up to 64 entries for reuse at different limits)
        let all: Vec<String> = v
            .into_iter()
            .take(limit.max(64))
            .map(|(id, _)| id)
            .collect();

        // Update cache
        *self.top_tips_cache.lock() = Some((std::time::Instant::now(), all.clone()));

        Ok(all.into_iter().take(limit).collect())
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

    /// Efficiently returns only the `n` lexicographically largest block IDs
    /// using a **reverse iterator**. This avoids loading all block IDs into
    /// memory — critical for ledgers with millions of blocks (e.g. 7.8M blocks
    /// in Eden would consume ~500 MB if loaded via `all_block_ids()`).
    ///
    /// When `n == 0`, falls back to `all_block_ids()` (unlimited).
    async fn newest_block_ids(&self, n: usize) -> Result<Vec<String>> {
        if n == 0 {
            return self.all_block_ids().await;
        }
        let cf_idx = self.cf("idx_blocks");
        let mut ids = Vec::with_capacity(n);
        for kv in self.db.iterator_cf(&cf_idx, IteratorMode::End) {
            let (k, _v) = kv?;
            ids.push(String::from_utf8(k.to_vec())?);
            if ids.len() >= n {
                break;
            }
        }
        ids.reverse(); // Restore ascending lexicographic order
        Ok(ids)
    }

    /// O(1) emptiness check — reads a single key from `idx_blocks` CF.
    async fn is_empty(&self) -> Result<bool> {
        let cf_idx = self.cf("idx_blocks");
        let mut iter = self.db.iterator_cf(&cf_idx, IteratorMode::Start);
        Ok(iter.next().is_none())
    }

    /// Returns up to `n` block IDs in chronological order (oldest-first),
    /// using the `by_time` CF which is keyed by `[ts_BE:8][0x00][block_id]`.
    ///
    /// A reverse iterator reads the N most-recent entries, then the result
    /// is reversed so callers get oldest-first ordering — matching the
    /// expectation of `bootstrap_from_store_with_capacity` where blocks
    /// should be inserted parents-before-children.
    ///
    /// Falls back to `newest_block_ids(n)` (lexicographic) if the `by_time`
    /// CF is empty (e.g. after an `import_json` that skips time indices).
    async fn newest_block_ids_by_time(&self, n: usize) -> Result<Vec<String>> {
        if n == 0 {
            return self.all_block_ids().await;
        }

        let cf_time = self.cf("by_time");
        let mut ids = Vec::with_capacity(n);

        for kv in self.db.iterator_cf(&cf_time, IteratorMode::End) {
            let (k, _v) = kv?;
            if let Some((_ts, id)) = parse_time_index_key(&k) {
                ids.push(id);
                if ids.len() >= n {
                    break;
                }
            }
        }

        // Reverse: iterator was newest-first, we want oldest-first
        // so bootstrap_insert processes parents before children.
        ids.reverse();
        Ok(ids)
    }

    async fn block_count(&self) -> Result<u64> {
        let cf_idx = self.cf("idx_blocks");
        let count = self
            .db
            .iterator_cf(&cf_idx, rocksdb::IteratorMode::Start)
            .count();
        Ok(count as u64)
    }

    /// O(1) tip count from the inline atomic counter — maintained by
    /// `add_tip` / `remove_tip` / batch atomic-append. Used by SaaS
    /// watchers polling `GET /v1/dag/status`.
    async fn tip_count_estimate(&self) -> usize {
        Self::tip_count_estimate(self)
    }

    /// O(1) approximate count via RocksDB metadata property.
    /// Falls back to full scan on failure.
    async fn block_count_estimate(&self) -> Result<u64> {
        let cf_idx = self.cf("idx_blocks");
        // rocksdb::properties::ESTIMATE_NUM_KEYS = "rocksdb.estimate-num-keys"
        match self
            .db
            .property_int_value_cf(&cf_idx, "rocksdb.estimate-num-keys")
        {
            Ok(Some(n)) => Ok(n),
            Ok(None) => self.block_count().await,
            Err(_) => self.block_count().await,
        }
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
    ///   - `add_tip` (pour qu'il soit éligible à la sélection de parents)
    ///   - pour chaque parent :
    ///       - `add_child_edge(parent, id)` (reconstruit les compteurs)
    ///       - `remove_tip(parent)` (un parent référencé n'est plus un tip)
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
        let _ = (
            &cf_blocks,
            &cf_idx,
            &cf_tips,
            &cf_children_cnt,
            &cf_children_set,
        );

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

        // Point de départ pour l'itérateur (Reverse).
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
                        // sinon on l'a skip en consommant déjà l'item égal
                        ids.push(parse_time_index_key(&k).unwrap().1);
                    }
                }
            }
        }

        // Poursuivre jusqu'à limit+1 (pour savoir s'il y a une page suivante)
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
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let cf_blocks = self.cf("blocks");
        let keys: Vec<_> = ids.iter().map(|id| (&cf_blocks, id.as_bytes())).collect();
        let results = self.db.multi_get_cf(keys);

        let mut out = Vec::with_capacity(ids.len());
        for result in results {
            match result {
                Ok(Some(v)) => {
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
                Ok(None) => {} // block not found, skip
                Err(e) => {
                    tracing::warn!("multi_get_cf error reading block: {e}");
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
        let mut batch = rocksdb::WriteBatch::default();
        for id in ids {
            batch.put_cf(&cf_final, id.as_bytes(), b"");
        }
        self.db.write(batch)?;
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

            // CREATES — format d'écriture unique : UtxoValue::encode_output
            for (txid, idx, out) in &d.create {
                let key = make_utxo_key(txid, *idx);
                let json = crate::rocks_store::utxo::UtxoValue::encode_output(out)?;
                batch.put_cf(&cf_utxo, &key, &json);
            }
        }

        // 1.a-bis) Bridge-mint anti-replay (audit rang 3, B3) — record the
        // source lock consumed by this BridgeMint in the SAME atomic batch, so
        // the durable `bridge_consumed` marker can never diverge from the mint.
        // A replayed BridgeMint reusing this `lock_block_id` is rejected at
        // validation (`DagStorage::is_bridge_lock_consumed`) before re-minting.
        if let Some(lock_block_id) = bridge_mint_lock_id(b) {
            let cf_bridge_consumed = self.cf("bridge_consumed");
            batch.put_cf(&cf_bridge_consumed, lock_block_id.as_bytes(), b.id.as_bytes());
        }

        // 1.b) Indices DAG
        self.apply_dag_indices(&mut batch, b)?;

        // 1.c) Per-address activity indexes (addr_activity + addr_type_activity + activity_items)
        let ts = crate::helpers::now_ms_i64();
        self.apply_addr_activity_indices(&mut batch, b, ts, None)?;

        // 2) write atomique
        self.db.write(batch)?;

        // 3) trim tips (amortized every 64 blocks; by_time/id2ts grow unbounded for activity API)
        self.maybe_trim_tips()?;

        // 4) record in recent-blocks Bloom — keeps the dedup
        // fast-path consistent for blocks that take this single-block
        // route (e.g. low-RPS test paths).
        self.recent_blocks_bloom.write().insert(b.id.as_bytes());

        Ok(true)
    }

    /// Persist multiple blocks in a single RocksDB WriteBatch.
    ///
    /// This is the critical optimization for sustained high-TPS: instead of N
    /// individual `db.write(batch)` calls (each acquiring the DB write mutex and
    /// appending to WAL), a single mega-batch reduces overhead by up to 64×.
    ///
    /// Duplicate blocks are detected via `multi_get_cf` (one batch read instead
    /// of N individual reads) and skipped. `maybe_trim_tips()` is called once
    /// at the end instead of per-block.
    async fn append_blocks_batch(
        &self,
        blocks: &[(&StoredBlock, Option<&UtxoDelta>, &[(String, u64)])],
    ) -> Result<usize> {
        use crate::helpers::{key_time_index, ts_to_be, u64_to_le};
        use rocksdb::WriteBatch;
        use std::collections::HashMap;

        if blocks.is_empty() {
            return Ok(0);
        }

        // Sub-stage timings for the consumer side, exported as
        // `pms_persist_consumer_substage_us_total{stage="dedup|build|write|trim"}`.
        // The profile test reads the deltas to identify which sub-stage
        // owns the per-block cost growth — the headline `c_us/blk` only
        // tells us the consumer is slow, not why.
        let t_total_start = std::time::Instant::now();
        let t_dedup_start = std::time::Instant::now();

        // ── Pre-resolve all CF handles once for the whole batch.
        //
        // `RocksStore::cf()` does a HashMap lookup for the prefixed name
        // followed by `db.cf_handle(full)`, which under the hood acquires
        // a per-DB mutex. At the previous per-call rate (~10 lookups per
        // block × 64 blocks per batch = ~640 cf_handle() calls per batch)
        // this added measurable overhead to the persist consumer hot
        // path. Resolving once amortizes the cost over the whole batch.
        let cf_blocks = self.cf("blocks");
        let cf_idx = self.cf("idx_blocks");
        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");
        let cf_tips = self.cf("tips");
        let cf_count = self.cf("children_count");
        let cf_childset = self.cf("children_set");
        let cf_utxo = self.cf("utxo");
        let cf_utxo_spent = self.cf("utxo_spent");

        // ── Batch dedup check, fronted by an in-RAM Bloom filter.
        //
        // Goal: skip the `multi_get_cf` LSM read for the 99%+ of
        // blocks that are genuinely new. The filter is authoritative
        // for negatives once warmed (every persisted block_id is
        // inserted, so a "definitely not in filter" answer means
        // "definitely not in DB").
        //
        // We still need to confirm any positive — Bloom has a small
        // false-positive rate (~0.8% by design) — so we collect the
        // positive subset and `multi_get_cf` only those keys. Genuine
        // duplicates always produce a positive (no false negatives by
        // construction), so dedup correctness is preserved.
        //
        // Until the filter has been warmed from the DB at startup,
        // every block is treated as "maybe in DB" and we fall back to
        // the legacy whole-batch lookup. This avoids a correctness
        // window after restart where a block actually present in
        // RocksDB would be misclassified as new.
        let mut existing = vec![false; blocks.len()];
        let mut maybe_existing_idx: Vec<usize> = Vec::new();
        let mut bloom_skips = 0u64;
        let mut bloom_hits = 0u64;
        {
            let bloom = self.recent_blocks_bloom.read();
            if bloom.is_warmed() {
                for (i, (b, _, _)) in blocks.iter().enumerate() {
                    if bloom.maybe_contains(b.id.as_bytes()) {
                        maybe_existing_idx.push(i);
                        bloom_hits += 1;
                    } else {
                        bloom_skips += 1;
                    }
                }
            } else {
                maybe_existing_idx.extend(0..blocks.len());
            }
        }
        if !maybe_existing_idx.is_empty() {
            let lookup_keys: Vec<_> = maybe_existing_idx
                .iter()
                .map(|&i| (cf_blocks.clone(), blocks[i].0.id.as_bytes().to_vec()))
                .collect();
            let results = self
                .db
                .multi_get_cf(lookup_keys.iter().map(|(cf, k)| (cf, k.as_slice())));
            for (slot, r) in maybe_existing_idx.iter().zip(results.into_iter()) {
                if matches!(r, Ok(Some(_))) {
                    existing[*slot] = true;
                }
            }
        }
        if bloom_skips > 0 {
            self.bloom_skips
                .fetch_add(bloom_skips, std::sync::atomic::Ordering::Relaxed);
        }
        if bloom_hits > 0 {
            self.bloom_hits
                .fetch_add(bloom_hits, std::sync::atomic::Ordering::Relaxed);
        }
        let t_dedup = t_dedup_start.elapsed();
        let t_build_start = std::time::Instant::now();

        // ── parent_counts: post-insert `children_count` values
        // snapshotted from the in-memory DAG by the producer.
        //
        // This replaces the previous per-batch `multi_get_cf` walk that
        // was the dominant TPS-degradation cost: as the DAG grew,
        // parent blocks aged out of the memtable and the LSM-tree
        // lookup walked into L0 SSTs. The producer's
        // `ConcurrentDag::get_children_count` is a lock-free atomic
        // load that scales as O(1) regardless of DAG size, and FIFO
        // channel ordering guarantees the consumer's `put_cf` writes
        // see the same values RocksDB would have read.
        //
        // We deduplicate across the batch and keep the maximum count
        // seen for each parent — when two blocks in the same batch
        // share a parent the producer captured two different counts,
        // and the WriteBatch only retains the last `put_cf`, so the
        // running max preserves correctness.
        let mut parent_counts: HashMap<&[u8], u64> = HashMap::new();
        for (i, (_, _, pc)) in blocks.iter().enumerate() {
            if existing[i] {
                continue;
            }
            for (parent_id, count) in pc.iter() {
                let key = parent_id.as_bytes();
                let entry = parent_counts.entry(key).or_insert(0);
                if *count > *entry {
                    *entry = *count;
                }
            }
        }

        // ── Build a single mega WriteBatch for all new blocks.
        let mut batch = WriteBatch::default();
        let mut count = 0usize;
        let now_ts = crate::helpers::now_ms_i64();

        for (i, (b, delta, _pc)) in blocks.iter().enumerate() {
            if existing[i] {
                continue; // block already persisted
            }

            // UTXO Delta — handles cleared at the top of this fn.
            if let Some(d) = delta {
                for (txid, idx) in &d.spend {
                    let key = make_utxo_key(txid, *idx);
                    batch.delete_cf(&cf_utxo, &key);
                    batch.put_cf(&cf_utxo_spent, &key, b.id.as_bytes());
                }

                for (txid, idx, out) in &d.create {
                    let key = make_utxo_key(txid, *idx);
                    let json = crate::rocks_store::utxo::UtxoValue::encode_output(out)?;
                    batch.put_cf(&cf_utxo, &key, &json);
                }
            }

            // Bridge-mint anti-replay (audit rang 3, B3) — same atomic batch as
            // the block, mirrors the single-block path. Records the source lock
            // this BridgeMint consumes so a replay reusing it is rejected. The CF
            // handle is resolved lazily (bridge mints are rare) so non-bridge
            // batches never touch it.
            if let Some(lock_block_id) = bridge_mint_lock_id(b) {
                let cf_bridge_consumed = self.cf("bridge_consumed");
                batch.put_cf(&cf_bridge_consumed, lock_block_id.as_bytes(), b.id.as_bytes());
            }

            // ── DAG indices (inlined from apply_dag_indices, using
            // the pre-resolved CF handles + producer-supplied counts).
            let time_key = key_time_index(now_ts, &b.id);
            let json = serde_json::to_vec(b)?;
            batch.put_cf(&cf_blocks, b.id.as_bytes(), &json);
            batch.put_cf(&cf_idx, b.id.as_bytes(), b"");
            batch.put_cf(&cf_time, &time_key, b"");
            batch.put_cf(&cf_i2t, b.id.as_bytes(), ts_to_be(now_ts));
            batch.put_cf(&cf_tips, b.id.as_bytes(), ts_to_be(now_ts));
            self.tip_count_estimate
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for p in &b.parents {
                batch.delete_cf(&cf_tips, p.as_bytes());
                self.tip_count_estimate
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                // Use the producer-supplied count if available; otherwise
                // fall back to "1" (the legacy default for new parents).
                let p_bytes: &[u8] = p.as_bytes();
                if let Some(new_count) = parent_counts.get(p_bytes) {
                    batch.put_cf(&cf_count, p_bytes, u64_to_le(*new_count));
                }

                let mut edge_key =
                    Vec::with_capacity(p.len() + 1 + b.id.len());
                edge_key.extend_from_slice(p_bytes);
                edge_key.push(0);
                edge_key.extend_from_slice(b.id.as_bytes());
                batch.put_cf(&cf_childset, edge_key, b"");
            }

            // Activity indices are deferred to a background task — they
            // feed the dashboard's history API, which has no consensus
            // role and tolerates a few seconds of lag. Removing them
            // from the hot WriteBatch shrinks `db.write()` time and frees
            // the persist consumer to drain the channel faster.
            //
            // The async writer hooks in via
            // `pms_core::background_activity::ActivityJob`; the producer
            // side is `do_persist_block_internal` (alongside
            // `persist_tx.try_send`). If activity ever falls catastrophically
            // behind we lose only the dashboard view of those blocks —
            // balances, UTXOs and consensus stay correct.

            count += 1;
        }

        let t_build = t_build_start.elapsed();
        let t_write_start = std::time::Instant::now();

        if count > 0 {
            // Single atomic write for all blocks
            self.db.write(batch)?;
        }
        let t_write = t_write_start.elapsed();
        let t_trim_start = std::time::Instant::now();

        if count > 0 {
            // Trim tips once for the whole batch
            self.maybe_trim_tips()?;

            // Mark every block we just persisted in the recent-blocks
            // Bloom filter so subsequent batches can skip the LSM
            // dedup read. Acquired under write-lock briefly; the
            // hot read-path uses `read()` and never blocks.
            let mut bloom = self.recent_blocks_bloom.write();
            for (i, (b, _, _)) in blocks.iter().enumerate() {
                if !existing[i] {
                    bloom.insert(b.id.as_bytes());
                }
            }
        }
        let t_trim = t_trim_start.elapsed();

        // Export sub-stage timings via the atomic counters on RocksStore.
        // pms-core's metrics sampler reads these out and exposes them as
        // Prometheus counters (pms-storage doesn't pull in prometheus).
        use std::sync::atomic::Ordering;
        self.append_us_dedup
            .fetch_add(t_dedup.as_micros() as u64, Ordering::Relaxed);
        self.append_us_build
            .fetch_add(t_build.as_micros() as u64, Ordering::Relaxed);
        self.append_us_write
            .fetch_add(t_write.as_micros() as u64, Ordering::Relaxed);
        self.append_us_trim
            .fetch_add(t_trim.as_micros() as u64, Ordering::Relaxed);
        let _ = t_total_start;

        Ok(count)
    }

    /// Persist the activity-index entries for a batch of blocks in a
    /// single `WriteBatch`. Called by the dedicated activity-writer
    /// background task — see `pms_core::background_activity`.
    async fn append_activity_batch(
        &self,
        blocks: &[(&StoredBlock, i64)],
    ) -> Result<usize> {
        use rocksdb::WriteBatch;

        if blocks.is_empty() {
            return Ok(0);
        }

        let mut batch = WriteBatch::default();
        let mut count = 0usize;
        for (b, ts) in blocks {
            self.apply_addr_activity_indices(&mut batch, b, *ts, None)?;
            count += 1;
        }

        if count > 0 {
            self.db.write(batch)?;
        }
        Ok(count)
    }

    async fn is_outpoint_spent(&self, txid: &str, index: u32) -> Result<bool> {
        let cf_utxo_spent = self.cf("utxo_spent");
        let key = make_utxo_key(txid, index);
        Ok(self.db.get_cf(&cf_utxo_spent, &key)?.is_some())
    }

    async fn is_bridge_lock_consumed(&self, lock_block_id: &str) -> Result<bool> {
        // Durable anti-replay record: `bridge_consumed[lock_block_id] = mint_block_id`.
        // Written in the atomic batch alongside the BridgeMint block (see
        // `append_block_atomic_with_utxo` / `append_blocks_batch`). Survives
        // restarts and FIFO eviction of any RAM tracker (audit rang 3, B3).
        let cf_bridge_consumed = self.cf("bridge_consumed");
        Ok(self
            .db
            .get_cf(&cf_bridge_consumed, lock_block_id.as_bytes())?
            .is_some())
    }

    async fn block_ts_ms(&self, id: &str) -> Result<Option<i64>> {
        let cf_i2t = self.cf("id2ts");
        match self.db.get_cf(&cf_i2t, id.as_bytes())? {
            Some(v) if v.len() == 8 => {
                let mut b = [0u8; 8];
                b.copy_from_slice(&v);
                Ok(Some(u64::from_be_bytes(b) as i64))
            }
            _ => Ok(None),
        }
    }

    fn is_write_stopped(&self) -> Option<bool> {
        // RocksDB exposes the boolean as the literal string "0" or "1".
        // Anything else (property unsupported, parse failure) returns None
        // so the metrics sampler treats this sample as unavailable rather
        // than reporting a misleading "not stalled" reading.
        match self.db.property_value("rocksdb.is-write-stopped") {
            Ok(Some(v)) => match v.trim() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            },
            _ => None,
        }
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
        RocksStore::recent_ids_by_address_and_categories(
            self, addr, categories, after_ts, after_id, limit,
        )
        .await
    }
}
