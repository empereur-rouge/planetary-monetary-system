//! Background maintenance and tip management for RocksStore.
//!
//! Includes tip trimming (amortized), WAL flushing, compaction scheduling,
//! and checkpoint rotation.

use crate::checkpoint_rocks::rotate_checkpoints;
use crate::helpers::{be_to_i64, parse_time_index_key};
use crate::rocks_store::store::RocksStore;
use std::cmp::Reverse;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::{Duration, MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

impl RocksStore {
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

        // 1. Collecte toutes les tips : (id, ts)
        let mut tips: Vec<(String, i64)> = Vec::new();
        for kv in self.db.iterator_cf(&cf_tips, rocksdb::IteratorMode::Start) {
            let (k, v) = kv?;
            let id = String::from_utf8(k.to_vec())?;
            let ts = be_to_i64(&v)?;
            tips.push((id, ts));
        }

        // Correct the estimate to the actual count
        self.tip_count_estimate
            .store(tips.len(), std::sync::atomic::Ordering::Relaxed);

        // 2. Trie par ts DESC (plus récent d'abord).
        tips.sort_by_key(|(_, ts)| Reverse(*ts));

        // 3. Si on est déjà <= tip_limit, rien à faire.
        if tips.len() <= self.tip_limit {
            return Ok(());
        }

        // 4. SAFETY: Always keep at least 1 tip (most recent).
        //    Mirrors the same protection applied in ConcurrentDag::prune_oldest()
        //    (commit 9e2922f). Without this, an over-pruned tips CF causes
        //    top_tips() to return empty, silently blocking fee distribution.
        let keep = self.tip_limit.max(1);

        // 5. Supprime les tips excédentaires (les plus anciennes).
        let to_remove: Vec<_> = tips.into_iter().skip(keep).collect();
        if !to_remove.is_empty() {
            tracing::debug!(
                kept = keep,
                removed = to_remove.len(),
                "trim_tips: pruning excess tips from RocksDB"
            );
            let mut batch = rocksdb::WriteBatch::default();
            for (id, _) in &to_remove {
                batch.delete_cf(&cf_tips, id.as_bytes());
            }
            self.db.write(batch)?;
            // Update estimate after trimming
            self.tip_count_estimate
                .store(keep, std::sync::atomic::Ordering::Relaxed);
        }

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
