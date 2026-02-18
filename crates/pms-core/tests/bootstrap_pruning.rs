//! Integration tests for `bootstrap_from_store_with_capacity`.
//!
//! These tests exercise the REAL production code path:
//! `ConcurrentDag::bootstrap_from_store_with_capacity(store, max_blocks)`
//!
//! Unlike the unit tests in concurrent_dag.rs which call `insert_block()` directly
//! (chronological, in-memory), these tests use a mock `DagStorage` that returns
//! block IDs in **lexicographic** order (like RocksDB), forcing the bootstrap code
//! to handle out-of-order loading, two-phase bootstrap, and post-load pruning.

use anyhow::Result;
use async_trait::async_trait;
use pms_core::ConcurrentDag;
use pms_storage::{DagStorage, StoredBlock, UtxoDelta};
use pms_wire::WireBlock;
use std::collections::HashMap;
use std::sync::RwLock;

// ─── Mock DagStorage ─────────────────────────────────────────────────────────

/// In-memory DagStorage that mimics RocksDB behavior:
/// - `all_block_ids()` returns IDs sorted **lexicographically** (not chronologically)
/// - `get_block()` returns the stored block
///
/// This is critical because RocksDB stores keys in sorted order, so
/// `all_block_ids()` returns hash-like IDs in random-looking order relative
/// to the DAG's temporal structure.
struct MockStore {
    blocks: RwLock<HashMap<String, StoredBlock>>,
}

impl MockStore {
    fn new() -> Self {
        Self {
            blocks: RwLock::new(HashMap::new()),
        }
    }

    fn insert(&self, id: &str, parents: Vec<String>) {
        let sb = StoredBlock {
            id: id.to_string(),
            parents,
            payload_json: None,
            nonce: 0,
            network_id: "test".to_string(),
            protocol_version: 1,
            signer_pk_hex: String::new(),
            signature_hex: String::new(),
            metadata: None,
        };
        self.blocks.write().unwrap().insert(id.to_string(), sb);
    }
}

#[async_trait]
impl DagStorage for MockStore {
    async fn all_block_ids(&self) -> Result<Vec<String>> {
        let map = self.blocks.read().unwrap();
        let mut ids: Vec<String> = map.keys().cloned().collect();
        ids.sort(); // Lexicographic order — same as RocksDB
        Ok(ids)
    }

    async fn get_block(&self, id: &str) -> Result<Option<StoredBlock>> {
        Ok(self.blocks.read().unwrap().get(id).cloned())
    }

    // ── Unused methods (only all_block_ids + get_block are called by bootstrap) ──

    async fn put_block(&self, _b: &StoredBlock) -> Result<pms_storage::PutResult> {
        unimplemented!("not used by bootstrap")
    }
    async fn add_child_edge(&self, _parent: &str, _child: &str) -> Result<()> {
        unimplemented!()
    }
    async fn children_count(&self, _id: &str) -> Result<u64> {
        unimplemented!()
    }
    async fn add_tip(&self, _id: &str) -> Result<()> {
        unimplemented!()
    }
    async fn remove_tip(&self, _id: &str) -> Result<()> {
        unimplemented!()
    }
    async fn top_tips(&self, _limit: usize) -> Result<Vec<String>> {
        unimplemented!()
    }
    async fn block_count(&self) -> Result<u64> {
        unimplemented!()
    }
    async fn export_json(&self) -> Result<String> {
        unimplemented!()
    }
    async fn export_namespace(&self) -> Result<String> {
        unimplemented!()
    }
    async fn import_json(&self, _dump: &str) -> Result<()> {
        unimplemented!()
    }
    async fn append_block_atomic(&self, _b: &StoredBlock) -> Result<bool> {
        unimplemented!()
    }
    async fn load_final(&self) -> Result<Vec<String>> {
        unimplemented!()
    }
    async fn load_last_milestone(&self) -> Result<Option<String>> {
        unimplemented!()
    }
    async fn recent_ids(&self, _limit: usize) -> Result<Vec<String>> {
        unimplemented!()
    }
    async fn recent_ids_by_time(
        &self,
        _after_ts: Option<i64>,
        _after_id: Option<String>,
        _limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        unimplemented!()
    }
    async fn get_blocks_by_ids(&self, _ids: &[String]) -> Result<Vec<WireBlock>> {
        unimplemented!()
    }
    async fn persist_final(&self, _ids: &[String]) -> Result<()> {
        unimplemented!()
    }
    async fn persist_last_milestone(&self, _id: &str) -> Result<()> {
        unimplemented!()
    }
    async fn append_block_atomic_with_utxo(
        &self,
        _b: &StoredBlock,
        _delta: Option<&UtxoDelta>,
    ) -> Result<bool> {
        unimplemented!()
    }
}

// ─── Helper: build a chain in the mock store ─────────────────────────────────

/// Builds a chain of `count` blocks: genesis -> b_1 -> b_2 -> ... -> b_{count-1}
/// Block IDs use zero-padded numbers to control lexicographic order.
fn build_chain(store: &MockStore, count: usize) {
    for i in 0..count {
        let id = format!("b_{:08}", i);
        let parents = if i == 0 {
            vec![] // genesis
        } else {
            vec![format!("b_{:08}", i - 1)]
        };
        store.insert(&id, parents);
    }
}

/// Builds a chain with hash-like IDs (SHA256-ish hex) to simulate real block IDs.
/// Returns the tip ID.
fn build_chain_with_hash_ids(store: &MockStore, count: usize) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let mut hasher = DefaultHasher::new();
        format!("block_{}", i).hash(&mut hasher);
        let hash = hasher.finish();
        // 16-char hex IDs — enough to be "random" lexicographically
        ids.push(format!("{:016x}", hash));
    }

    for i in 0..count {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![ids[i - 1].clone()]
        };
        store.insert(&ids[i], parents);
    }

    ids.last().unwrap().clone()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// Core test: bootstrap 5000 blocks from store, capacity 500.
/// This is the exact production scenario that was failing.
#[tokio::test]
async fn bootstrap_from_store_prunes_to_capacity() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 5000);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    assert!(
        dag.len() <= 510,
        "DAG should be pruned to ~500 after bootstrap, got {}",
        dag.len()
    );
    assert!(
        dag.len() >= 490,
        "DAG should have ~500 blocks, not fewer (got {})",
        dag.len()
    );

    // Tip (last block) must survive
    assert!(
        dag.contains_block("b_00004999"),
        "tip must survive pruning"
    );

    // Old blocks should be gone
    assert!(
        !dag.contains_block("b_00000000"),
        "genesis should be pruned"
    );

    Ok(())
}

/// Same test but with hash-like IDs (truly random lexicographic order).
/// This is the most realistic simulation of production RocksDB behavior
/// where block IDs are SHA256 hashes.
#[tokio::test]
async fn bootstrap_from_store_hash_ids_prunes_correctly() -> Result<()> {
    let store = MockStore::new();
    let tip_id = build_chain_with_hash_ids(&store, 5000);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    assert!(
        dag.len() <= 510,
        "DAG with hash IDs should prune to ~500, got {}",
        dag.len()
    );

    // The tip must survive
    assert!(
        dag.contains_block(&tip_id),
        "tip {} must survive pruning",
        tip_id
    );

    Ok(())
}

/// Large-scale test: 100K blocks, capacity 1000.
/// Simulates a ledger that accumulated many blocks before pruning was enabled.
#[tokio::test]
async fn bootstrap_from_store_large_scale_pruning() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 100_000);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 1000).await?;

    assert!(
        dag.len() <= 1010,
        "large DAG should prune to ~1000, got {}",
        dag.len()
    );

    // Tip must survive
    assert!(
        dag.contains_block("b_00099999"),
        "tip of 100K chain must survive"
    );

    Ok(())
}

/// Multi-branch DAG: backbone + 20 branches (like testnet with 20 agents).
/// All branch tips must survive pruning.
#[tokio::test]
async fn bootstrap_from_store_multi_branch_preserves_tips() -> Result<()> {
    let store = MockStore::new();

    // Backbone: g -> b1 -> b2
    store.insert("backbone_0", vec![]);
    store.insert("backbone_1", vec!["backbone_0".into()]);
    store.insert("backbone_2", vec!["backbone_1".into()]);

    // 20 agents, each with 200 blocks branching off backbone_2
    let mut tip_ids = Vec::new();
    for agent in 0..20u32 {
        let first_id = format!("agent{:02}_000", agent);
        store.insert(&first_id, vec!["backbone_2".into()]);
        for step in 1..200u32 {
            let id = format!("agent{:02}_{:03}", agent, step);
            let parent = format!("agent{:02}_{:03}", agent, step - 1);
            store.insert(&id, vec![parent]);
        }
        tip_ids.push(format!("agent{:02}_199", agent));
    }
    // Total: 3 + 4000 = 4003 blocks

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    assert!(
        dag.len() <= 530,
        "multi-branch DAG should prune to ~500, got {}",
        dag.len()
    );

    // ALL 20 tips must survive
    for tip in &tip_ids {
        assert!(
            dag.contains_block(tip),
            "branch tip {} must survive pruning",
            tip
        );
    }

    Ok(())
}

/// Unlimited capacity (max_blocks=0): no pruning, all blocks kept.
#[tokio::test]
async fn bootstrap_from_store_unlimited_keeps_all() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 1000);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 0).await?;

    assert_eq!(
        dag.len(),
        1000,
        "unlimited capacity should keep all blocks"
    );

    Ok(())
}

/// After bootstrap+prune, runtime `insert_block()` must still work and
/// subsequent prunes must continue to cap the DAG.
#[tokio::test]
async fn bootstrap_then_runtime_inserts_continue_pruning() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 2000);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    assert!(
        dag.len() <= 510,
        "initial bootstrap should prune to ~500, got {}",
        dag.len()
    );

    // Now simulate runtime: insert 1500 more blocks (triggers amortized prune at 1000)
    let last_store_id = "b_00001999".to_string();
    assert!(
        dag.contains_block(&last_store_id),
        "tip from bootstrap must exist for chaining"
    );

    let mut parent = last_store_id;
    for i in 2000..3500 {
        let id = format!("rt_{:08}", i);
        let block = pms_types::Block {
            id: id.clone(),
            parents: vec![parent.clone()],
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        };
        dag.insert_block(block);
        parent = id;
    }

    // Amortized prune triggers at insert #1000 — after 1500 runtime inserts,
    // at least one prune cycle has run. DAG should stay bounded.
    assert!(
        dag.len() <= 1100,
        "after runtime inserts, DAG should stay bounded, got {}",
        dag.len()
    );

    // Latest runtime tip must survive
    assert!(
        dag.contains_block("rt_00003499"),
        "latest runtime tip must survive"
    );

    Ok(())
}

/// Verify children_count consistency after bootstrap+prune.
/// No surviving block should have a stale or incorrect children_count.
#[tokio::test]
async fn bootstrap_prune_children_count_consistent() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 3000);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    // For each surviving block, verify children_count matches actual children in DAG
    for entry in dag.blocks.iter() {
        let block_id = entry.key();
        let count = dag
            .children_count
            .get(block_id.as_str())
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);

        // Count actual children still in DAG
        let actual_children_in_dag = dag
            .children_idx
            .get(block_id.as_str())
            .map(|children| {
                children
                    .value()
                    .iter()
                    .filter(|child_id| dag.blocks.contains_key(child_id.as_str()))
                    .count()
            })
            .unwrap_or(0);

        // children_count may be >= actual (includes pruned children),
        // but for the tip it must be 0
        if actual_children_in_dag == 0 && count == 0 {
            // This is a valid tip — OK
        } else if actual_children_in_dag > 0 {
            assert!(
                count > 0,
                "block {} has {} live children but children_count is 0",
                block_id,
                actual_children_in_dag
            );
        }
    }

    // The tip's children_count must be exactly 0
    assert_eq!(
        dag.children_count
            .get("b_00002999")
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(999),
        0,
        "tip must have children_count == 0"
    );

    Ok(())
}

/// find_tips() must only return blocks that actually exist in the DAG.
#[tokio::test]
async fn bootstrap_prune_find_tips_consistent() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 5000);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    let tips = dag.find_tips();
    for tip in &tips {
        assert!(
            dag.contains_block(tip),
            "find_tips returned '{}' which doesn't exist in DAG",
            tip
        );
    }

    // In a single chain, there should be exactly 1 tip
    assert_eq!(
        tips.len(),
        1,
        "single chain should have exactly 1 tip, got {}",
        tips.len()
    );
    assert_eq!(tips[0], "b_00004999", "tip should be the last block");

    Ok(())
}

/// Edge case: bootstrap with fewer blocks than capacity — no pruning needed.
#[tokio::test]
async fn bootstrap_under_capacity_no_pruning() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 100);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    assert_eq!(
        dag.len(),
        100,
        "under capacity, all blocks should be kept"
    );

    Ok(())
}

/// Edge case: bootstrap with exactly capacity blocks.
#[tokio::test]
async fn bootstrap_at_exact_capacity() -> Result<()> {
    let store = MockStore::new();
    build_chain(&store, 500);

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 500).await?;

    assert_eq!(
        dag.len(),
        500,
        "at exact capacity, no pruning should occur"
    );

    Ok(())
}

/// Diamond/merge topology: blocks with multiple parents.
#[tokio::test]
async fn bootstrap_prune_diamond_topology() -> Result<()> {
    let store = MockStore::new();

    //  g
    //  |
    //  a
    // / \
    // b   c
    // \ /
    //  d
    //  |
    // ... chain of 500 more blocks
    store.insert("g", vec![]);
    store.insert("a", vec!["g".into()]);
    store.insert("b", vec!["a".into()]);
    store.insert("c", vec!["a".into()]);
    store.insert("d", vec!["b".into(), "c".into()]);

    // Continue chain from d
    let mut prev = "d".to_string();
    for i in 0..500 {
        let id = format!("chain_{:04}", i);
        store.insert(&id, vec![prev]);
        prev = id;
    }
    // Total: 5 + 500 = 505 blocks

    let dag = ConcurrentDag::bootstrap_from_store_with_capacity(&store, 100).await?;

    assert!(
        dag.len() <= 110,
        "diamond DAG should prune to ~100, got {}",
        dag.len()
    );

    // Tip must survive
    assert!(
        dag.contains_block("chain_0499"),
        "tip must survive pruning"
    );

    Ok(())
}
