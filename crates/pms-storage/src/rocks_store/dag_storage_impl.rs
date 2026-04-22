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
use serde::Serialize;

use super::activity_index::iter_cf_all;

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

                let val = OutVal {
                    addr,
                    amt,
                    asset_id: asset_id.as_deref(),
                };
                let json = serde_json::to_vec(&val)?;
                batch.put_cf(&cf_utxo, &key, &json);
            }
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
        blocks: &[(&StoredBlock, Option<&UtxoDelta>)],
    ) -> Result<usize> {
        use rocksdb::WriteBatch;

        if blocks.is_empty() {
            return Ok(0);
        }

        let cf_blocks = self.cf("blocks");

        // Batch dedup check: single multi_get_cf call instead of N get_cf calls.
        // We clone the Arc per key because multi_get_cf requires AsColumnFamilyRef
        // (Arc, not &Arc). The Arc clone is cheap (refcount bump).
        let keys: Vec<_> = blocks
            .iter()
            .map(|(b, _)| (cf_blocks.clone(), b.id.as_bytes().to_vec()))
            .collect();
        let existing: Vec<bool> = self
            .db
            .multi_get_cf(keys.iter().map(|(cf, k)| (cf, k.as_slice())))
            .into_iter()
            .map(|r| matches!(r, Ok(Some(_))))
            .collect();

        // Build a single mega WriteBatch for all new blocks
        let mut batch = WriteBatch::default();
        let mut count = 0usize;

        for (i, (b, delta)) in blocks.iter().enumerate() {
            if existing[i] {
                continue; // block already persisted
            }

            // UTXO Delta
            if let Some(d) = delta {
                let cf_utxo = self.cf("utxo");
                let cf_utxo_spent = self.cf("utxo_spent");

                for (txid, idx) in &d.spend {
                    let key = make_utxo_key(txid, *idx);
                    batch.delete_cf(&cf_utxo, &key);
                    batch.put_cf(&cf_utxo_spent, &key, b.id.as_bytes());
                }

                for (txid, idx, addr, amt, asset_id) in &d.create {
                    let key = make_utxo_key(txid, *idx);

                    #[derive(Serialize)]
                    struct OutVal<'a> {
                        addr: &'a str,
                        amt: &'a str,
                        #[serde(
                            default,
                            skip_serializing_if = "Option::is_none",
                            rename = "ast"
                        )]
                        asset_id: Option<&'a str>,
                    }

                    let val = OutVal {
                        addr,
                        amt,
                        asset_id: asset_id.as_deref(),
                    };
                    let json = serde_json::to_vec(&val)?;
                    batch.put_cf(&cf_utxo, &key, &json);
                }
            }

            // DAG indices
            self.apply_dag_indices(&mut batch, b)?;

            // Activity indices
            let ts = crate::helpers::now_ms_i64();
            self.apply_addr_activity_indices(&mut batch, b, ts, None)?;

            count += 1;
        }

        if count > 0 {
            // Single atomic write for all blocks
            self.db.write(batch)?;
            // Trim tips once for the whole batch
            self.maybe_trim_tips()?;
        }

        Ok(count)
    }

    async fn is_outpoint_spent(&self, txid: &str, index: u32) -> Result<bool> {
        let cf_utxo_spent = self.cf("utxo_spent");
        let key = make_utxo_key(txid, index);
        Ok(self.db.get_cf(&cf_utxo_spent, &key)?.is_some())
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
