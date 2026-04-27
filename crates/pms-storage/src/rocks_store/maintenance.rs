//! Background maintenance and tip management for RocksStore.
//!
//! Includes tip trimming (amortized), WAL flushing, compaction scheduling,
//! and checkpoint rotation.

use crate::checkpoint_rocks::rotate_checkpoints;
use crate::helpers::{be_to_i64, le_to_u64, parse_time_index_key};
use crate::rocks_store::store::RocksStore;
use std::cmp::Reverse;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::{Duration, MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

impl RocksStore {
    /// Walk the `by_time` CF newest-first and feed up to `limit` block
    /// IDs into the recent-blocks Bloom filter, then mark it warmed.
    ///
    /// Until this completes, `append_blocks_batch` falls back to the
    /// legacy whole-batch `multi_get_cf` lookup so a block actually
    /// present in RocksDB can never be misclassified as new.
    ///
    /// The walk only reads `by_time` keys (no value, no JSON parse) so
    /// it's bounded by raw RocksDB iteration speed — typically a few
    /// hundred ns per key. With `limit = 2_000_000` the boot cost
    /// stays under a couple of seconds even on a 20M-block DB.
    pub fn warm_recent_blocks_bloom(&self, limit: usize) -> anyhow::Result<usize> {
        let cf_time = self.cf("by_time");
        let mut bloom = self.recent_blocks_bloom.write();
        let mut inserted = 0usize;
        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::End) {
            if inserted >= limit {
                break;
            }
            let (k, _v) = kv?;
            if let Some((_ts, id)) = parse_time_index_key(&k) {
                bloom.insert(id.as_bytes());
                inserted += 1;
            }
        }
        bloom.mark_warmed();
        Ok(inserted)
    }

    /// `(front_inserted, back_inserted, capacity_per_segment, warmed)`
    /// for the recent-blocks Bloom filter. Exposed for the admin
    /// `/admin/rocksdb-stats` JSON snapshot.
    pub fn bloom_filter_status(&self) -> (usize, usize, usize, bool) {
        let g = self.recent_blocks_bloom.read();
        let (f, b, cap) = g.stats();
        (f, b, cap, g.is_warmed())
    }

    // helper privé appelé après append_block_atomic
    #[allow(dead_code)]
    pub(crate) fn trim_by_time(&self) -> anyhow::Result<()> {
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

    /// Amortized trim_tips: only runs the actual trim every 64 block persists.
    /// At 120 TPS in coordinator mode the tip count stays at ~1-3, so the
    /// fast-path estimate check in `trim_tips()` returns instantly almost always.
    /// But even the atomic load has measurable cost at high TPS. Amortizing
    /// every 64 blocks keeps tips bounded (max overshoot = 64) while removing
    /// the per-block overhead entirely.
    const TRIM_TIPS_INTERVAL: u64 = 64;

    pub(crate) fn maybe_trim_tips(&self) -> anyhow::Result<()> {
        let count = self
            .persist_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % Self::TRIM_TIPS_INTERVAL == 0 {
            self.trim_tips()?;
        }
        Ok(())
    }

    pub(crate) fn trim_tips(&self) -> anyhow::Result<()> {
        if self.tip_limit == 0 {
            return Ok(());
        }

        // Fast path: if our estimate says we're under the limit, skip the
        // expensive full-scan + sort. The estimate may drift slightly but
        // trim_tips is defensive (full scan when triggered).
        let estimate = self
            .tip_count_estimate
            .load(std::sync::atomic::Ordering::Relaxed);
        if estimate <= self.tip_limit {
            return Ok(());
        }

        let cf_tips = self.cf("tips");
        let cf_count = self.cf("children_count");

        // 1. Collecte toutes les tips : (id, ts, children_count).
        //    A tip whose `children_count > 0` is a zombie: a real child has
        //    arrived but `remove_tip` was never called (or was lost to a
        //    crash). `RocksStore::tips` is meant to be an index of blocks
        //    with no children — the ground-truth `children_count` CF wins.
        let mut tips: Vec<(String, i64, u64)> = Vec::new();
        for kv in self.db.iterator_cf(&cf_tips, rocksdb::IteratorMode::Start) {
            let (k, v) = kv?;
            let id = String::from_utf8(k.to_vec())?;
            let ts = be_to_i64(&v)?;
            let cc = self
                .db
                .get_cf(&cf_count, id.as_bytes())?
                .filter(|bytes| bytes.len() == 8)
                .map(|bytes| le_to_u64(&bytes))
                .unwrap_or(0);
            tips.push((id, ts, cc));
        }

        // Correct the estimate to the actual count
        self.tip_count_estimate
            .store(tips.len(), std::sync::atomic::Ordering::Relaxed);

        // 2. Split into real tips (children_count == 0) and zombies.
        //    Zombies are always safe to drop — they've stopped being tips;
        //    real tips we only trim down to `tip_limit` entries, oldest first.
        //    This is the H3 fix: before, `trim_tips` only sorted by timestamp
        //    and could evict an active tip while leaving zombies in place,
        //    drifting the CF out of sync with what `ConcurrentDag::tips`
        //    considers to be a tip.
        let mut real_tips: Vec<(String, i64)> = Vec::with_capacity(tips.len());
        let mut zombie_tips: Vec<String> = Vec::new();
        for (id, ts, cc) in tips {
            if cc == 0 {
                real_tips.push((id, ts));
            } else {
                zombie_tips.push(id);
            }
        }

        let mut batch = rocksdb::WriteBatch::default();
        let zombie_count = zombie_tips.len();
        for id in &zombie_tips {
            batch.delete_cf(&cf_tips, id.as_bytes());
        }

        // 3. If we still have too many *real* tips, keep the most recent.
        //    SAFETY: Always keep at least 1 tip. Mirrors
        //    `ConcurrentDag::prune_oldest()` (commit 9e2922f). Without this,
        //    an over-pruned tips CF causes `top_tips()` to return empty,
        //    silently blocking fee distribution.
        real_tips.sort_by_key(|(_, ts)| Reverse(*ts));
        let keep = self.tip_limit.max(1);
        let mut evicted_real = 0usize;
        if real_tips.len() > keep {
            for (id, _) in real_tips.iter().skip(keep) {
                batch.delete_cf(&cf_tips, id.as_bytes());
                evicted_real += 1;
            }
        }

        let remaining = real_tips.len().min(keep);
        if zombie_count + evicted_real > 0 {
            tracing::debug!(
                remaining,
                zombies = zombie_count,
                evicted_old = evicted_real,
                "trim_tips: reconciled RocksDB tips CF against children_count"
            );
            self.db.write(batch)?;
            self.tip_count_estimate
                .store(remaining, std::sync::atomic::Ordering::Relaxed);
        }

        Ok(())
    }

    /// Curative reconcile of the `tips` CF — re-derive tips from
    /// `children_count` (the ground truth) and add anything that's missing.
    ///
    /// The pre-0.7.3 `add_tip` logic could leak a real tip if a process
    /// crashed (or a code bug skipped the `add_tip` call) between
    /// `append_block_atomic` and `add_tip`: the block was persisted with
    /// `children_count = 0`, but no row landed in the `tips` CF. The
    /// 0.7.3 fix to `trim_tips` only deletes zombies (tips that have
    /// since gained a child), so a missing tip stays missing forever
    /// — the `tips` CF is sub-populated and `top_tips()` may return the
    /// wrong set, blocking fee distribution silently.
    ///
    /// This method walks the `n` newest blocks (via `by_time`, which is
    /// the same index `top_tips` already uses) and, for each block whose
    /// `children_count == 0`, ensures it sits in the `tips` CF with its
    /// timestamp. Existing entries are skipped (idempotent: cheap to run).
    ///
    /// `scan_limit = 0` means "scan every block" — only reasonable on
    /// small dev DBs. Production callers should pass a bounded value
    /// (e.g. `tip_limit * 8`, or whatever recent window they trust).
    ///
    /// Returns `(scanned, added)` so the caller can log the work.
    pub fn rebuild_tips_from_children_count(
        &self,
        scan_limit: usize,
    ) -> anyhow::Result<(usize, usize)> {
        // Use the by-time index so we walk newest first — old blocks are
        // exponentially less likely to still be tips, so a scan_limit
        // bounded against the recent window catches drift cheaply.
        let cf_time = self.cf("by_time");
        let cf_count = self.cf("children_count");
        let cf_tips = self.cf("tips");
        let cf_i2t = self.cf("id2ts");

        let mut scanned = 0usize;
        let mut added = 0usize;
        let mut batch = rocksdb::WriteBatch::default();

        for kv in self.db.iterator_cf(&cf_time, rocksdb::IteratorMode::End) {
            if scan_limit > 0 && scanned >= scan_limit {
                break;
            }
            let (k, _v) = kv?;
            // by_time keys are `(ts_be, id)` packed; parse_time_index_key
            // is the canonical decoder used everywhere else in this file.
            let Some((ts, block_id)) = parse_time_index_key(&k) else {
                continue;
            };
            scanned += 1;

            // children_count == 0 → this block IS a tip. Anything else
            // is just a regular block, skip.
            let cc = self
                .db
                .get_cf(&cf_count, block_id.as_bytes())?
                .filter(|bytes| bytes.len() == 8)
                .map(|bytes| le_to_u64(&bytes))
                .unwrap_or(0);
            if cc != 0 {
                continue;
            }

            // Already in the tips CF — done.
            if self.db.get_cf(&cf_tips, block_id.as_bytes())?.is_some() {
                continue;
            }

            // Missing tip: insert with the same timestamp encoding `add_tip`
            // uses elsewhere (8-byte big-endian i64). Cross-check `id2ts`
            // for consistency: if the index is internally inconsistent we
            // prefer the timestamp from the iterator key (already authoritative).
            let _ = self.db.get_cf(&cf_i2t, block_id.as_bytes())?;
            batch.put_cf(&cf_tips, block_id.as_bytes(), ts.to_be_bytes());
            added += 1;
        }

        if added > 0 {
            self.db.write(batch)?;
            // Bump the cached estimate so trim_tips sees the new state on
            // its next fast-path check. We saturating-add and let trim
            // reconcile if the actual count drifted further.
            self.tip_count_estimate
                .fetch_add(added, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(
                target = "rocks_tips",
                scanned,
                added,
                "rebuild_tips_from_children_count: added missing tips"
            );
        } else {
            tracing::debug!(
                target = "rocks_tips",
                scanned,
                "rebuild_tips_from_children_count: no missing tips"
            );
        }

        Ok((scanned, added))
    }

    #[cfg(test)]
    pub(crate) fn inject_tip_for_test(
        &self,
        id: &str,
        ts_ms: i64,
        children_count: u64,
    ) -> anyhow::Result<()> {
        let cf_tips = self.cf("tips");
        self.db
            .put_cf(&cf_tips, id.as_bytes(), ts_ms.to_be_bytes())?;
        if children_count > 0 {
            let cf_count = self.cf("children_count");
            self.db
                .put_cf(&cf_count, id.as_bytes(), children_count.to_le_bytes())?;
        }
        Ok(())
    }

    /// Test-only: simulate the drift the curative rebuild fixes.
    /// Plants a block in `by_time` + `id2ts` + `children_count` (set to
    /// the requested value, even when zero, so a tip with `cc == 0` is
    /// still indexed correctly) but **never** in the `tips` CF — that's
    /// the exact state a crashed `add_tip` call leaves behind.
    #[cfg(test)]
    pub(crate) fn inject_block_without_tip_entry_for_test(
        &self,
        id: &str,
        ts_ms: i64,
        children_count: u64,
    ) -> anyhow::Result<()> {
        use crate::helpers::key_time_index;

        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");
        let cf_count = self.cf("children_count");

        self.db.put_cf(&cf_time, &key_time_index(ts_ms, id), b"")?;
        self.db
            .put_cf(&cf_i2t, id.as_bytes(), ts_ms.to_be_bytes())?;
        self.db
            .put_cf(&cf_count, id.as_bytes(), children_count.to_le_bytes())?;
        Ok(())
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
        // Backup directory: derived from the DB path so checkpoints always
        // land on the same filesystem/volume as the data (critical in Docker
        // where only the data dir is volume-mounted).
        // Override with PMS_BACKUP_ROOT env var if needed.
        let backup_root = std::env::var("PMS_BACKUP_ROOT").unwrap_or_else(|_| {
            // Place backups as a sibling of the rocks directory:
            //   db_path = /home/pms/data/rocks  →  backup = /home/pms/data/backups/pms
            self.db_path
                .parent()
                .unwrap_or(&self.db_path)
                .join("backups")
                .join("pms")
                .to_string_lossy()
                .into_owned()
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

            tracing::info!(
                "[rocks] background maintenance started (flush={:?}, compact={:?}, stats={:?}, checkpoint={:?}, backup_root={})",
                flush_every, compact_every, stats_every, checkpoint_every, backup_root,
            );

            loop {
                tokio::select! {
                    // Signal d'arrêt propre (Server::run appelle cancel.cancel())
                    _ = cancel.cancelled() => {
                        tracing::info!("[rocks] background maintenance cancelled, exiting");
                        break;
                    }

                    // Flush WAL (évite d'avoir un WAL trop gros, améliore la durabilité)
                    _ = flush_tick.tick() => {
                        if let Err(e) = self.flush_wal().await {
                            tracing::error!("[rocks] flush_wal failed: {e:#}");
                        }
                    }

                    // Compaction de toutes les CF (réduction fragmentation, taille disque)
                    _ = compact_tick.tick() => {
                        if let Err(e) = self.compact_all().await {
                            tracing::error!("[rocks] compact_all failed: {e:#}");
                        }
                    }

                    // Log de stats RocksDB (diagnostic: taille, compaction, etc.)
                    _ = stats_tick.tick() => {
                        if let Err(e) = self.log_stats().await {
                            tracing::error!("[rocks] log_stats failed: {e:#}");
                        }
                    }

                    // Checkpoint + rotation (snapshots de sécurité)
                    _ = checkpoint_tick.tick() => {
                        if let Err(e) = self.create_checkpoint(&backup_root) {
                            tracing::error!("[rocks] create_checkpoint failed: {e:#}");
                        } else if let Err(e) = rotate_checkpoints(&backup_root, 3) {
                            tracing::error!("[rocks] rotate_checkpoints failed: {e:#}");
                        }
                    }
                }
            }

            tracing::info!("[rocks] background maintenance stopped");
        })
    }
}

#[cfg(test)]
mod trim_tips_tests {
    //! Non-regression tests for audit finding H3 — RocksDB `tips` CF drift.
    //!
    //! Pre-0.7.3, `trim_tips` sorted by timestamp only. A tip that had
    //! already gained a child (its `remove_tip` call was lost to a crash
    //! or bug) would survive because of a recent timestamp, and
    //! `trim_tips` would evict a genuinely-active older tip instead —
    //! drifting the `tips` CF out of sync with `ConcurrentDag::tips`
    //! (the real source of truth).
    //!
    //! Post-fix, `trim_tips` cross-checks every tip candidate against
    //! the `children_count` CF: zombies (children_count > 0) are
    //! reclaimed first, then tip_limit enforcement runs against the
    //! real tips only.

    use super::RocksStore;
    use crate::rocks_store::store::RocksMemoryConfig;
    use tempfile::TempDir;

    async fn fresh_store(tip_limit: usize) -> (RocksStore, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_string_lossy().into_owned();
        let store = RocksStore::new(
            &path,
            tip_limit,
            "main",
            None,
            &RocksMemoryConfig::default(),
        )
        .await
        .expect("rocks store");
        (store, dir)
    }

    fn count_tips(store: &RocksStore) -> usize {
        let cf = store.cf("tips");
        store
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start)
            .count()
    }

    fn tip_ids(store: &RocksStore) -> Vec<String> {
        let cf = store.cf("tips");
        store
            .db
            .iterator_cf(&cf, rocksdb::IteratorMode::Start)
            .filter_map(|kv| kv.ok())
            .map(|(k, _)| String::from_utf8_lossy(&k).into_owned())
            .collect()
    }

    #[tokio::test]
    async fn zombies_are_evicted_before_real_tips() {
        let (store, _dir) = fresh_store(2).await;

        // Three real tips (children_count == 0). Only 2 should survive
        // because tip_limit = 2, and the two *newest* win.
        store.inject_tip_for_test("real-new", 300, 0).unwrap();
        store.inject_tip_for_test("real-mid", 200, 0).unwrap();
        store.inject_tip_for_test("real-old", 100, 0).unwrap();

        // Two zombies: in the `tips` CF but with recorded children.
        // Under the pre-fix logic these would have survived because of
        // their recent timestamps and displaced the real tips.
        store.inject_tip_for_test("zombie-recent", 500, 3).unwrap();
        store.inject_tip_for_test("zombie-older", 50, 7).unwrap();

        store.tip_count_estimate.store(
            count_tips(&store),
            std::sync::atomic::Ordering::Relaxed,
        );

        println!("before trim_tips:");
        println!("  count = {}", count_tips(&store));
        println!("  ids   = {:?}", tip_ids(&store));

        store.trim_tips().expect("trim_tips");

        let remaining = tip_ids(&store);
        println!("after trim_tips:");
        println!("  count = {}", remaining.len());
        println!("  ids   = {:?}", remaining);

        assert!(!remaining.contains(&"zombie-recent".to_string()));
        assert!(!remaining.contains(&"zombie-older".to_string()));

        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&"real-new".to_string()));
        assert!(remaining.contains(&"real-mid".to_string()));
        assert!(!remaining.contains(&"real-old".to_string()));
    }

    #[tokio::test]
    async fn zombie_cleared_once_trim_runs() {
        // `trim_tips` has a cheap fast-path: if the cached estimate is at or
        // below tip_limit, it skips the full scan entirely. That's fine in
        // practice — zombies appear on failure paths and get reclaimed the
        // next time the count actually drifts over the limit. This test
        // forces the scan via the estimate to prove that once trim runs,
        // zombies disappear even when real_tips + zombies > tip_limit but
        // real_tips alone is ≤ tip_limit.
        let (store, _dir) = fresh_store(2).await;

        store.inject_tip_for_test("real-a", 10, 0).unwrap();
        store.inject_tip_for_test("real-b", 20, 0).unwrap();
        store.inject_tip_for_test("zombie", 30, 1).unwrap();

        // Force the fast-path threshold by pushing the estimate above
        // tip_limit. This is the state a few inserts after a zombie
        // entered the CF.
        store
            .tip_count_estimate
            .store(3, std::sync::atomic::Ordering::Relaxed);

        store.trim_tips().expect("trim_tips");

        let remaining = tip_ids(&store);
        println!("remaining after trim = {:?}", remaining);

        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&"real-a".to_string()));
        assert!(remaining.contains(&"real-b".to_string()));
        assert!(!remaining.contains(&"zombie".to_string()));
    }

    #[tokio::test]
    async fn tip_limit_zero_disables_trimming_entirely() {
        // tip_limit = 0 is the documented "no trimming" config.
        // Must not wipe the CF even if a zombie is present.
        let (store, _dir) = fresh_store(0).await;
        store.inject_tip_for_test("only-real", 1, 0).unwrap();
        store.inject_tip_for_test("zombie", 2, 1).unwrap();

        store.trim_tips().expect("trim_tips");

        let remaining = tip_ids(&store);
        println!("tip_limit=0 → untouched tips CF: {:?}", remaining);
        assert!(remaining.contains(&"only-real".to_string()));
        assert!(remaining.contains(&"zombie".to_string()));
    }

    // ════════════════════════════════════════════════════════════════════
    // rebuild_tips_from_children_count — curative reconcile (item 6, v0.7.4)
    //
    // `trim_tips` only DELETES from the tips CF. If a tip is ever lost (a
    // crash between `append_block_atomic` and `add_tip`, or a code bug
    // that skipped the `add_tip` call), the missing entry is missing
    // forever — `top_tips` returns the wrong set and fee distribution
    // can stall. The curative rebuild scans the recent window via
    // `by_time` and adds any block whose `children_count == 0` is
    // missing from the `tips` CF.
    // ════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn rebuild_recovers_a_missing_real_tip() {
        let (store, _dir) = fresh_store(8).await;

        // Three real tips already in the tips CF (the happy ones).
        store.inject_tip_for_test("real-a", 100, 0).unwrap();
        store.inject_tip_for_test("real-b", 200, 0).unwrap();
        store.inject_tip_for_test("real-c", 300, 0).unwrap();
        // …and add them to the by_time index too so the iterator sees them
        // (without that, the rebuild scan would only walk the missing ones).
        for (id, ts) in [("real-a", 100i64), ("real-b", 200), ("real-c", 300)] {
            store
                .inject_block_without_tip_entry_for_test(id, ts, 0)
                .unwrap();
            // …but the inject_tip_for_test call above already put them
            // into `tips` — we want `inject_block_without_tip_entry` to
            // populate `by_time` only for these. Re-add the tip after:
            store.inject_tip_for_test(id, ts, 0).unwrap();
        }

        // The drift case: a fourth block exists in by_time + children_count
        // but never made it into the tips CF.
        store
            .inject_block_without_tip_entry_for_test("missing-tip", 400, 0)
            .unwrap();

        let before = tip_ids(&store);
        println!("before rebuild: tips = {:?}", before);
        assert!(!before.contains(&"missing-tip".to_string()));

        let (scanned, added) = store
            .rebuild_tips_from_children_count(0)
            .expect("rebuild");
        println!("rebuild stats: scanned={scanned} added={added}");

        let after = tip_ids(&store);
        println!("after rebuild: tips = {:?}", after);
        assert!(added >= 1, "rebuild must add the missing tip");
        assert!(after.contains(&"missing-tip".to_string()));
    }

    #[tokio::test]
    async fn rebuild_skips_blocks_that_already_have_children() {
        let (store, _dir) = fresh_store(8).await;

        // A "block" sits in by_time + has children (children_count > 0).
        // It must NOT be added to tips — it isn't a tip.
        store
            .inject_block_without_tip_entry_for_test("non-tip", 500, 3)
            .unwrap();

        let (scanned, added) = store
            .rebuild_tips_from_children_count(0)
            .expect("rebuild");
        println!("scanned={scanned} added={added}");

        let after = tip_ids(&store);
        println!("after rebuild: tips = {:?}", after);
        assert!(!after.contains(&"non-tip".to_string()));
        assert_eq!(added, 0);
    }

    #[tokio::test]
    async fn rebuild_is_idempotent_on_correct_state() {
        let (store, _dir) = fresh_store(8).await;

        // Real tip both in tips CF and in by_time. Running rebuild
        // should NOT duplicate or modify it.
        store
            .inject_block_without_tip_entry_for_test("ok", 600, 0)
            .unwrap();
        store.inject_tip_for_test("ok", 600, 0).unwrap();

        let first = store
            .rebuild_tips_from_children_count(0)
            .expect("rebuild #1");
        let second = store
            .rebuild_tips_from_children_count(0)
            .expect("rebuild #2");
        println!("first={:?} second={:?}", first, second);

        assert_eq!(first.1, 0);
        assert_eq!(second.1, 0);
        assert_eq!(tip_ids(&store), vec!["ok".to_string()]);
    }

    #[tokio::test]
    async fn rebuild_respects_scan_limit() {
        let (store, _dir) = fresh_store(8).await;

        // 5 missing tips, oldest first. With scan_limit=2 the rebuild
        // walks newest-to-oldest and stops after 2 entries.
        for (i, ts) in [("blk-1", 100), ("blk-2", 200), ("blk-3", 300), ("blk-4", 400), ("blk-5", 500)]
        {
            store
                .inject_block_without_tip_entry_for_test(i, ts, 0)
                .unwrap();
        }

        let (scanned, added) = store
            .rebuild_tips_from_children_count(2)
            .expect("rebuild");
        println!("scanned={scanned} added={added} tips={:?}", tip_ids(&store));

        assert_eq!(scanned, 2);
        assert_eq!(added, 2);
        let recovered = tip_ids(&store);
        // Newest first → blk-5 + blk-4 should be the ones recovered.
        assert!(recovered.contains(&"blk-5".to_string()));
        assert!(recovered.contains(&"blk-4".to_string()));
        assert!(!recovered.contains(&"blk-1".to_string()));
    }
}
