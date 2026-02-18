//! Concurrent DAG implementation using DashMap for lock-free access.
//!
//! This module provides a high-performance, thread-safe DAG that allows
//! concurrent block insertions without a global mutex. This is the IOTA-like
//! approach to achieving high TPS.
//!
//! ## Architecture
//!
//! ```text
//! +-------------------------------------------------------------+
//! | ConcurrentDag                                               |
//! |                                                             |
//! |  +-----------------------------------------------------+   |
//! |  | blocks: DashMap<BlockId, Block>                     |   |
//! |  |   +- 16 internal segments (sharded locks)           |   |
//! |  |   +- Concurrent reads/writes with minimal contention|   |
//! |  +-----------------------------------------------------+   |
//! |                                                             |
//! |  +-----------------------------------------------------+   |
//! |  | children_count: DashMap<BlockId, AtomicU64>         |   |
//! |  |   +- Track how many children reference each block   |   |
//! |  +-----------------------------------------------------+   |
//! |                                                             |
//! |  +-----------------------------------------------------+   |
//! |  | spent_outpoints: DashSet<(String, u32)>             |   |
//! |  |   +- Lock-free double-spend detection               |   |
//! |  +-----------------------------------------------------+   |
//! +-------------------------------------------------------------+
//! ```

use crate::block_builder::BlockMineBuilder;
use crate::finality::FinalityState;
use anyhow::Result;
use dashmap::{DashMap, DashSet};
use pms_types::{Block, BlockId, PayloadEnvelope};
use rand::prelude::IndexedRandom;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

/// Maximum number of tips to return from find_tips
pub const MAX_TIPS_CAP: usize = 64;

/// Children threshold for tip weighting
pub const TIP_CHILDREN_THRESHOLD: u64 = 4;

/// How often to run pruning (every N inserts) to amortize the cost.
const PRUNE_CHECK_INTERVAL: u64 = 1000;

/// Concurrent DAG with lock-free block storage.
///
/// Uses DashMap (internally sharded HashMap) to allow concurrent
/// insertions without a global mutex. This is the key to achieving
/// high TPS in an IOTA-like architecture.
///
/// When `max_blocks > 0`, oldest non-tip blocks are pruned to bound
/// RAM usage. Pruned blocks remain in RocksDB for persistence.
pub struct ConcurrentDag {
    /// Block storage - DashMap provides internal sharding (16 segments by default)
    /// Each segment has its own RwLock, so 10 concurrent writers rarely collide.
    pub blocks: DashMap<BlockId, Block>,

    /// Children count per block (for tip selection weight)
    /// Using AtomicU64 for lock-free increment
    pub children_count: DashMap<BlockId, AtomicU64>,

    /// Children index for BFS traversal (used by finality)
    /// The Vec inside is only appended to, so we can use a simple DashMap
    pub children_idx: DashMap<BlockId, Vec<BlockId>>,

    /// Spent outpoints for double-spend detection
    /// DashSet is optimized for concurrent contains/insert
    pub spent_outpoints: DashSet<(String, u32)>,

    /// Finality state (updated in background, read-mostly)
    /// Using RwLock because writes are rare (batch updates)
    pub finality: RwLock<FinalityState>,

    /// Insertion order for pruning (FIFO). Protected by Mutex.
    insertion_order: Mutex<VecDeque<BlockId>>,

    /// Maximum blocks to keep in RAM. 0 = unlimited.
    max_blocks: usize,

    /// Insert counter for amortized pruning checks.
    insert_counter: AtomicU64,
}

impl ConcurrentDag {
    /// Create a new empty ConcurrentDag (unlimited, for tests/CLI).
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Create a new ConcurrentDag with a maximum in-memory block count.
    /// `max_blocks = 0` means unlimited (no pruning).
    pub fn with_capacity(max_blocks: usize) -> Self {
        Self {
            blocks: DashMap::new(),
            children_count: DashMap::new(),
            children_idx: DashMap::new(),
            spent_outpoints: DashSet::new(),
            finality: RwLock::new(FinalityState::default()),
            insertion_order: Mutex::new(VecDeque::new()),
            max_blocks,
            insert_counter: AtomicU64::new(0),
        }
    }

    /// Create a new ConcurrentDag with a genesis block
    pub fn new_with_genesis(genesis: Block) -> Self {
        let dag = Self::new();
        dag.children_count
            .insert(genesis.id.clone(), AtomicU64::new(0));
        dag.blocks.insert(genesis.id.clone(), genesis.clone());
        dag.insertion_order
            .lock()
            .unwrap()
            .push_back(genesis.id);
        dag
    }

    /// Insert a block during bootstrap (store replay).
    ///
    /// Builds the DAG structure (blocks, children_count, children_idx) without
    /// tracking insertion order or triggering amortized pruning. This avoids
    /// the false-tip pollution that occurs when intermediate prunes run during
    /// out-of-order loading from RocksDB (lexicographic = random order).
    ///
    /// After all blocks are loaded, the caller must populate `insertion_order`
    /// and call `prune_oldest()` exactly once.
    fn bootstrap_insert(&self, block: Block) {
        let block_id = block.id.clone();

        if self.blocks.contains_key(&block_id) {
            return;
        }

        for parent_id in &block.parents {
            self.children_count
                .entry(parent_id.clone())
                .or_insert_with(|| AtomicU64::new(0))
                .fetch_add(1, Ordering::Relaxed);

            self.children_idx
                .entry(parent_id.clone())
                .or_insert_with(Vec::new)
                .push(block_id.clone());
        }

        self.children_count
            .entry(block_id.clone())
            .or_insert_with(|| AtomicU64::new(0));

        self.blocks.insert(block_id, block);
    }

    /// Insert a block into the DAG (non-blocking)
    ///
    /// This is the core operation that must be fast for high TPS.
    /// Returns true if inserted, false if already exists.
    pub fn insert_block(&self, block: Block) -> bool {
        let block_id = block.id.clone();

        // Check if already exists (fast path)
        if self.blocks.contains_key(&block_id) {
            return false;
        }

        // Update children count for each parent
        for parent_id in &block.parents {
            // Increment parent's children count
            self.children_count
                .entry(parent_id.clone())
                .or_insert_with(|| AtomicU64::new(0))
                .fetch_add(1, Ordering::Relaxed);

            // Add to children index for BFS traversal
            self.children_idx
                .entry(parent_id.clone())
                .or_insert_with(Vec::new)
                .push(block_id.clone());
        }

        // Initialize children count for new block (only if not already tracked).
        // During bootstrap, children may be loaded before their parent, so the
        // parent's count is already >0 — we must NOT overwrite it.
        self.children_count
            .entry(block_id.clone())
            .or_insert_with(|| AtomicU64::new(0));

        // Insert the block itself
        self.blocks.insert(block_id.clone(), block);

        // Track insertion order for pruning
        match self.insertion_order.lock() {
            Ok(mut order) => order.push_back(block_id),
            Err(poisoned) => poisoned.into_inner().push_back(block_id),
        }

        // Amortized pruning: check every PRUNE_CHECK_INTERVAL inserts
        let count = self.insert_counter.fetch_add(1, Ordering::Relaxed);
        if self.max_blocks > 0 && count % PRUNE_CHECK_INTERVAL == 0 {
            self.prune_oldest();
        }

        true
    }

    /// Prune oldest non-tip blocks to keep the DAG within `max_blocks`.
    ///
    /// Removes blocks from the front of the insertion order (oldest first),
    /// skipping current tips (blocks with 0 children) to preserve tip selection.
    /// Does NOT touch `spent_outpoints` (needed for double-spend detection).
    fn prune_oldest(&self) {
        if self.max_blocks == 0 {
            return;
        }

        let current_len = self.blocks.len();
        if current_len <= self.max_blocks {
            return;
        }

        let to_remove = current_len - self.max_blocks;
        let mut removed = 0;
        let mut skipped_tips: Vec<BlockId> = Vec::new();

        let mut order = match self.insertion_order.lock() {
            Ok(o) => o,
            Err(poisoned) => {
                tracing::error!(
                    "insertion_order mutex POISONED — pruning disabled! \
                     Recovering with into_inner()"
                );
                poisoned.into_inner()
            }
        };

        while removed < to_remove {
            let Some(old_id) = order.pop_front() else {
                break;
            };

            // Skip ghost entries: blocks already removed by a previous prune cycle.
            // Without this, ghosts accumulate and count toward the skipped-tips limit,
            // causing pruning to abort before doing meaningful work.
            if !self.blocks.contains_key(&old_id) {
                continue;
            }

            // Don't prune tips - they're needed for parent selection
            let is_tip = self
                .children_count
                .get(&old_id)
                .map(|c| c.load(Ordering::Relaxed) == 0)
                .unwrap_or(false); // false: if no entry, block was partially cleaned → safe to prune

            if is_tip {
                skipped_tips.push(old_id);
                // Stop if we've skipped too many to avoid infinite churn
                if skipped_tips.len() > self.max_blocks / 10 {
                    break;
                }
                continue;
            }

            // Remove from blocks, children_count, children_idx
            self.blocks.remove(&old_id);
            self.children_count.remove(&old_id);
            self.children_idx.remove(&old_id);
            removed += 1;
        }

        // Re-enqueue skipped tips at the front so they get checked again later
        for tip_id in skipped_tips.into_iter().rev() {
            order.push_front(tip_id);
        }

        tracing::info!(
            target = to_remove,
            removed,
            remaining = self.blocks.len(),
            deque_remaining = order.len(),
            max_blocks = self.max_blocks,
            "DAG prune_oldest completed"
        );
    }

    /// Check if a block exists
    #[inline]
    pub fn contains_block(&self, id: &str) -> bool {
        self.blocks.contains_key(id)
    }

    /// Get a block by ID (clones the block)
    pub fn get_block(&self, id: &str) -> Option<Block> {
        self.blocks.get(id).map(|r| r.value().clone())
    }

    /// Get the number of blocks in the DAG
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Check if the DAG is empty
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Mark an outpoint as spent
    pub fn mark_spent(&self, txid: &str, index: u32) {
        self.spent_outpoints.insert((txid.to_string(), index));
    }

    /// Check if an outpoint is already spent
    pub fn is_spent(&self, txid: &str, index: u32) -> bool {
        self.spent_outpoints.contains(&(txid.to_string(), index))
    }

    /// Find tips (blocks with no children or few children)
    ///
    /// A tip is a block that hasn't been referenced by many other blocks.
    /// We use children_count to determine this efficiently.
    pub fn find_tips(&self) -> Vec<BlockId> {
        let mut tips = Vec::new();

        for entry in self.children_count.iter() {
            let id = entry.key();
            let count = entry.value().load(Ordering::Relaxed);

            // A tip has 0 children (or few children for weighted selection)
            if count == 0 {
                tips.push(id.clone());
            }
        }

        // Cap the number of tips
        if tips.len() > MAX_TIPS_CAP {
            tips.truncate(MAX_TIPS_CAP);
        }

        tips
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

        let mut seen: HashSet<String> = HashSet::new();
        let mut queue = Vdq::new();

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

    /// Forge a block locally (select parents, build, insert)
    /// This replaces the legacy `Dag::add_payload_auto_parents_mined`
    pub fn forge_block(
        &self,
        payload: Option<PayloadEnvelope>,
        difficulty_leading_zeros: u8,
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    ) -> Result<Block> {
        // 1. Select parents (2 min, 2 max)
        let parents = self.select_parents(2, 2);

        // Bootstrapping: if we have blocks but select_parents returns nothing,
        // it means something is wrong unless we only have genesis.
        if parents.is_empty() && self.blocks.len() > 1 {
            return Err(anyhow::anyhow!("No parents available"));
        }

        // 2. Build block
        // Note: BlockMineBuilder uses a callback to check parent existence
        let block =
            BlockMineBuilder::new(parents, payload, compute_id, |id| self.contains_block(id))
                .difficulty(difficulty_leading_zeros)
                .canonicalize_parents(true)
                .build();

        // 3. Insert
        self.insert_block(block.clone());

        // 4. Update finality?
        // Skipped for now in ConcurrentDag as it uses background updates.

        Ok(block)
    }

    /// Check if a block is final (helper)
    pub fn is_final(&self, block_id: &str) -> bool {
        self.finality.read().unwrap().finalized.contains(block_id)
    }

    /// Load the DAG from storage (RAM replay), with optional capacity limit.
    ///
    /// Uses a two-phase approach to avoid the false-tip problem:
    /// 1. Load ALL blocks without pruning (children_counts are built correctly
    ///    regardless of load order).
    /// 2. Populate `insertion_order` from the loaded blocks, then prune once
    ///    with full knowledge of the DAG structure.
    pub async fn bootstrap_from_store_with_capacity<S>(
        store: &S,
        max_blocks: usize,
    ) -> Result<Self>
    where
        S: pms_storage::DagStorage + Send + Sync,
    {
        let dag = Self::with_capacity(max_blocks);
        let ids = store.all_block_ids().await?;

        // Phase 1: Load all blocks WITHOUT pruning or insertion-order tracking.
        // This ensures children_counts are fully correct before any pruning.
        for id in ids {
            if let Some(sb) = store.get_block(&id).await? {
                let payload = if let Some(json) = &sb.payload_json {
                    match serde_json::from_str(json) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            tracing::error!(
                                "Failed to parse payload for block {}: {}",
                                sb.id,
                                e
                            );
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
        }

        // Phase 2: Build insertion_order from loaded blocks.
        // Order doesn't matter for the initial prune (any non-tip block can go),
        // but after pruning, new runtime blocks are appended chronologically.
        {
            let mut order = dag.insertion_order.lock().unwrap();
            for entry in dag.blocks.iter() {
                order.push_back(entry.key().clone());
            }
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
        Self::bootstrap_from_store_with_capacity(store, 0).await
    }
}

impl Default for ConcurrentDag {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_block(id: &str, parents: Vec<&str>) -> Block {
        Block {
            id: id.to_string(),
            parents: parents.into_iter().map(|s| s.to_string()).collect(),
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        }
    }

    #[test]
    fn test_insert_and_get() {
        let dag = ConcurrentDag::new();

        let genesis = make_block("genesis", vec![]);

        assert!(dag.insert_block(genesis.clone()));
        assert!(!dag.insert_block(genesis.clone())); // Duplicate

        assert!(dag.contains_block("genesis"));
        assert!(!dag.contains_block("nonexistent"));

        let retrieved = dag.get_block("genesis").unwrap();
        assert_eq!(retrieved.id, "genesis");
    }

    #[test]
    fn test_children_tracking() {
        let dag = ConcurrentDag::new();

        dag.insert_block(make_block("genesis", vec![]));
        dag.insert_block(make_block("child1", vec!["genesis"]));

        assert_eq!(dag.get_children_count("genesis"), 1);
        assert_eq!(dag.get_children("genesis"), vec!["child1".to_string()]);
    }

    #[test]
    fn test_spent_outpoints() {
        let dag = ConcurrentDag::new();

        assert!(!dag.is_spent("tx1", 0));

        dag.mark_spent("tx1", 0);
        assert!(dag.is_spent("tx1", 0));
        assert!(!dag.is_spent("tx1", 1));
    }

    #[test]
    fn test_pruning_respects_capacity() {
        // DAG with max 5 blocks
        let dag = ConcurrentDag::with_capacity(5);

        // Build a chain: genesis -> b1 -> b2 -> b3 -> b4 -> b5 -> b6 -> b7
        dag.insert_block(make_block("genesis", vec![]));
        dag.insert_block(make_block("b1", vec!["genesis"]));
        dag.insert_block(make_block("b2", vec!["b1"]));
        dag.insert_block(make_block("b3", vec!["b2"]));
        dag.insert_block(make_block("b4", vec!["b3"]));
        // At this point: 5 blocks, at capacity

        dag.insert_block(make_block("b5", vec!["b4"]));
        dag.insert_block(make_block("b6", vec!["b5"]));
        dag.insert_block(make_block("b7", vec!["b6"]));
        // Force prune (insert_block only auto-prunes every PRUNE_CHECK_INTERVAL)
        dag.prune_oldest();

        // Should have pruned down to ~5 blocks
        assert!(
            dag.len() <= 6,
            "DAG should be pruned to around max_blocks, got {}",
            dag.len()
        );

        // The tip (b7) must still exist
        assert!(
            dag.contains_block("b7"),
            "Latest tip should not be pruned"
        );

        // Oldest blocks should be gone
        assert!(
            !dag.contains_block("genesis"),
            "Genesis (deeply buried) should be pruned"
        );
    }

    #[test]
    fn test_pruning_preserves_tips() {
        // DAG with max 3 blocks
        let dag = ConcurrentDag::with_capacity(3);

        // Fan-out: genesis -> [t1, t2, t3, t4]
        dag.insert_block(make_block("genesis", vec![]));
        dag.insert_block(make_block("t1", vec!["genesis"]));
        dag.insert_block(make_block("t2", vec!["genesis"]));
        dag.insert_block(make_block("t3", vec!["genesis"]));
        dag.insert_block(make_block("t4", vec!["genesis"]));
        dag.prune_oldest();

        // genesis has 4 children -> not a tip -> can be pruned
        // t1..t4 are all tips -> should be preserved
        // After pruning genesis, we have 4 blocks which is > max_blocks(3),
        // but all remaining are tips so pruning stops
        assert!(!dag.contains_block("genesis"), "genesis should be pruned (has children)");

        // All tips should be preserved
        for tip in &["t1", "t2", "t3", "t4"] {
            assert!(dag.contains_block(tip), "tip {} should be preserved", tip);
        }
    }

    #[test]
    fn test_children_count_preserved_on_out_of_order_insert() {
        // Verifies the fix for the bootstrap bug: children loaded before parents.
        // all_block_ids() returns lexicographic order (hash-based = random),
        // so a child can be loaded before its parent. insert_block must NOT
        // overwrite the parent's children_count.
        let dag = ConcurrentDag::new(); // unlimited, just checking counts

        // Load child FIRST, then parent
        dag.insert_block(make_block("child", vec!["parent"]));
        // At this point, parent's children_count == 1 (set by child)
        assert_eq!(
            dag.children_count
                .get("parent")
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0),
            1,
            "parent children_count should be 1 after child loaded"
        );

        // Now load parent - must NOT overwrite children_count to 0
        dag.insert_block(make_block("parent", vec!["genesis"]));
        assert_eq!(
            dag.children_count
                .get("parent")
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0),
            1,
            "parent children_count must be preserved (not overwritten to 0)"
        );
    }

    #[test]
    fn test_pruning_with_out_of_order_bootstrap() {
        // Simulates a real bootstrap: 20 blocks in a chain, loaded in shuffled
        // order (children before parents), then pruned to max_blocks=8.
        let dag = ConcurrentDag::with_capacity(8);

        // Chain: g -> b1 -> b2 -> ... -> b19
        // Load in "random" order to simulate lexicographic hash order
        let shuffled_order: Vec<usize> = vec![
            12, 5, 18, 1, 9, 15, 3, 7, 0, 11, 16, 6, 19, 2, 13, 8, 4, 17, 10, 14,
        ];

        for &i in &shuffled_order {
            let id = format!("b{}", i);
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(Block {
                id,
                parents,
                payload: None,
                nonce: 0,
                metadata: None,
                signer_pk: None,
                signature: None,
            });
        }

        assert_eq!(dag.len(), 20);

        // Force post-bootstrap prune
        dag.prune_oldest();

        // Must prune to ~8 blocks
        assert!(
            dag.len() <= 10,
            "DAG should be pruned to around 8, got {}",
            dag.len()
        );

        // Tip (b19) must survive
        assert!(dag.contains_block("b19"), "tip b19 should survive");
    }

    #[test]
    fn test_unlimited_capacity_no_pruning() {
        let dag = ConcurrentDag::new(); // unlimited

        for i in 0..100 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(Block {
                id: format!("b{}", i),
                parents,
                payload: None,
                nonce: 0,
                metadata: None,
                signer_pk: None,
                signature: None,
            });
        }
        dag.prune_oldest();

        assert_eq!(dag.len(), 100, "Unlimited DAG should keep all blocks");
    }

    // ─── Ghost entries ─────────────────────────────────────────────

    #[test]
    fn test_ghost_entries_do_not_block_pruning() {
        // After a first prune, removed block IDs become "ghosts" in
        // insertion_order. The next prune must skip them without counting
        // them as tips, so subsequent prune cycles keep working.
        let dag = ConcurrentDag::with_capacity(50);

        // Build a chain of 100 blocks
        for i in 0..100 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(make_block(&format!("b{}", i), parents.iter().map(|s| s.as_str()).collect()));
        }
        assert_eq!(dag.len(), 100);

        // First prune: removes ~50 oldest
        dag.prune_oldest();
        let after_first = dag.len();
        assert!(
            after_first <= 52,
            "first prune should reduce to ~50, got {}",
            after_first
        );

        // Insert 30 more blocks (continuing the chain)
        for i in 100..130 {
            dag.insert_block(make_block(
                &format!("b{}", i),
                vec![&format!("b{}", i - 1)],
            ));
        }

        // Second prune: must still work despite ghost entries from first prune
        dag.prune_oldest();
        let after_second = dag.len();
        assert!(
            after_second <= 55,
            "second prune should keep ~50, got {} (ghost entries blocking?)",
            after_second
        );

        // Tip must survive
        assert!(dag.contains_block("b129"), "tip b129 must survive");
    }

    // ─── Continuous operation (simulates a running node) ───────────

    #[test]
    fn test_continuous_insert_and_prune_cycles() {
        // Simulates a node continuously receiving blocks: capacity 30,
        // insert 200 blocks with a prune every 20 inserts.
        let dag = ConcurrentDag::with_capacity(30);

        for i in 0..200 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                parents.iter().map(|s| s.as_str()).collect(),
            ));

            // Prune every 20 inserts (simulating amortized pruning)
            if i > 0 && i % 20 == 0 {
                dag.prune_oldest();
            }
        }
        dag.prune_oldest();

        assert!(
            dag.len() <= 35,
            "after 200 inserts with periodic prunes, should be ~30, got {}",
            dag.len()
        );
        assert!(dag.contains_block("b199"), "latest tip must survive");
        assert!(
            !dag.contains_block("b0"),
            "genesis should be long pruned"
        );
    }

    // ─── Diamond / multi-parent topology ───────────────────────────

    #[test]
    fn test_pruning_diamond_dag() {
        //   g
        //  / \
        // a   b
        //  \ /
        //   c
        //   |
        //   d
        //   |
        //   e   (tip)
        let dag = ConcurrentDag::with_capacity(3);

        dag.insert_block(make_block("g", vec![]));
        dag.insert_block(make_block("a", vec!["g"]));
        dag.insert_block(make_block("b", vec!["g"]));
        dag.insert_block(make_block("c", vec!["a", "b"])); // diamond merge
        dag.insert_block(make_block("d", vec!["c"]));
        dag.insert_block(make_block("e", vec!["d"]));
        dag.prune_oldest();

        // e is the only tip, g/a/b should be prunable
        assert!(dag.contains_block("e"), "tip e must survive");
        assert!(
            dag.len() <= 4,
            "should prune to ~3, got {}",
            dag.len()
        );

        // Verify children_count consistency: for surviving blocks with parents
        // still in DAG, the parent's children_count should be > 0
        if dag.contains_block("d") {
            assert!(
                dag.get_children_count("d") > 0,
                "d has child e, count must be > 0"
            );
        }
    }

    // ─── Wide DAG with many concurrent tips ────────────────────────

    #[test]
    fn test_pruning_wide_dag_many_tips() {
        // Simulates ~20 concurrent agents, each creating a long branch.
        // Capacity 500 vs 20 tips → skip limit 50, plenty of headroom.
        // This mirrors the testnet topology (many agents, long histories).
        let dag = ConcurrentDag::with_capacity(500);

        // Shared backbone: g -> b1 -> b2
        dag.insert_block(make_block("g", vec![]));
        dag.insert_block(make_block("b1", vec!["g"]));
        dag.insert_block(make_block("b2", vec!["b1"]));

        // 20 agents each create 50 blocks branching off b2
        for agent in 0..20 {
            let first = format!("a{}_0", agent);
            dag.insert_block(make_block(&first, vec!["b2"]));
            for step in 1..50 {
                let id = format!("a{}_{}", agent, step);
                let parent = format!("a{}_{}", agent, step - 1);
                dag.insert_block(make_block(&id, vec![&parent]));
            }
        }
        // Total: 3 backbone + 1000 agent blocks = 1003

        dag.prune_oldest();

        assert!(
            dag.len() <= 530,
            "wide DAG should prune to ~500, got {}",
            dag.len()
        );

        // All 20 branch tips (a*_49) must survive
        for agent in 0..20 {
            let tip = format!("a{}_49", agent);
            assert!(
                dag.contains_block(&tip),
                "agent {} tip should survive",
                agent
            );
        }
    }

    // ─── Spent outpoints preserved after pruning ───────────────────

    #[test]
    fn test_spent_outpoints_preserved_after_pruning() {
        let dag = ConcurrentDag::with_capacity(5);

        dag.insert_block(make_block("g", vec![]));
        dag.mark_spent("tx_old", 0);
        dag.mark_spent("tx_old", 1);

        // Build a chain past capacity
        for i in 1..=10 {
            let parent = if i == 1 {
                "g".to_string()
            } else {
                format!("b{}", i - 1)
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                vec![&parent],
            ));
        }
        dag.prune_oldest();

        // g is pruned, but spent_outpoints must survive for double-spend detection
        assert!(!dag.contains_block("g"), "g should be pruned");
        assert!(
            dag.is_spent("tx_old", 0),
            "spent outpoint must survive pruning"
        );
        assert!(
            dag.is_spent("tx_old", 1),
            "spent outpoint must survive pruning"
        );
    }

    // ─── Amortized pruning via PRUNE_CHECK_INTERVAL ────────────────

    #[test]
    fn test_amortized_pruning_triggers_automatically() {
        // With PRUNE_CHECK_INTERVAL = 1000, after inserting 1050 blocks
        // into a DAG with capacity 100, pruning should have triggered
        // automatically at least once (at insert #1000).
        let dag = ConcurrentDag::with_capacity(100);

        for i in 0..1050 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                parents.iter().map(|s| s.as_str()).collect(),
            ));
        }

        // Amortized prune should have triggered at insert #1000.
        // At that point len was 1001, it should have pruned ~901 blocks.
        // Then 50 more inserts bring it to ~150.
        // We don't call prune_oldest() manually here — relying on amortized.
        assert!(
            dag.len() < 200,
            "amortized pruning should have kicked in, but len = {}",
            dag.len()
        );

        assert!(dag.contains_block("b1049"), "latest tip must survive");
    }

    // ─── Memory cleanup: children_count/children_idx ───────────────

    #[test]
    fn test_pruning_cleans_children_count_and_idx() {
        let dag = ConcurrentDag::with_capacity(5);

        // Chain: g -> b1 -> b2 -> b3 -> b4 -> b5 -> b6 -> b7
        dag.insert_block(make_block("g", vec![]));
        for i in 1..=7 {
            let parent = if i == 1 {
                "g".to_string()
            } else {
                format!("b{}", i - 1)
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                vec![&parent],
            ));
        }
        dag.prune_oldest();

        // Pruned blocks should have their children_count/children_idx removed
        for pruned_id in &["g", "b1"] {
            if !dag.contains_block(pruned_id) {
                assert!(
                    dag.children_count.get(*pruned_id).is_none(),
                    "{} pruned but children_count entry leaked",
                    pruned_id
                );
                assert!(
                    dag.children_idx.get(*pruned_id).is_none(),
                    "{} pruned but children_idx entry leaked",
                    pruned_id
                );
            }
        }
    }

    // ─── Large bootstrap with intermediate prune cycles ────────────

    #[test]
    fn test_large_bootstrap_with_intermediate_prunes() {
        // Simulates loading 2500 blocks from store (PRUNE_CHECK_INTERVAL=1000
        // triggers 2 intermediate prune cycles during load) with capacity 500.
        let dag = ConcurrentDag::with_capacity(500);

        // Load in chronological order (simplest case) — 2500 block chain
        for i in 0..2500 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                parents.iter().map(|s| s.as_str()).collect(),
            ));
        }

        // After the loop, amortized prune ran at inserts 0, 1000, 2000.
        // Final forced prune:
        dag.prune_oldest();

        assert!(
            dag.len() <= 510,
            "should prune to ~500 after bootstrap, got {}",
            dag.len()
        );
        assert!(dag.contains_block("b2499"), "tip must survive");
        assert!(!dag.contains_block("b0"), "old genesis should be gone");
    }

    // ─── Concurrent multi-threaded insert + prune ──────────────────

    #[test]
    fn test_concurrent_inserts_with_pruning() {
        use std::sync::Arc;
        use std::thread;

        let dag = Arc::new(ConcurrentDag::with_capacity(200));

        // Create a shared backbone
        dag.insert_block(make_block("g", vec![]));

        // 4 threads, each inserting 100 blocks
        let mut handles = vec![];
        for t in 0..4 {
            let dag = dag.clone();
            handles.push(thread::spawn(move || {
                let first = format!("t{}_0", t);
                dag.insert_block(make_block(&first, vec!["g"]));
                for i in 1..100 {
                    let id = format!("t{}_{}", t, i);
                    let parent = format!("t{}_{}", t, i - 1);
                    dag.insert_block(make_block(&id, vec![&parent]));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
        // Total: 1 + 400 = 401 blocks

        dag.prune_oldest();

        assert!(
            dag.len() <= 210,
            "concurrent DAG should prune to ~200, got {}",
            dag.len()
        );

        // All 4 branch tips must survive
        for t in 0..4 {
            assert!(
                dag.contains_block(&format!("t{}_99", t)),
                "thread {} tip must survive",
                t
            );
        }
    }

    // ─── Edge: prune on empty / single-block DAG ───────────────────

    #[test]
    fn test_prune_empty_dag() {
        let dag = ConcurrentDag::with_capacity(10);
        dag.prune_oldest(); // must not panic
        assert_eq!(dag.len(), 0);
    }

    #[test]
    fn test_prune_single_block_dag() {
        let dag = ConcurrentDag::with_capacity(1);
        dag.insert_block(make_block("g", vec![]));
        dag.prune_oldest();
        // g is a tip (0 children) — should be preserved
        assert_eq!(dag.len(), 1, "single block (tip) must be preserved");
    }

    // ─── Edge: capacity exactly at block count ─────────────────────

    #[test]
    fn test_prune_at_exact_capacity() {
        let dag = ConcurrentDag::with_capacity(5);
        for i in 0..5 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                parents.iter().map(|s| s.as_str()).collect(),
            ));
        }
        dag.prune_oldest();
        // Exactly at capacity — nothing to prune
        assert_eq!(dag.len(), 5, "at exact capacity, nothing should be pruned");
    }

    // ─── find_tips consistency after pruning ────────────────────────

    #[test]
    fn test_find_tips_consistent_after_pruning() {
        let dag = ConcurrentDag::with_capacity(10);

        // Build: g -> b1 -> b2 -> ... -> b19
        for i in 0..20 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                parents.iter().map(|s| s.as_str()).collect(),
            ));
        }
        dag.prune_oldest();

        let tips = dag.find_tips();

        // Every tip returned must actually exist in the DAG
        for tip in &tips {
            assert!(
                dag.contains_block(tip),
                "find_tips returned {} which is not in the DAG",
                tip
            );
        }

        // The real tip (b19) must be in the list
        assert!(
            tips.contains(&"b19".to_string()),
            "b19 should be a tip, tips = {:?}",
            tips
        );
    }

    // ─── Repeated prune on already-pruned DAG (idempotent) ─────────

    #[test]
    fn test_repeated_prune_is_idempotent() {
        let dag = ConcurrentDag::with_capacity(10);

        for i in 0..30 {
            let parents = if i == 0 {
                vec![]
            } else {
                vec![format!("b{}", i - 1)]
            };
            dag.insert_block(make_block(
                &format!("b{}", i),
                parents.iter().map(|s| s.as_str()).collect(),
            ));
        }
        dag.prune_oldest();
        let len_after_first = dag.len();

        // Pruning again without inserting anything should be a no-op
        dag.prune_oldest();
        assert_eq!(
            dag.len(),
            len_after_first,
            "second prune without new inserts must be a no-op"
        );

        dag.prune_oldest();
        assert_eq!(
            dag.len(),
            len_after_first,
            "third prune must still be a no-op"
        );
    }
}
