//! Activity index operations on RocksStore.
//!
//! Manages the `addr_activity`, `addr_type_activity`, and `activity_items`
//! column families for per-address activity tracking with pagination.

use crate::rocks_store::store::{RocksStore, ReindexStats};
use anyhow::Result;
use rocksdb::{DBWithThreadMode, Direction, IteratorMode, MultiThreaded};
use std::collections::HashMap;

impl RocksStore {
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
        let iter = self
            .db
            .iterator_cf(&cf_aa, IteratorMode::From(&seek_key, Direction::Reverse));
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

    /// Like `recent_ids_by_address` but also returns the timestamp from the key
    /// and any pre-computed `StoredActivityItem`s from the `activity_items` CF.
    ///
    /// Returns `(entries, next_cursor)` where each entry is
    /// `(block_id, ts_ms, Option<Vec<StoredActivityItem>>)`.
    /// When `Some`, the items can be used directly without fetching the full block.
    pub async fn recent_activity_items_by_address(
        &self,
        addr: &str,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(
        Vec<(String, i64, Option<Vec<crate::activity_item::StoredActivityItem>>)>,
        Option<(i64, String, bool)>,
    )> {
        use crate::helpers::{key_addr_activity, parse_addr_activity_key, prefix_addr_activity};

        let cf_aa = self.cf("addr_activity");
        let cf_items = self.cf("activity_items");
        let addr_len = addr.len();
        let prefix = prefix_addr_activity(addr);

        let seek_key = if let (Some(ts), Some(id)) = (after_ts, after_id.as_deref()) {
            key_addr_activity(addr, ts, id)
        } else {
            let mut end_key = prefix.clone();
            if let Some(last) = end_key.last_mut() {
                *last = 0x01;
            }
            end_key
        };

        // Phase 1: Collect entries and their keys from the index iterator
        let mut raw_entries: Vec<(String, i64, Vec<u8>)> = Vec::with_capacity(limit + 1);
        let iter = self
            .db
            .iterator_cf(&cf_aa, IteratorMode::From(&seek_key, Direction::Reverse));
        let mut skipped_cursor = false;

        for item in iter {
            let (k, _) = item?;

            if k.len() < prefix.len() || &k[..prefix.len()] != prefix.as_slice() {
                break;
            }

            let Some((ts, block_id)) = parse_addr_activity_key(&k, addr_len) else {
                continue;
            };

            if !skipped_cursor && after_ts.is_some() && after_id.is_some() {
                if Some(ts) == after_ts && Some(&block_id) == after_id.as_ref() {
                    skipped_cursor = true;
                    continue;
                }
                skipped_cursor = true;
            }

            raw_entries.push((block_id, ts, k.to_vec()));
            if raw_entries.len() > limit {
                break;
            }
        }

        let has_more = raw_entries.len() > limit;
        if has_more {
            raw_entries.pop();
        }

        let next_cursor = if has_more {
            raw_entries.last().map(|(id, ts, _)| (*ts, id.clone(), true))
        } else {
            None
        };

        // Phase 2: Batch-fetch pre-computed items via multi_get_cf
        let keys: Vec<_> = raw_entries.iter().map(|(_, _, k)| (&cf_items, k.as_slice())).collect();
        let precomputed_results = self.db.multi_get_cf(keys);

        // Phase 3: Zip results
        let mut entries = Vec::with_capacity(raw_entries.len());
        for ((block_id, ts, _), result) in raw_entries.into_iter().zip(precomputed_results) {
            let precomputed = result.ok().flatten().and_then(|v| {
                serde_json::from_slice::<Vec<crate::activity_item::StoredActivityItem>>(&v).ok()
            });
            entries.push((block_id, ts, precomputed));
        }

        Ok((entries, next_cursor))
    }

    /// Like `recent_ids_by_address_and_categories` but also returns timestamps
    /// and pre-computed `StoredActivityItem`s from the `activity_items` CF.
    pub async fn recent_activity_items_by_address_and_categories(
        &self,
        addr: &str,
        categories: &[u8],
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(
        Vec<(String, i64, Option<Vec<crate::activity_item::StoredActivityItem>>)>,
        Option<(i64, String, bool)>,
    )> {
        // Get IDs + timestamps from the typed scan.
        let (ids, cursor) = self
            .recent_ids_with_ts_by_address_and_categories(
                addr, categories, after_ts, after_id, limit,
            )
            .await?;

        if ids.is_empty() {
            return Ok((vec![], cursor));
        }

        // Build keys for batch lookup
        let cf_items = self.cf("activity_items");
        let item_keys: Vec<Vec<u8>> = ids
            .iter()
            .map(|(id, ts)| crate::helpers::key_addr_activity(addr, *ts, id))
            .collect();

        // Batch-fetch all pre-computed items in one multi_get_cf call
        let multi_keys: Vec<_> = item_keys.iter().map(|k| (&cf_items, k.as_slice())).collect();
        let results = self.db.multi_get_cf(multi_keys);

        let mut entries = Vec::with_capacity(ids.len());
        for ((id, ts), result) in ids.into_iter().zip(results) {
            let precomputed = result.ok().flatten().and_then(|v| {
                serde_json::from_slice::<Vec<crate::activity_item::StoredActivityItem>>(&v).ok()
            });
            entries.push((id, ts, precomputed));
        }

        Ok((entries, cursor))
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

    /// Write **both** `addr_activity` (untyped) and `addr_type_activity` (typed)
    /// index entries for a block in a single atomic WriteBatch.
    ///
    /// Used for encrypted payloads where the coordinator knows the plain payload
    /// before encryption and can extract addresses + categories.
    ///
    /// If `precomputed_items` is provided, they are also written to the
    /// `activity_items` CF for fast reads.
    pub fn write_addr_activity_entries_with_categories(
        &self,
        block_id: &str,
        addresses: &[String],
        typed: &[(String, crate::helpers::ActivityCategory)],
        precomputed_items: Option<&std::collections::HashMap<String, Vec<crate::activity_item::StoredActivityItem>>>,
    ) -> Result<()> {
        if addresses.is_empty() && typed.is_empty() {
            return Ok(());
        }
        let ts = crate::helpers::now_ms_i64();
        let mut batch = rocksdb::WriteBatch::default();

        // Untyped index (addr_activity)
        if !addresses.is_empty() {
            let cf_aa = self.cf("addr_activity");
            for addr in addresses {
                let key = crate::helpers::key_addr_activity(addr, ts, block_id);
                batch.put_cf(&cf_aa, &key, b"");
            }
        }

        // Typed index (addr_type_activity)
        if !typed.is_empty() {
            let cf_ata = self.cf("addr_type_activity");
            for (addr, cat) in typed {
                let key =
                    crate::helpers::key_addr_type_activity(addr, cat.as_byte(), ts, block_id);
                batch.put_cf(&cf_ata, &key, b"");
            }
        }

        // Pre-computed activity items (activity_items CF)
        if let Some(items_map) = precomputed_items {
            let cf_items = self.cf("activity_items");
            for (addr, items) in items_map {
                if !items.is_empty() {
                    let key = crate::helpers::key_addr_activity(addr, ts, block_id);
                    let val = serde_json::to_vec(items)?;
                    batch.put_cf(&cf_items, &key, &val);
                }
            }
        }

        self.db.write(batch)?;
        Ok(())
    }

    /// Rebuild `addr_activity` and `addr_type_activity` indexes for ALL blocks
    /// in the store. Uses the original block timestamp from `id2ts` to preserve
    /// chronological order.
    ///
    /// Only indexes `Plain` payloads (encrypted payloads cannot be decoded
    /// without the recipient's private key — the coordinator fix in `f0109ad`
    /// handles indexing at creation time for new encrypted blocks).
    pub fn reindex_all_activity(&self) -> Result<ReindexStats> {
        let cf_blocks = self.cf("blocks");
        let cf_i2t = self.cf("id2ts");
        let cf_aa = self.cf("addr_activity");
        let cf_ata = self.cf("addr_type_activity");

        let mut stats = ReindexStats::default();

        for kv in iter_cf_all(&self.db, &cf_blocks) {
            let (_k, v) = kv?;
            stats.total_blocks += 1;

            let Ok(sb) = serde_json::from_slice::<crate::StoredBlock>(&v) else {
                continue;
            };

            let Some(pjson) = &sb.payload_json else {
                stats.skipped_no_payload += 1;
                continue;
            };

            let Ok(env) =
                serde_json::from_str::<pms_types_payload::PayloadEnvelope>(pjson)
            else {
                continue;
            };

            let plain = match env {
                pms_types_payload::PayloadEnvelope::Plain(p) => p,
                pms_types_payload::PayloadEnvelope::Encrypted(_) => {
                    stats.skipped_encrypted += 1;
                    continue;
                }
            };

            // Get original timestamp from id2ts
            let ts = match self.db.get_cf(&cf_i2t, sb.id.as_bytes())? {
                Some(raw) if raw.len() == 8 => {
                    let mut be = [0u8; 8];
                    be.copy_from_slice(&raw);
                    u64::from_be_bytes(be) as i64
                }
                _ => crate::helpers::now_ms_i64(), // fallback
            };

            let addrs = crate::helpers::extract_involved_addresses(&plain);
            let typed = crate::helpers::extract_involved_with_category(&plain);

            if addrs.is_empty() && typed.is_empty() {
                continue;
            }

            let mut batch = rocksdb::WriteBatch::default();

            for addr in &addrs {
                let key = crate::helpers::key_addr_activity(addr, ts, &sb.id);
                batch.put_cf(&cf_aa, &key, b"");
            }
            for (addr, cat) in &typed {
                let key =
                    crate::helpers::key_addr_type_activity(addr, cat.as_byte(), ts, &sb.id);
                batch.put_cf(&cf_ata, &key, b"");
            }

            self.db.write(batch)?;
            stats.indexed += 1;

            if stats.indexed % 10_000 == 0 {
                tracing::info!(
                    "Reindex progress: {} indexed / {} scanned",
                    stats.indexed,
                    stats.total_blocks,
                );
            }
        }

        tracing::info!(
            "Reindex complete: {} indexed, {} encrypted skipped, {} total blocks",
            stats.indexed,
            stats.skipped_encrypted,
            stats.total_blocks,
        );

        Ok(stats)
    }

    /// Rebuild `activity_items` CF for ALL blocks in the store.
    ///
    /// For each block, extracts the plain payload, computes per-address items
    /// via `precompute_all_items()`, and writes them to the `activity_items` CF.
    ///
    /// For TxUtxo payloads, `sender_addr = None` because historical UTXOs are
    /// already spent (this means sender-based classification may be incomplete
    /// for old blocks — the fallback path will handle it).
    pub fn reindex_all_activity_items(&self) -> Result<ReindexStats> {
        let cf_blocks = self.cf("blocks");
        let cf_i2t = self.cf("id2ts");
        let cf_items = self.cf("activity_items");

        let mut stats = ReindexStats::default();
        let mut batch = rocksdb::WriteBatch::default();
        let mut batch_count = 0usize;
        const FLUSH_EVERY: usize = 1000;

        for kv in iter_cf_all(&self.db, &cf_blocks) {
            let (_k, v) = kv?;
            stats.total_blocks += 1;

            let Ok(sb) = serde_json::from_slice::<crate::StoredBlock>(&v) else {
                continue;
            };

            let Some(pjson) = &sb.payload_json else {
                stats.skipped_no_payload += 1;
                continue;
            };

            let Ok(env) =
                serde_json::from_str::<pms_types_payload::PayloadEnvelope>(pjson)
            else {
                continue;
            };

            let plain = match env {
                pms_types_payload::PayloadEnvelope::Plain(p) => p,
                pms_types_payload::PayloadEnvelope::Encrypted(_) => {
                    stats.skipped_encrypted += 1;
                    continue;
                }
            };

            // Get original timestamp from id2ts
            let ts = match self.db.get_cf(&cf_i2t, sb.id.as_bytes())? {
                Some(raw) if raw.len() == 8 => {
                    let mut be = [0u8; 8];
                    be.copy_from_slice(&raw);
                    u64::from_be_bytes(be) as i64
                }
                _ => crate::helpers::now_ms_i64(),
            };

            let addrs = crate::helpers::extract_involved_addresses(&plain);
            if addrs.is_empty() {
                continue;
            }

            // Pre-compute items (sender=None for TxUtxo — UTXOs already spent)
            let items_map = crate::helpers::precompute_all_items(&plain, &addrs, None);

            for (addr, items) in &items_map {
                if !items.is_empty() {
                    let key = crate::helpers::key_addr_activity(addr, ts, &sb.id);
                    let val = serde_json::to_vec(items)?;
                    batch.put_cf(&cf_items, &key, &val);
                }
            }

            stats.indexed += 1;
            batch_count += 1;

            // Flush batch every N blocks to bound memory usage
            if batch_count >= FLUSH_EVERY {
                self.db.write(batch)?;
                batch = rocksdb::WriteBatch::default();
                batch_count = 0;
            }

            if stats.indexed % 10_000 == 0 {
                tracing::info!(
                    "Reindex activity_items progress: {} indexed / {} scanned",
                    stats.indexed,
                    stats.total_blocks,
                );
            }
        }

        // Flush remaining
        if batch_count > 0 {
            self.db.write(batch)?;
        }

        tracing::info!(
            "Reindex activity_items complete: {} indexed, {} encrypted skipped, {} total blocks",
            stats.indexed,
            stats.skipped_encrypted,
            stats.total_blocks,
        );

        Ok(stats)
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
        let (ids_with_ts, cursor) = self
            .recent_ids_with_ts_by_address_and_categories(
                addr, categories, after_ts, after_id, limit,
            )
            .await?;
        let ids = ids_with_ts.into_iter().map(|(id, _)| id).collect();
        Ok((ids, cursor))
    }

    /// Like `recent_ids_by_address_and_categories` but also returns the
    /// timestamp from the `addr_type_activity` key for each entry.
    /// Used internally by `recent_activity_items_by_address_and_categories`
    /// to look up pre-computed items with the correct key timestamp.
    async fn recent_ids_with_ts_by_address_and_categories(
        &self,
        addr: &str,
        categories: &[u8],
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(Vec<(String, i64)>, Option<(i64, String, bool)>)> {
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

            let seek_key = if let (Some(ts), Some(id)) = (after_ts, after_id.as_deref()) {
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
            let iter = self
                .db
                .iterator_cf(&cf_ata, IteratorMode::From(&seek_key, Direction::Reverse));
            let mut skipped_cursor = false;

            for item in iter {
                let (k, _) = item?;
                if k.len() < prefix.len() || &k[..prefix.len()] != prefix.as_slice() {
                    break;
                }
                let Some((_cat, ts, block_id)) = parse_addr_type_activity_key(&k, addr_len) else {
                    continue;
                };

                if !skipped_cursor && after_ts.is_some() && after_id.is_some() {
                    if Some(ts) == after_ts && Some(&block_id) == after_id.as_ref() {
                        skipped_cursor = true;
                        continue;
                    }
                    skipped_cursor = true;
                }

                ids.push((block_id, ts));
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
            iter: rocksdb::DBIteratorWithThreadMode<'a, DBWithThreadMode<MultiThreaded>>,
            current: Option<(i64, String)>, // (ts, block_id) of the peeked entry
            addr_len: usize,
        }

        let mut iters: Vec<CatIter<'_>> = Vec::with_capacity(categories.len());

        for &cat in categories {
            let prefix = prefix_addr_type_activity(addr, cat);
            let seek_key = if let (Some(ts), Some(id)) = (after_ts, after_id.as_deref()) {
                key_addr_type_activity(addr, cat, ts, id)
            } else {
                let mut end_key = prefix.clone();
                if let Some(last) = end_key.last_mut() {
                    *last = cat.wrapping_add(1);
                }
                end_key
            };

            let iter = self
                .db
                .iterator_cf(&cf_ata, IteratorMode::From(&seek_key, Direction::Reverse));

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

        // Merge: pick the iterator with the highest timestamp each round.
        // Invariant: every CatIter in `iters` has `current.is_some()`.
        // We use defensive Option handling to avoid panics on corruption.
        let mut ids = Vec::with_capacity(limit + 1);
        while !iters.is_empty() && ids.len() <= limit {
            // Find iterator with the newest entry
            let Some(best_idx) = iters
                .iter()
                .enumerate()
                .filter(|(_, ci)| ci.current.is_some())
                .max_by(|(_, a), (_, b)| {
                    // SAFETY: filter above guarantees is_some()
                    let (ts_a, id_a) = a.current.as_ref().unwrap();
                    let (ts_b, id_b) = b.current.as_ref().unwrap();
                    ts_a.cmp(ts_b).then_with(|| id_a.cmp(id_b))
                })
                .map(|(i, _)| i)
            else {
                break; // All iterators exhausted (should not happen given while guard)
            };

            let Some((ts, block_id)) = iters[best_idx].current.clone() else {
                // Defensive: iterator became None unexpectedly
                iters.swap_remove(best_idx);
                continue;
            };
            ids.push((block_id, ts));

            iters[best_idx].advance_next();
            if iters[best_idx].current.is_none() {
                iters.swap_remove(best_idx);
            }
        }

        self.finalize_ids_cursor(ids, limit).await
    }

    /// Shared logic for building the pagination cursor from a collected ids vec.
    /// Each entry is `(block_id, ts)` where ts comes from the `addr_type_activity` key.
    async fn finalize_ids_cursor(
        &self,
        mut ids: Vec<(String, i64)>,
        limit: usize,
    ) -> Result<(Vec<(String, i64)>, Option<(i64, String, bool)>)> {
        let has_more = ids.len() > limit;
        if has_more {
            ids.pop();
        }
        let next_cursor = if has_more {
            ids.last().map(|(last_id, ts)| (*ts, last_id.clone(), true))
        } else {
            None
        };
        Ok((ids, next_cursor))
    }
}

/// Iterate over all key-value pairs in a column family.
pub(crate) fn iter_cf_all<'a>(
    db: &'a super::store::PmsDb,
    cf: &impl rocksdb::AsColumnFamilyRef,
) -> impl Iterator<Item = anyhow::Result<(Box<[u8]>, Box<[u8]>)>> + 'a {
    db.iterator_cf(cf, IteratorMode::Start).map(|res| {
        let (k, v) = res?;
        Ok((k, v))
    })
}

/// Helper to build a UTXO key from transaction ID and output index.
pub(crate) fn make_utxo_key(txid: &str, index: u32) -> Vec<u8> {
    // Pre-allocate: txid (64 hex chars typical) + '#' + index (up to 10 digits)
    let mut key = Vec::with_capacity(txid.len() + 1 + 10);
    key.extend_from_slice(txid.as_bytes());
    key.push(b'#');
    // itoa is faster than .to_string() for integer formatting
    let mut buf = itoa::Buffer::new();
    key.extend_from_slice(buf.format(index).as_bytes());
    key
}
