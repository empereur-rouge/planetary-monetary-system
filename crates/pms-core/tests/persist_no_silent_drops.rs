//! Non-regression tests for the v0.7.2 persist pipeline fix.
//!
//! History: v0.7.1 `do_persist_block` wrapped `persist_tx.send()` in a 5s
//! `tokio::time::timeout` and returned `PutResult::Inserted` even when the
//! send timed out or the channel was closed. The background persist task
//! also dropped entire batches on a single storage failure with only a
//! `tracing::error!`. Together these two bugs could silently lose blocks
//! that callers had been told were persisted — a critical data-loss bug
//! for a financial system.
//!
//! These tests lock in the post-fix contract:
//! 1. Transient `append_blocks_batch` failures are retried; successful
//!    batches on retry do not shut down the task.
//! 2. Persistent failures drain the retry budget and then close the
//!    channel, so every subsequent `send()` returns `Err(closed)` — the
//!    signal `do_persist_block` now translates into an HTTP error instead
//!    of a fake `Inserted` acknowledgement.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pms_core::background_persist::{PersistJob, spawn_background_persist_with_retry};
use pms_storage::store::PutResult;
use pms_storage::{DagStorage, StoredBlock, UtxoDelta};
use pms_wire::WireBlock;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Mock `DagStorage` that fails the first `fail_first` calls to
/// `append_blocks_batch` and then succeeds. Only the methods actually
/// exercised by the background persist task are implemented; the rest
/// panic via `todo!()` so misuse is caught immediately.
struct CountingFailStore {
    fail_first: usize,
    calls: AtomicUsize,
    successes: AtomicUsize,
}

impl CountingFailStore {
    fn new(fail_first: usize) -> Self {
        Self {
            fail_first,
            calls: AtomicUsize::new(0),
            successes: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn successes(&self) -> usize {
        self.successes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DagStorage for CountingFailStore {
    async fn append_blocks_batch(
        &self,
        blocks: &[(&StoredBlock, Option<&UtxoDelta>, &[(String, u64)])],
    ) -> Result<usize> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_first {
            return Err(anyhow!("injected transient failure (call #{n})"));
        }
        self.successes.fetch_add(1, Ordering::SeqCst);
        Ok(blocks.len())
    }

    async fn persist_final(&self, _ids: &[String]) -> Result<()> {
        Ok(())
    }

    // --- Unused by background_persist_task: stubbed to panic if ever called. ---
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
    async fn persist_last_milestone(&self, _id: &str) -> Result<()> {
        todo!()
    }
}

fn make_job(id: &str) -> PersistJob {
    PersistJob {
        block: StoredBlock {
            id: id.to_string(),
            parents: vec![],
            payload_json: None,
            nonce: 0,
            network_id: "test".into(),
            protocol_version: 1,
            signer_pk_hex: "deadbeef".into(),
            signature_hex: "cafe".into(),
            metadata: None,
        },
        delta: None,
        newly_finalized: vec![],
        parent_count_updates: vec![],
    }
}

#[tokio::test]
async fn persist_retries_then_succeeds_on_transient_failure() {
    // 2 failures injected; the 3rd call succeeds. With retry delays of
    // [10ms, 20ms, 50ms] the total retry window is well under 100ms so the
    // test stays fast.
    let store = Arc::new(CountingFailStore::new(2));
    let (tx, handle) =
        spawn_background_persist_with_retry(store.clone(), 4, vec![10, 20, 50, 100, 100, 100]);

    let started = Instant::now();

    tx.send(make_job("block-1"))
        .await
        .expect("first send must succeed — channel is open");

    // Send a second job to confirm the task is still alive after retry.
    tx.send(make_job("block-2"))
        .await
        .expect("second send must succeed — task must still be running");

    // Let the task drain & process both jobs.
    tokio::time::sleep(Duration::from_millis(200)).await;

    drop(tx);
    handle.await.expect("background task joins cleanly");

    let elapsed = started.elapsed();
    println!("test: retries_then_succeeds");
    println!("  append_blocks_batch calls = {}", store.calls());
    println!("  successes                 = {}", store.successes());
    println!("  elapsed                   = {:?}", elapsed);

    // Expected: 2 failed calls on the first batch + 1 success, then
    // immediate success on the second batch = 4 total calls, 2 successes.
    // The first batch may or may not absorb both jobs depending on timing;
    // either way the total successes count the successful batches.
    assert!(
        store.successes() >= 1,
        "at least one batch must have been persisted successfully"
    );
    assert!(
        store.calls() >= 3,
        "the failing batch must have been retried at least twice before succeeding (got {} calls)",
        store.calls()
    );
}

#[tokio::test]
async fn persist_permanent_failure_closes_channel() {
    // Inject a huge failure budget so every retry fails. With 3 short
    // retry delays [5ms, 10ms, 10ms] the full retry loop completes in
    // well under 100ms before the task shuts down.
    let store = Arc::new(CountingFailStore::new(usize::MAX));
    let (tx, handle) = spawn_background_persist_with_retry(store.clone(), 4, vec![5, 10, 10]);

    // Push one job to trigger the retry-and-shutdown sequence.
    tx.send(make_job("block-dead"))
        .await
        .expect("first send must succeed — channel is open before shutdown");

    // Wait for the task to exhaust retries and shut down.
    handle.await.expect("background task joins cleanly");

    // After shutdown, *every* subsequent send() must report the channel as
    // closed so `do_persist_block` returns an error (v0.7.2 invariant).
    let send_result = tx.send(make_job("block-after-shutdown")).await;

    println!("test: permanent_failure_closes_channel");
    println!(
        "  append_blocks_batch calls = {} (must be initial try + all retries)",
        store.calls()
    );
    println!("  successes                 = {}", store.successes());
    println!("  post-shutdown send result = {:?}", send_result.is_err());

    assert_eq!(
        store.successes(),
        0,
        "no batch must have been persisted — store always fails"
    );
    // Initial attempt + 3 retries = 4 calls total.
    assert_eq!(
        store.calls(),
        4,
        "the task must try the initial call + each configured retry exactly once"
    );
    assert!(
        send_result.is_err(),
        "after permanent failure, send() must return Err(closed) — \
         this is the signal that do_persist_block translates into an HTTP \
         error instead of a fake PutResult::Inserted"
    );
}
