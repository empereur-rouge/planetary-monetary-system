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

    /// FIFO order for bounded spent_outpoints pruning.
    spent_order: Mutex<VecDeque<(String, u32)>>,

    /// Maximum spent outpoints to keep in RAM. 0 = unlimited.
    max_spent_outpoints: usize,

    /// Live tips (blocks with children_count == 0). Maintained incrementally
    /// by `insert_block` / `bootstrap_insert` / `prune_oldest` to avoid O(n)
    /// full scans in `find_tips()`.
    tips: DashSet<BlockId>,
}

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
        }
    }

    /// Create a new ConcurrentDag with a genesis block
    pub fn new_with_genesis(genesis: Block) -> Self {
        let dag = Self::new();
        dag.children_count
            .insert(genesis.id.clone(), AtomicU64::new(0));
        dag.tips.insert(genesis.id.clone());
        dag.blocks.insert(genesis.id.clone(), genesis.clone());
        match dag.insertion_order.lock() {
            Ok(mut order) => order.push_back(genesis.id),
            Err(poisoned) => poisoned.into_inner().push_back(genesis.id),
        }
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
    fn cleanup_ghost_entries(&self) {
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
    fn prune_oldest(&self) {
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
            match self.finality.write() {
                Ok(mut f) => {
                    for old_id in &ids_to_remove {
                        f.finalized.remove(old_id);
                    }
                }
                Err(poisoned) => {
                    let mut f = poisoned.into_inner();
                    for old_id in &ids_to_remove {
                        f.finalized.remove(old_id);
                    }
                }
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

    /// Mark an outpoint as spent (bounded FIFO eviction when limit > 0)
    pub fn mark_spent(&self, txid: &str, index: u32) {
        let key = (txid.to_string(), index);
        if self.spent_outpoints.insert(key.clone()) {
            if self.max_spent_outpoints > 0 {
                let mut order = match self.spent_order.lock() {
                    Ok(o) => o,
                    Err(poisoned) => poisoned.into_inner(),
                };
                order.push_back(key);
                while order.len() > self.max_spent_outpoints {
                    if let Some(oldest) = order.pop_front() {
                        self.spent_outpoints.remove(&oldest);
                    }
                }
            }
        }
    }

    /// Check if an outpoint is already spent
    pub fn is_spent(&self, txid: &str, index: u32) -> bool {
        self.spent_outpoints.contains(&(txid.to_string(), index))
    }

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
        match self.finality.read() {
            Ok(f) => f.finalized.contains(block_id),
            Err(poisoned) => poisoned.into_inner().finalized.contains(block_id),
        }
    }

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
            let mut order = match dag.insertion_order.lock() {
                Ok(o) => o,
                Err(poisoned) => {
                    tracing::error!("insertion_order mutex poisoned during bootstrap — recovering");
                    poisoned.into_inner()
                }
            };
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
        assert!(dag.contains_block("b7"), "Latest tip should not be pruned");

        // Oldest blocks should be gone
        assert!(
            !dag.contains_block("genesis"),
            "Genesis (deeply buried) should be pruned"
        );
    }

    #[test]
    fn test_pruning_removes_oldest_tips_too() {
        // DAG with max 3 blocks
        let dag = ConcurrentDag::with_capacity(3);

        // Fan-out: genesis -> [t1, t2, t3, t4]
        dag.insert_block(make_block("genesis", vec![]));
        dag.insert_block(make_block("t1", vec!["genesis"]));
        dag.insert_block(make_block("t2", vec!["genesis"]));
        dag.insert_block(make_block("t3", vec!["genesis"]));
        dag.insert_block(make_block("t4", vec!["genesis"]));
        dag.prune_oldest();

        // 5 blocks, capacity 3 → remove 2 oldest (genesis, t1)
        // Remaining: t2, t3, t4
        assert_eq!(dag.len(), 3, "should prune to exactly capacity");
        assert!(
            !dag.contains_block("genesis"),
            "genesis (oldest) should be pruned"
        );
        assert!(
            !dag.contains_block("t1"),
            "t1 (2nd oldest) should be pruned"
        );

        // Latest tips survive (they're at the back of insertion_order)
        assert!(dag.contains_block("t4"), "t4 (latest) should survive");
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
            dag.insert_block(make_block(
                &format!("b{}", i),
                parents.iter().map(|s| s.as_str()).collect(),
            ));
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
            dag.insert_block(make_block(&format!("b{}", i), vec![&format!("b{}", i - 1)]));
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
        assert!(!dag.contains_block("b0"), "genesis should be long pruned");
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
        assert!(dag.len() <= 4, "should prune to ~3, got {}", dag.len());

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
        // Oldest branches (agents 0-9) get pruned entirely, including their tips.
        // Recent branches (agents 10-19) survive because they're at the back.
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
            dag.len() <= 510,
            "wide DAG should prune to ~500, got {}",
            dag.len()
        );

        // Recent branch tips (agents 10-19) survive — they're at the back
        for agent in 10..20 {
            let tip = format!("a{}_49", agent);
            assert!(
                dag.contains_block(&tip),
                "recent agent {} tip should survive",
                agent
            );
        }

        // Oldest backbone blocks are pruned
        assert!(!dag.contains_block("g"), "oldest backbone should be pruned");
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
            dag.insert_block(make_block(&format!("b{}", i), vec![&parent]));
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
            dag.insert_block(make_block(&format!("b{}", i), vec![&parent]));
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
        // len == max_blocks → nothing to prune
        assert_eq!(dag.len(), 1, "at capacity, nothing should be pruned");
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

    // ─── Production scenario: orphaned tips from concurrent agents ───

    #[test]
    fn test_pruning_with_massive_orphaned_tips() {
        // Reproduces the production bug: 97 concurrent agents all pick the
        // same tip as parent, creating 96 orphaned branches per tick.
        // After N ticks, ~96*N orphaned tips accumulate. The old tip-skipping
        // logic couldn't prune ANY of them (skip limit exceeded).
        let dag = ConcurrentDag::with_capacity(500);

        dag.insert_block(make_block("g", vec![]));

        let mut latest_chain = "g".to_string();

        // Simulate 20 ticks, each with 97 agents picking the same parent
        for tick in 0..20 {
            let parent = latest_chain.clone();

            // 97 agents all create a block with the same parent
            for agent in 0..97 {
                let id = format!("t{}_a{}", tick, agent);
                dag.insert_block(make_block(&id, vec![&parent]));
            }

            // Only agent 0's block becomes the next chain link
            latest_chain = format!("t{}_a0", tick);
        }

        // Total inserted: 1 (genesis) + 20*97 (agent blocks) = 1941
        // Of which: 20 chain blocks (have children) + 1920 orphaned tips + 1 genesis
        // Note: amortized pruning already ran at insert #1000, so len < 1941.

        dag.prune_oldest();

        // Must prune to ~500 despite ~1920 orphaned tips
        assert!(
            dag.len() <= 510,
            "must prune to ~500 even with massive orphaned tips, got {}",
            dag.len()
        );

        // Latest chain tip must survive
        assert!(
            dag.contains_block("t19_a0"),
            "latest chain tip must survive"
        );
    }

    #[test]
    fn test_pruning_continuous_with_orphaned_tips() {
        // Continuous operation with orphaned tips: simulates a running node
        // where pruning triggers periodically while orphaned tips accumulate.
        let dag = ConcurrentDag::with_capacity(200);

        dag.insert_block(make_block("g", vec![]));
        let mut chain_tip = "g".to_string();
        let mut _total_inserted = 1u64;

        for tick in 0..50 {
            let parent = chain_tip.clone();

            // 10 agents, each creating a block off the same parent
            for agent in 0..10 {
                let id = format!("t{}_a{}", tick, agent);
                dag.insert_block(make_block(&id, vec![&parent]));
                _total_inserted += 1;
            }
            chain_tip = format!("t{}_a0", tick);

            // Simulate amortized pruning every 10 ticks
            if tick % 10 == 9 {
                dag.prune_oldest();
            }
        }
        dag.prune_oldest();

        assert!(
            dag.len() <= 210,
            "continuous prune should keep ~200, got {}",
            dag.len()
        );
        assert!(
            dag.contains_block("t49_a0"),
            "latest chain tip must survive"
        );
    }

    // ─── Tip protection: pruning never leaves the DAG tipless ────────

    #[test]
    fn test_pruning_preserves_at_least_one_tip_all_orphans() {
        // Critical scenario: ALL blocks in the DAG are orphaned tips
        // (no block has children). This reproduces the 03:48 AM fee
        // distribution failure: pruning removed all tips, leaving
        // find_tips() empty and fee distribution silently blocked.
        let dag = ConcurrentDag::with_capacity(3);

        // Insert 6 orphaned tips (all children of a non-existent parent)
        dag.insert_block(make_block("t1", vec!["phantom"]));
        dag.insert_block(make_block("t2", vec!["phantom"]));
        dag.insert_block(make_block("t3", vec!["phantom"]));
        dag.insert_block(make_block("t4", vec!["phantom"]));
        dag.insert_block(make_block("t5", vec!["phantom"]));
        dag.insert_block(make_block("t6", vec!["phantom"]));
        // All 6 are tips (children_count == 0)

        dag.prune_oldest();

        // At least 1 tip must survive — the DAG must NEVER be tipless
        let tips = dag.find_tips();
        assert!(
            !tips.is_empty(),
            "DAG must never be tipless after pruning! \
             blocks.len()={}, tips={:?}",
            dag.len(),
            tips
        );
        // Capacity is 3, so we should have roughly 3 blocks
        assert!(
            dag.len() >= 1 && dag.len() <= 4,
            "DAG should be close to capacity (3), got {}",
            dag.len()
        );
    }

    #[test]
    fn test_pruning_preserves_last_tip_in_mixed_dag() {
        // Mix of non-tip blocks and exactly 1 tip. Pruning must
        // protect the single tip even if it's the oldest block.
        let dag = ConcurrentDag::with_capacity(2);

        // Chain: g -> b1 -> b2 -> b3 (tip)
        dag.insert_block(make_block("g", vec![]));
        dag.insert_block(make_block("b1", vec!["g"]));
        dag.insert_block(make_block("b2", vec!["b1"]));
        dag.insert_block(make_block("b3", vec!["b2"]));
        // Only b3 is a tip (children_count == 0)

        dag.prune_oldest();

        // b3 must survive — it's the only tip
        assert!(
            dag.contains_block("b3"),
            "the only tip (b3) must survive pruning"
        );
        let tips = dag.find_tips();
        assert!(
            !tips.is_empty(),
            "find_tips() must not be empty after pruning"
        );
    }

    #[test]
    fn test_pruning_all_tips_capacity_one() {
        // Edge case: capacity=1 and multiple tips. Must keep exactly 1.
        let dag = ConcurrentDag::with_capacity(1);

        dag.insert_block(make_block("t1", vec![]));
        dag.insert_block(make_block("t2", vec![]));
        dag.insert_block(make_block("t3", vec![]));

        dag.prune_oldest();

        assert_eq!(dag.len(), 1, "capacity 1 should keep exactly 1 block");
        let tips = dag.find_tips();
        assert!(!tips.is_empty(), "the surviving block must be a tip");
    }

    // ─── Tips DashSet consistency tests ────────────────────────────────

    #[test]
    fn test_tips_set_consistency_chain() {
        // Chain: g -> b1 -> b2 -> b3
        // At each step, only the latest block should be a tip.
        let dag = ConcurrentDag::new();

        dag.insert_block(make_block("g", vec![]));
        let tips = dag.find_tips();
        println!("[tips_chain] after g: tips={:?}, tips_set_len={}", tips, dag.tips.len());
        assert_eq!(tips.len(), 1);
        assert!(tips.contains(&"g".to_string()));
        assert_eq!(dag.tips.len(), 1);

        dag.insert_block(make_block("b1", vec!["g"]));
        let tips = dag.find_tips();
        println!("[tips_chain] after b1: tips={:?}, tips_set_len={}", tips, dag.tips.len());
        assert_eq!(tips.len(), 1);
        assert!(tips.contains(&"b1".to_string()));
        assert!(!dag.tips.contains("g"), "g should no longer be a tip");

        dag.insert_block(make_block("b2", vec!["b1"]));
        let tips = dag.find_tips();
        println!("[tips_chain] after b2: tips={:?}, tips_set_len={}", tips, dag.tips.len());
        assert_eq!(tips.len(), 1);
        assert!(tips.contains(&"b2".to_string()));

        dag.insert_block(make_block("b3", vec!["b2"]));
        let tips = dag.find_tips();
        println!("[tips_chain] after b3: tips={:?}, tips_set_len={}", tips, dag.tips.len());
        assert_eq!(tips.len(), 1);
        assert!(tips.contains(&"b3".to_string()));
    }

    #[test]
    fn test_tips_set_fan_out_and_merge() {
        // Fan-out: g -> [t1, t2, t3, t4]
        // Then merge: m(t1, t2)
        // Tips should be: [t3, t4, m]
        let dag = ConcurrentDag::new();

        dag.insert_block(make_block("g", vec![]));
        dag.insert_block(make_block("t1", vec!["g"]));
        dag.insert_block(make_block("t2", vec!["g"]));
        dag.insert_block(make_block("t3", vec!["g"]));
        dag.insert_block(make_block("t4", vec!["g"]));

        let tips = dag.find_tips();
        println!("[tips_fanout] after fan-out: tips={:?}", tips);
        assert_eq!(tips.len(), 4, "4 branches = 4 tips");
        assert!(!dag.tips.contains("g"), "g has 4 children, not a tip");

        // Merge t1 + t2
        dag.insert_block(make_block("m", vec!["t1", "t2"]));
        let tips = dag.find_tips();
        println!("[tips_fanout] after merge: tips={:?}", tips);
        assert_eq!(tips.len(), 3, "merge consumed 2 tips, added 1 = 3 total");
        assert!(tips.contains(&"t3".to_string()));
        assert!(tips.contains(&"t4".to_string()));
        assert!(tips.contains(&"m".to_string()));
        assert!(!tips.contains(&"t1".to_string()), "t1 now has a child");
        assert!(!tips.contains(&"t2".to_string()), "t2 now has a child");
    }

    #[test]
    fn test_tips_set_after_pruning() {
        // With capacity=5, insert 10 blocks (chain).
        // After pruning, tips should be consistent with blocks in DAG.
        let dag = ConcurrentDag::with_capacity(5);

        let mut parent = "g".to_string();
        dag.insert_block(make_block("g", vec![]));
        for i in 1..10 {
            let id = format!("b{}", i);
            dag.insert_block(make_block(&id, vec![&parent]));
            parent = id;
        }

        // Force pruning
        dag.prune_oldest();

        let tips = dag.find_tips();
        println!(
            "[tips_prune] dag_len={}, tips={:?}, tips_set_len={}",
            dag.len(), tips, dag.tips.len()
        );

        // Every tip must exist in the DAG
        for tip in &tips {
            assert!(
                dag.contains_block(tip),
                "tip {} not in DAG after pruning",
                tip
            );
        }

        // Every block in tips DashSet must be in the DAG
        for entry in dag.tips.iter() {
            assert!(
                dag.blocks.contains_key(entry.key()),
                "tips set contains {} which is not in blocks",
                entry.key()
            );
        }

        // At least 1 tip must exist
        assert!(!tips.is_empty(), "at least 1 tip must survive pruning");
    }

    #[test]
    fn test_tips_set_matches_children_count() {
        // Verify that the tips DashSet exactly matches children_count == 0
        // for all blocks in the DAG. This validates incremental maintenance.
        let dag = ConcurrentDag::new();

        // Build a complex topology
        dag.insert_block(make_block("g", vec![]));
        dag.insert_block(make_block("a", vec!["g"]));
        dag.insert_block(make_block("b", vec!["g"]));
        dag.insert_block(make_block("c", vec!["a", "b"]));
        dag.insert_block(make_block("d", vec!["a"]));
        dag.insert_block(make_block("e", vec!["c"]));

        // Compute expected tips from children_count (ground truth)
        let expected_tips: Vec<String> = dag
            .children_count
            .iter()
            .filter(|e| {
                dag.blocks.contains_key(e.key())
                    && e.value().load(Ordering::Relaxed) == 0
            })
            .map(|e| e.key().clone())
            .collect();

        let actual_tips: Vec<String> = dag.tips.iter().map(|r| r.key().clone()).collect();

        println!(
            "[tips_match] expected={:?}, actual={:?}",
            expected_tips, actual_tips
        );

        let mut expected_sorted = expected_tips.clone();
        expected_sorted.sort();
        let mut actual_sorted = actual_tips.clone();
        actual_sorted.sort();

        assert_eq!(
            expected_sorted, actual_sorted,
            "tips DashSet must exactly match children_count==0 blocks"
        );
    }
}
