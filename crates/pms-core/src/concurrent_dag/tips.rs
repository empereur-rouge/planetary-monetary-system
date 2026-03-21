//! Tip management and traversal methods for ConcurrentDag.

use super::{ConcurrentDag, MAX_TIPS_CAP};
use pms_types::BlockId;
use rand::prelude::IndexedRandom;
use std::sync::atomic::Ordering;

impl ConcurrentDag {
    /// Find tips (blocks with no children).
    ///
    /// Uses the incrementally-maintained `tips` DashSet for O(tips) instead
    /// of the previous O(all_blocks) full scan of `children_count`.
    pub fn find_tips(&self) -> Vec<BlockId> {
        // Cap collection at MAX_TIPS_CAP to avoid allocating for all tips
        self.tips
            .iter()
            .take(MAX_TIPS_CAP)
            .map(|r| r.key().clone())
            .collect()
    }

    /// Select parents for a new block
    ///
    /// Randomly selects between min_parents and max_parents tips.
    pub fn select_parents(&self, min_parents: usize, max_parents: usize) -> Vec<BlockId> {
        // Bootstrapping: only genesis exists
        if self.blocks.len() == 1 {
            // Find the genesis (block with no parents)
            for entry in self.blocks.iter() {
                if entry.value().parents.is_empty() {
                    return vec![entry.key().clone()];
                }
            }
        }

        let tips = self.find_tips();

        if tips.is_empty() {
            // Fallback: pick any block
            if let Some(entry) = self.blocks.iter().next() {
                return vec![entry.key().clone()];
            }
            return vec![];
        }

        let mut rng = rand::rng();
        let count = min_parents.max(1).min(tips.len()).min(max_parents);

        tips.choose_multiple(&mut rng, count).cloned().collect()
    }

    /// Get children of a block
    pub fn get_children(&self, id: &str) -> Vec<BlockId> {
        self.children_idx
            .get(id)
            .map(|r| r.value().clone())
            .unwrap_or_default()
    }

    /// Get children count for a block
    pub fn get_children_count(&self, id: &str) -> u64 {
        self.children_count
            .get(id)
            .map(|r| r.value().load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Count all descendants (BFS) for k-depth finalization
    /// Returns the number of distinct descendant blocks
    pub fn count_descendants(&self, id: &str, max_count: usize) -> usize {
        use std::collections::{HashSet, VecDeque as Vdq};

        let mut seen: HashSet<String> = HashSet::with_capacity(max_count.min(256));
        let mut queue = Vdq::with_capacity(64);

        // Seed with direct children
        if let Some(children) = self.children_idx.get(id) {
            for child_id in children.value() {
                if seen.insert(child_id.clone()) {
                    queue.push_back(child_id.clone());
                }
            }
        }

        // BFS
        while let Some(x) = queue.pop_front() {
            if seen.len() >= max_count {
                return seen.len();
            }

            if let Some(children) = self.children_idx.get(&x) {
                for child_id in children.value() {
                    if seen.insert(child_id.clone()) {
                        queue.push_back(child_id.clone());
                    }
                }
            }
        }

        seen.len()
    }

    /// Collect ancestors of a block up to `max_depth` levels by walking parents.
    ///
    /// Used by incremental k-depth finalization: instead of scanning ALL blocks
    /// in the DAG, we only check ancestors of the newly inserted block (the ones
    /// that may have gained enough descendants to become final).
    ///
    /// Returns at most `max_depth` unique ancestor BlockIds.
    pub fn ancestors_within_depth(&self, id: &str, max_depth: usize) -> Vec<BlockId> {
        use std::collections::{HashSet, VecDeque};

        if max_depth == 0 {
            return vec![];
        }

        let mut seen = HashSet::new();
        let mut result = Vec::new();
        // Queue holds (block_id, depth_from_start)
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();

        // Seed: parents of the starting block
        if let Some(block) = self.blocks.get(id) {
            for parent_id in &block.parents {
                if seen.insert(parent_id.clone()) {
                    queue.push_back((parent_id.clone(), 1));
                }
            }
        }

        while let Some((ancestor_id, depth)) = queue.pop_front() {
            result.push(ancestor_id.clone());

            // Don't go deeper than max_depth
            if depth >= max_depth {
                continue;
            }

            // Walk further up: parents of this ancestor
            if let Some(block) = self.blocks.get(&ancestor_id) {
                for parent_id in &block.parents {
                    if seen.insert(parent_id.clone()) {
                        queue.push_back((parent_id.clone(), depth + 1));
                    }
                }
            }
        }

        result
    }
}
