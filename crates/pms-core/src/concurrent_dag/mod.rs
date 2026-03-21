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

mod bootstrap;
mod core;
mod finality;
mod forge;
mod pruning;
mod spent;
mod tips;

#[cfg(test)]
mod tests;

use crate::finality::FinalityState;
use dashmap::{DashMap, DashSet};
use pms_types::{Block, BlockId};
use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
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
    pub(super) insertion_order: Mutex<VecDeque<BlockId>>,

    /// Maximum blocks to keep in RAM. 0 = unlimited.
    pub(super) max_blocks: usize,

    /// Insert counter for amortized pruning checks.
    pub(super) insert_counter: AtomicU64,

    /// FIFO order for bounded spent_outpoints pruning.
    pub(super) spent_order: Mutex<VecDeque<(String, u32)>>,

    /// Maximum spent outpoints to keep in RAM. 0 = unlimited.
    pub(super) max_spent_outpoints: usize,

    /// Live tips (blocks with children_count == 0). Maintained incrementally
    /// by `insert_block` / `bootstrap_insert` / `prune_oldest` to avoid O(n)
    /// full scans in `find_tips()`.
    pub(super) tips: DashSet<BlockId>,
}

impl Default for ConcurrentDag {
    fn default() -> Self {
        Self::new()
    }
}
