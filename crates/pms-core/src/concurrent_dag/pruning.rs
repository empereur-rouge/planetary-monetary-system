//! DAG pruning and ghost entry cleanup for ConcurrentDag.

use super::ConcurrentDag;
use pms_types::BlockId;

impl ConcurrentDag {
    /// Remove ghost entries from `children_count` and `children_idx`.
    ///
    /// Ghost entries are parent IDs that have tracking data but no
    /// corresponding block in the `blocks` DashMap.  They accumulate during
    /// selective bootstrap when loaded blocks reference parents outside the
    /// loaded window (e.g. with 50K blocks loaded from 13M, ~99% of parents
    /// under lexicographic loading, ~2-5% under chronological loading).
    ///
    /// Without cleanup, these ghost entries persist forever and consume
    /// hundreds of MB in the DashMaps.
    pub(super) fn cleanup_ghost_entries(&self) {
        let ghosts: Vec<BlockId> = self
            .children_count
            .iter()
            .filter(|entry| !self.blocks.contains_key(entry.key()))
            .map(|entry| entry.key().clone())
            .collect();

        let ghost_count = ghosts.len();
        for ghost_id in &ghosts {
            self.children_count.remove(ghost_id);
            self.children_idx.remove(ghost_id);
            self.tips.remove(ghost_id);
        }

        if ghost_count > 0 {
            tracing::info!(
                ghost_count,
                children_count_len = self.children_count.len(),
                children_idx_len = self.children_idx.len(),
                "DAG bootstrap: cleaned ghost entries (orphan parents outside loaded window)"
            );
        }
    }

    /// Prune oldest blocks to keep the DAG within `max_blocks`.
    ///
    /// Removes blocks from the front of the insertion order (oldest first).
    /// ALL blocks are eligible for pruning, including tips — old orphaned tips
    /// from concurrent agents are the primary source of unbounded growth.
    /// Recent tips at the back of the deque survive naturally because pruning
    /// stops once `blocks.len() <= max_blocks`.
    ///
    /// **Safety:** At least one tip is always preserved. If a candidate block
    /// is one of the last remaining tips, it is pushed back to the end of the
    /// deque instead of being removed. This guarantees that fee distribution,
    /// parent selection, and other tip-dependent operations never see an empty
    /// tip set.
    ///
    /// Does NOT touch `spent_outpoints` (needed for double-spend detection).
    pub(super) fn prune_oldest(&self) {
        if self.max_blocks == 0 {
            return;
        }

        let current_len = self.blocks.len();
        if current_len <= self.max_blocks {
            return;
        }

        let to_remove = current_len - self.max_blocks;
        let mut tip_skips = 0usize;

        // O(1) tip count via DashSet instead of O(n) children_count scan
        let mut live_tips: usize = self.tips.len();

        // Phase 1: Collect IDs to remove while holding the lock.
        // The actual DashMap removals happen outside the lock to minimize
        // contention with concurrent insert_block() calls.
        let mut ids_to_remove: Vec<BlockId> = Vec::with_capacity(to_remove);
        {
            let mut order = self.insertion_order.lock();

            // Safety cap: never iterate more than the deque length to avoid infinite loops
            // when all remaining blocks are tips.
            let max_iterations = order.len();
            let mut iterations = 0;

            while ids_to_remove.len() < to_remove && iterations < max_iterations {
                let Some(old_id) = order.pop_front() else {
                    break;
                };
                iterations += 1;

                // Skip ghost entries: blocks already removed by a previous prune cycle.
                if !self.blocks.contains_key(&old_id) {
                    continue;
                }

                // Protect the last tip: if this block is a tip and it's the only one
                // remaining, push it to the back of the deque and skip it.
                let is_tip = self.tips.contains(&old_id);

                if is_tip && live_tips <= 1 {
                    order.push_back(old_id);
                    tip_skips += 1;
                    continue;
                }

                ids_to_remove.push(old_id);

                if is_tip {
                    live_tips = live_tips.saturating_sub(1);
                }
            }
        } // Lock released here — insert_block() is unblocked

        // Phase 2: Remove from DashMaps (lock-free, concurrent-safe)
        for old_id in &ids_to_remove {
            self.blocks.remove(old_id);
            self.children_count.remove(old_id);
            self.children_idx.remove(old_id);
            self.tips.remove(old_id);
        }

        // Phase 3: Prune the finalized HashSet to prevent unbounded growth.
        // Remove entries for blocks no longer in the RAM DAG. This bounds
        // the finalized set to approximately max_blocks entries instead of
        // growing to millions over the node's lifetime (~340MB saved).
        if !ids_to_remove.is_empty() {
            let mut f = self.finality.write();
            for old_id in &ids_to_remove {
                f.finalized.remove(old_id);
            }
        }

        if tip_skips > 0 {
            tracing::warn!(
                tip_skips,
                live_tips,
                "DAG prune_oldest: protected last tip(s) from removal"
            );
        }

        tracing::info!(
            target = to_remove,
            removed = ids_to_remove.len(),
            remaining = self.blocks.len(),
            max_blocks = self.max_blocks,
            "DAG prune_oldest completed"
        );
    }
}
