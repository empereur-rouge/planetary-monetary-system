//! Storage bootstrap (DAG replay from RocksDB) for ConcurrentDag.

use super::ConcurrentDag;
use anyhow::Result;
use pms_types::Block;

impl ConcurrentDag {
    /// Load the DAG from storage (RAM replay), with optional capacity limit.
    ///
    /// Uses a multi-phase approach to avoid false-tip pollution and OOM:
    ///
    /// 1. **Chronological selective loading**: uses `newest_block_ids_by_time(n)`
    ///    which reads the `by_time` CF in reverse — loading the N most-recent
    ///    blocks by timestamp.  Consecutive blocks reference recent parents, so
    ///    the loaded set forms a mostly-connected subgraph (~2-5% orphan parents
    ///    vs ~99.8% with the old lexicographic approach).
    /// 2. **Ghost cleanup**: removes `children_count`/`children_idx` entries for
    ///    parents outside the loaded window to prevent unbounded DashMap growth.
    /// 3. **Insertion order**: uses the loading order directly (oldest-first from
    ///    the chronological iterator) so `prune_oldest()` evicts truly oldest
    ///    blocks first.
    /// 4. **Single prune** with fully-correct children_counts.
    pub async fn bootstrap_from_store_with_capacity<S>(
        store: &S,
        max_blocks: usize,
        max_spent_outpoints: usize,
    ) -> Result<Self>
    where
        S: pms_storage::DagStorage + Send + Sync,
    {
        let dag = Self::with_capacity_and_spent_limit(max_blocks, max_spent_outpoints);

        // ── Selective loading ──────────────────────────────────────────────
        // Prefer chronological loading via `by_time` CF over lexicographic
        // (`idx_blocks` CF).  Hash-based block IDs have no correlation with
        // time, so lexicographic "newest N" actually loads N random blocks
        // across the entire history — causing 99.8% orphan tips and massive
        // ghost entries (~800 MB on Eden with 13M blocks).
        //
        // Chronological loading preserves parent-child locality: the N most
        // recent blocks mostly reference each other as parents, yielding
        // only ~2-5% orphan tips at the boundary.
        let ids = if max_blocks > 0 {
            // O(1) approximate count for logging (avoids full-scan of 13M+ keys)
            let total_in_db = store.block_count_estimate().await.unwrap_or(0) as usize;

            // Try chronological loading first (by_time CF)
            let mut ids = store.newest_block_ids_by_time(max_blocks).await?;

            // Fallback: if by_time CF is empty (e.g. after import_json which
            // skips time indices), use lexicographic loading as last resort
            if ids.is_empty() && total_in_db > 0 {
                tracing::warn!(
                    total_in_db,
                    "by_time CF empty — falling back to lexicographic loading \
                     (expect high orphan tip count)"
                );
                ids = store.newest_block_ids(max_blocks).await?;
            }

            if total_in_db > ids.len() {
                tracing::info!(
                    total_in_db,
                    loading = ids.len(),
                    skipped = total_in_db - ids.len(),
                    "Selective bootstrap: loading newest blocks by timestamp \
                     (full history preserved in RocksDB)"
                );
            }
            ids
        } else {
            store.all_block_ids().await?
        };

        // Phase 1: Load blocks WITHOUT pruning or insertion-order tracking.
        // children_counts are built correctly regardless of load order.
        for (i, id) in ids.iter().enumerate() {
            if let Some(sb) = store.get_block(id).await? {
                let payload = if let Some(json) = &sb.payload_json {
                    match serde_json::from_str(json) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            tracing::error!("Failed to parse payload for block {}: {}", sb.id, e);
                            None
                        }
                    }
                } else {
                    None
                };

                let block = Block {
                    id: sb.id,
                    parents: sb.parents,
                    payload,
                    nonce: sb.nonce,
                    metadata: None,
                    signer_pk: Some(sb.signer_pk_hex).filter(|s| !s.is_empty()),
                    signature: Some(sb.signature_hex).filter(|s| !s.is_empty()),
                };
                dag.bootstrap_insert(block);
            }
            if (i + 1) % 10_000 == 0 {
                tracing::info!(loaded = i + 1, total = ids.len(), "DAG bootstrap progress...");
            }
        }

        // Phase 1.5: Clean ghost entries — orphan parent IDs that have tracking
        // data (children_count, children_idx) but no corresponding block.
        // With chronological loading this is ~2-5% of loaded blocks at the
        // boundary; with lexicographic fallback it can be ~99.8%.
        dag.cleanup_ghost_entries();

        // Phase 2: Build insertion_order from the loading order directly.
        // With chronological loading, `ids` is oldest-first — so the oldest
        // blocks are at the front of the deque and get pruned first (correct
        // temporal behaviour). Only include IDs that were actually loaded
        // (some get_block calls may have returned None).
        {
            let mut order = dag.insertion_order.lock();
            for id in &ids {
                if dag.blocks.contains_key(id) {
                    order.push_back(id.clone());
                }
            }
        }

        // Diagnostic: count tips and parentless blocks before pruning
        {
            let total = dag.blocks.len();
            let tips_count = dag.tips.len();
            let parentless = dag
                .blocks
                .iter()
                .filter(|e| e.value().parents.is_empty())
                .count();
            let orphan_parents = dag
                .blocks
                .iter()
                .filter(|e| {
                    e.value()
                        .parents
                        .iter()
                        .any(|p| !dag.blocks.contains_key(p))
                })
                .count();
            tracing::info!(
                total,
                tips_count,
                parentless,
                orphan_parents,
                "DAG bootstrap diagnostic (post-ghost-cleanup)"
            );
        }

        // Phase 3: Single prune with fully-correct children_counts.
        dag.prune_oldest();

        tracing::info!(
            loaded = dag.blocks.len(),
            max_blocks,
            "DAG bootstrap complete (post-prune)"
        );

        Ok(dag)
    }

    /// Load the DAG from storage (RAM replay) - unlimited capacity (for tests/CLI).
    pub async fn bootstrap_from_store<S>(store: &S) -> Result<Self>
    where
        S: pms_storage::DagStorage + Send + Sync,
    {
        Self::bootstrap_from_store_with_capacity(store, 0, 0).await
    }
}
