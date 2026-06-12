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
    /// Removes blocks from the front of the insertion order (oldest first),
    /// but **protects tips** (audit S9). A tip — a block with no children — is
    /// the active frontier of a branch; pruning it just because it is "old" by
    /// insertion order (e.g. a concurrent agent that finished its chain early)
    /// amputates a legitimate branch head from the RAM DAG. So tips are skipped
    /// (pushed to the back of the deque) as long as the **non-tip** blocks
    /// suffice to reach the bound — the normal case, since the frontier is a
    /// small fraction of the DAG and history (non-tips) lives on disk anyway.
    ///
    /// **RAM is still bounded.** The number of blocks removed is unchanged
    /// (`current_len - max_blocks`); only their *identity* shifts toward old
    /// non-tip history instead of recent tips. When non-tip blocks are
    /// insufficient (a flood of orphaned tips from dead agents — the historical
    /// source of unbounded growth), the **oldest tips** are pruned to make up
    /// the deficit, always keeping **≥ 1** tip so fee distribution and parent
    /// selection never see an empty tip set. Mirrors the RocksDB layer, where
    /// `trim_tips` keeps the newest `tip_limit` (256) real tips.
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

        // O(1) tip count via DashSet instead of O(n) children_count scan.
        let live_tips_start: usize = self.tips.len();
        let non_tip_available = current_len.saturating_sub(live_tips_start);

        // Tip-protection budget : on PRÉSERVE tous les tips tant que les blocs
        // NON-tip suffisent à atteindre la borne. On n'élague des tips QUE si
        // les non-tips sont insuffisants (flood de tips orphelins) — les plus
        // anciens d'abord, en gardant toujours ≥ 1 tip. `budget == 0` ⇒ aucun
        // tip n'est supprimé (cas nominal : la frontière est petite).
        let mut tip_removal_budget = to_remove
            .saturating_sub(non_tip_available)
            .min(live_tips_start.saturating_sub(1));

        // Phase 1: Collect IDs to remove while holding the lock.
        // The actual DashMap removals happen outside the lock to minimize
        // contention with concurrent insert_block() calls.
        let mut ids_to_remove: Vec<BlockId> = Vec::with_capacity(to_remove);
        {
            let mut order = self.insertion_order.lock();

            // Safety cap: never iterate more than the deque length to avoid infinite loops
            // when protected tips are repeatedly pushed back (none removable).
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

                // Protect tips: push back (skip) unless the tip-removal budget
                // is non-zero (non-tip blocks were insufficient). Oldest tips
                // are consumed first; ≥ 1 tip always survives (budget capped at
                // live_tips_start - 1).
                if self.tips.contains(&old_id) {
                    if tip_removal_budget == 0 {
                        order.push_back(old_id);
                        tip_skips += 1;
                        continue;
                    }
                    tip_removal_budget -= 1;
                }

                ids_to_remove.push(old_id);
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
            tracing::debug!(
                tip_skips,
                live_tips_start,
                "DAG prune_oldest: protected tip(s) from removal (non-tip blocks sufficed)"
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
