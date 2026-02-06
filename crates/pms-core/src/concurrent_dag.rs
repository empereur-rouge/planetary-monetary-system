//! Concurrent DAG implementation using DashMap for lock-free access.
//!
//! This module provides a high-performance, thread-safe DAG that allows
//! concurrent block insertions without a global mutex. This is the IOTA-like
//! approach to achieving high TPS.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ ConcurrentDag                                               │
//! │                                                             │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │ blocks: DashMap<BlockId, Block>                     │   │
//! │  │   └─ 16 internal segments (sharded locks)           │   │
//! │  │   └─ Concurrent reads/writes with minimal contention│   │
//! │  └─────────────────────────────────────────────────────┘   │
//! │                                                             │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │ children_count: DashMap<BlockId, AtomicU64>         │   │
//! │  │   └─ Track how many children reference each block   │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! │                                                             │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │ spent_outpoints: DashSet<(String, u32)>             │   │
//! │  │   └─ Lock-free double-spend detection               │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use crate::block_builder::BlockMineBuilder;
use crate::finality::FinalityState;
use anyhow::Result;
use dashmap::{DashMap, DashSet};
use pms_types::{Block, BlockId, PayloadEnvelope};
use rand::prelude::IndexedRandom;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum number of tips to return from find_tips
pub const MAX_TIPS_CAP: usize = 64;

/// Children threshold for tip weighting
pub const TIP_CHILDREN_THRESHOLD: u64 = 4;

/// Concurrent DAG with lock-free block storage.
///
/// Uses DashMap (internally sharded HashMap) to allow concurrent
/// insertions without a global mutex. This is the key to achieving
/// high TPS in an IOTA-like architecture.
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
}

impl ConcurrentDag {
    /// Create a new empty ConcurrentDag
    pub fn new() -> Self {
        Self {
            blocks: DashMap::new(),
            children_count: DashMap::new(),
            children_idx: DashMap::new(),
            spent_outpoints: DashSet::new(),
            finality: RwLock::new(FinalityState::default()),
        }
    }

    /// Create a new ConcurrentDag with a genesis block
    pub fn new_with_genesis(genesis: Block) -> Self {
        let dag = Self::new();
        dag.children_count
            .insert(genesis.id.clone(), AtomicU64::new(0));
        dag.blocks.insert(genesis.id.clone(), genesis);
        dag
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

        // Initialize children count for new block
        self.children_count
            .insert(block_id.clone(), AtomicU64::new(0));

        // Insert the block itself
        self.blocks.insert(block_id, block);

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
        use std::collections::{HashSet, VecDeque};

        let mut seen: HashSet<String> = HashSet::new();
        let mut queue = VecDeque::new();

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

    /// Load the DAG from storage (RAM replay)
    pub async fn bootstrap_from_store<S>(store: &S) -> Result<Self>
    where
        S: pms_storage::DagStorage + Send + Sync,
    {
        let dag = Self::new();
        let ids = store.all_block_ids().await?;

        for id in ids {
            if let Some(sb) = store.get_block(&id).await? {
                // Parse payload from JSON if present
                let payload = if let Some(json) = &sb.payload_json {
                    match serde_json::from_str(json) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            tracing::error!(
                                "❌ Failed to parse payload for block {}: {}",
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
                dag.insert_block(block);
            }
        }
        Ok(dag)
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

    #[test]
    fn test_insert_and_get() {
        let dag = ConcurrentDag::new();

        let genesis = Block {
            id: "genesis".to_string(),
            parents: vec![],
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        };

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

        let genesis = Block {
            id: "genesis".to_string(),
            parents: vec![],
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        };
        dag.insert_block(genesis);

        let child = Block {
            id: "child1".to_string(),
            parents: vec!["genesis".to_string()],
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        };
        dag.insert_block(child);

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
}
