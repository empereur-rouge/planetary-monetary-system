//! Core construction and block insertion methods for ConcurrentDag.

use super::{ConcurrentDag, PRUNE_CHECK_INTERVAL};
use crate::finality::FinalityState;
use dashmap::{DashMap, DashSet};
use parking_lot::{Mutex, RwLock};
use pms_types::{Block, BlockId};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

impl ConcurrentDag {
    /// Create a new empty ConcurrentDag (unlimited, for tests/CLI).
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Create a new ConcurrentDag with a maximum in-memory block count.
    /// `max_blocks = 0` means unlimited (no pruning).
    pub fn with_capacity(max_blocks: usize) -> Self {
        Self::with_capacity_and_spent_limit(max_blocks, 0)
    }

    /// Create a new ConcurrentDag with bounded blocks and bounded spent outpoints.
    /// `max_spent_outpoints = 0` means unlimited.
    pub fn with_capacity_and_spent_limit(max_blocks: usize, max_spent_outpoints: usize) -> Self {
        Self {
            blocks: DashMap::new(),
            children_count: DashMap::new(),
            children_idx: DashMap::new(),
            spent_outpoints: DashSet::new(),
            finality: RwLock::new(FinalityState::default()),
            insertion_order: Mutex::new(VecDeque::new()),
            max_blocks,
            insert_counter: AtomicU64::new(0),
            spent_order: Mutex::new(VecDeque::new()),
            max_spent_outpoints,
            tips: DashSet::new(),
            consumed_bridge_locks: DashSet::new(),
        }
    }

    /// Create a new ConcurrentDag with a genesis block
    pub fn new_with_genesis(genesis: Block) -> Self {
        let dag = Self::new();
        dag.children_count
            .insert(genesis.id.clone(), AtomicU64::new(0));
        dag.tips.insert(genesis.id.clone());
        dag.blocks.insert(genesis.id.clone(), genesis.clone());
        dag.insertion_order.lock().push_back(genesis.id);
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
    pub(super) fn bootstrap_insert(&self, block: Block) {
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

            // Parent now has a child → no longer a tip
            self.tips.remove(parent_id);
        }

        self.children_count
            .entry(block_id.clone())
            .or_insert_with(|| AtomicU64::new(0));

        // New block starts as a tip (no children yet)
        self.tips.insert(block_id.clone());

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

        // Capture parents before block is moved into DashMap
        let parents: Vec<BlockId> = block.parents.clone();

        // Update children count for each parent
        for parent_id in &parents {
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

            // Parent now has a child → no longer a tip
            self.tips.remove(parent_id);
        }

        // Initialize children count for new block (only if not already tracked).
        // During bootstrap, children may be loaded before their parent, so the
        // parent's count is already >0 — we must NOT overwrite it.
        self.children_count
            .entry(block_id.clone())
            .or_insert_with(|| AtomicU64::new(0));

        // New block starts as a tip (no children yet)
        self.tips.insert(block_id.clone());

        // Insert the block itself
        self.blocks.insert(block_id.clone(), block);

        // Track insertion order for pruning
        self.insertion_order.lock().push_back(block_id);

        // Amortized pruning: check every PRUNE_CHECK_INTERVAL inserts
        let count = self.insert_counter.fetch_add(1, Ordering::Relaxed);
        if self.max_blocks > 0 && count % PRUNE_CHECK_INTERVAL == 0 {
            self.prune_oldest();
        }

        true
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
}
