//! Non-regression test for audit finding C3.
//!
//! History: `ConcurrentDag::is_spent()` tracks spent outpoints in a bounded
//! FIFO (`max_spent_outpoints`). Once the cap is reached, the oldest entries
//! are evicted from RAM — but they remain in the RocksDB `utxo_spent` column
//! family, which is the source of truth. A validation path that relied on
//! `is_spent()` alone would silently report an evicted, truly-spent outpoint
//! as "not spent" and accept a double-spend.
//!
//! v0.7.2 contract (this test locks it in):
//! * `is_spent()` is documented as best-effort RAM and may return `false` for
//!   an actually-spent outpoint after FIFO eviction.
//! * `is_outpoint_spent_authoritative(&store, …)` consults the RAM tracker
//!   first and falls back to `DagStorage::is_outpoint_spent`, which reads
//!   the authoritative `utxo_spent` column family. This method MUST return
//!   `true` for any outpoint that was ever spent.

use anyhow::Result;
use async_trait::async_trait;
use pms_core::concurrent_dag::ConcurrentDag;
use pms_storage::store::PutResult;
use pms_storage::{DagStorage, StoredBlock, UtxoDelta};
use pms_wire::WireBlock;
use std::collections::HashSet;
use std::sync::Mutex;

/// Minimal `DagStorage` mock that persists spent outpoints in a `HashSet`,
/// mirroring the role of the RocksDB `utxo_spent` column family. Every
/// method the background persist task or the validation flow actually
/// exercises is implemented; unused methods panic via `todo!()`.
struct SpentTrackingStore {
    spent: Mutex<HashSet<(String, u32)>>,
}

impl SpentTrackingStore {
    fn new() -> Self {
        Self {
            spent: Mutex::new(HashSet::new()),
        }
    }

    fn record_spent(&self, txid: &str, index: u32) {
        self.spent
            .lock()
            .unwrap()
            .insert((txid.to_string(), index));
    }
}

#[async_trait]
impl DagStorage for SpentTrackingStore {
    async fn is_outpoint_spent(&self, txid: &str, index: u32) -> Result<bool> {
        Ok(self
            .spent
            .lock()
            .unwrap()
            .contains(&(txid.to_string(), index)))
    }

    // ---- Not exercised by this test: panic on accidental use. ----
    async fn put_block(&self, _b: &StoredBlock) -> Result<PutResult> {
        todo!()
    }
    async fn get_block(&self, _id: &str) -> Result<Option<StoredBlock>> {
        todo!()
    }
    async fn add_child_edge(&self, _parent: &str, _child: &str) -> Result<()> {
        todo!()
    }
    async fn children_count(&self, _id: &str) -> Result<u64> {
        todo!()
    }
    async fn add_tip(&self, _id: &str) -> Result<()> {
        todo!()
    }
    async fn remove_tip(&self, _id: &str) -> Result<()> {
        todo!()
    }
    async fn top_tips(&self, _limit: usize) -> Result<Vec<String>> {
        todo!()
    }
    async fn all_block_ids(&self) -> Result<Vec<String>> {
        todo!()
    }
    async fn block_count(&self) -> Result<u64> {
        todo!()
    }
    async fn export_json(&self) -> Result<String> {
        todo!()
    }
    async fn export_namespace(&self) -> Result<String> {
        todo!()
    }
    async fn import_json(&self, _dump: &str) -> Result<()> {
        todo!()
    }
    async fn append_block_atomic(&self, _b: &StoredBlock) -> Result<bool> {
        todo!()
    }
    async fn append_block_atomic_with_utxo(
        &self,
        _b: &StoredBlock,
        _delta: Option<&UtxoDelta>,
    ) -> Result<bool> {
        todo!()
    }
    async fn load_final(&self) -> Result<Vec<String>> {
        todo!()
    }
    async fn load_last_milestone(&self) -> Result<Option<String>> {
        todo!()
    }
    async fn recent_ids(&self, _limit: usize) -> Result<Vec<String>> {
        todo!()
    }
    async fn recent_ids_by_time(
        &self,
        _after_ts: Option<i64>,
        _after_id: Option<String>,
        _limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        todo!()
    }
    async fn get_blocks_by_ids(&self, _ids: &[String]) -> Result<Vec<WireBlock>> {
        todo!()
    }
    async fn persist_final(&self, _ids: &[String]) -> Result<()> {
        todo!()
    }
    async fn persist_last_milestone(&self, _id: &str) -> Result<()> {
        todo!()
    }
}

#[tokio::test]
async fn authoritative_check_detects_fifo_evicted_outpoint() {
    // Tiny FIFO: only 2 outpoints survive in RAM at any moment.
    let dag = ConcurrentDag::with_capacity_and_spent_limit(10, 2);
    let store = SpentTrackingStore::new();

    // The validation flow marks spent in both layers; the RAM FIFO evicts
    // the oldest entry once we spend the third outpoint.
    for (txid, idx) in &[("tx_old", 0u32), ("tx_mid", 0u32), ("tx_new", 0u32)] {
        dag.mark_spent(txid, *idx);
        store.record_spent(txid, *idx);
    }

    let old_is_spent_ram = dag.is_spent("tx_old", 0);
    let mid_is_spent_ram = dag.is_spent("tx_mid", 0);
    let new_is_spent_ram = dag.is_spent("tx_new", 0);

    let old_is_spent_auth = dag
        .is_outpoint_spent_authoritative(&store, "tx_old", 0)
        .await
        .expect("authoritative call must succeed");
    let mid_is_spent_auth = dag
        .is_outpoint_spent_authoritative(&store, "tx_mid", 0)
        .await
        .expect("authoritative call must succeed");
    let new_is_spent_auth = dag
        .is_outpoint_spent_authoritative(&store, "tx_new", 0)
        .await
        .expect("authoritative call must succeed");

    println!("test: authoritative_check_detects_fifo_evicted_outpoint");
    println!(
        "  RAM  is_spent (tx_old, tx_mid, tx_new) = ({}, {}, {})",
        old_is_spent_ram, mid_is_spent_ram, new_is_spent_ram
    );
    println!(
        "  AUTH is_spent (tx_old, tx_mid, tx_new) = ({}, {}, {})",
        old_is_spent_auth, mid_is_spent_auth, new_is_spent_auth
    );

    // `tx_old` is the first one we spent: it has been evicted from the
    // FIFO (cap is 2) and the RAM check now lies — this is the C3 bug.
    assert!(
        !old_is_spent_ram,
        "RAM check is expected to lie for evicted outpoints — this is why \
         is_outpoint_spent_authoritative exists"
    );

    // Fresh entries still in the FIFO are reported correctly by both paths.
    assert!(mid_is_spent_ram, "tx_mid must still be in RAM FIFO");
    assert!(new_is_spent_ram, "tx_new must still be in RAM FIFO");

    // The v0.7.2 invariant: the authoritative check MUST report every
    // ever-spent outpoint as spent, regardless of RAM eviction.
    assert!(
        old_is_spent_auth,
        "authoritative check MUST detect tx_old via storage fallback"
    );
    assert!(mid_is_spent_auth);
    assert!(new_is_spent_auth);
}

#[tokio::test]
async fn authoritative_check_is_false_for_never_spent_outpoint() {
    let dag = ConcurrentDag::with_capacity_and_spent_limit(10, 2);
    let store = SpentTrackingStore::new();

    // Only spend one outpoint.
    dag.mark_spent("tx_a", 0);
    store.record_spent("tx_a", 0);

    let unknown_ram = dag.is_spent("tx_unknown", 0);
    let unknown_auth = dag
        .is_outpoint_spent_authoritative(&store, "tx_unknown", 0)
        .await
        .expect("authoritative call must succeed");
    let known_auth = dag
        .is_outpoint_spent_authoritative(&store, "tx_a", 0)
        .await
        .expect("authoritative call must succeed");

    println!("test: authoritative_check_is_false_for_never_spent_outpoint");
    println!("  RAM  is_spent(tx_unknown) = {}", unknown_ram);
    println!("  AUTH is_spent(tx_unknown) = {}", unknown_auth);
    println!("  AUTH is_spent(tx_a)       = {}", known_auth);

    assert!(!unknown_ram);
    assert!(
        !unknown_auth,
        "authoritative check must NOT generate false positives — \
         unknown outpoints must report not-spent"
    );
    assert!(known_auth);
}
