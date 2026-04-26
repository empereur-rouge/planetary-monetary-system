//! Background persistence task for high-TPS architecture.
//!
//! This module provides asynchronous block persistence to avoid blocking
//! the main request path on RocksDB writes.
//!
//! ## Architecture
//!
//! ```text
//! persist_block() → mpsc::send(block) → return OK immediately
//!                          ↓
//!               background_persist_task()
//!                          ↓
//!         batch drain (up to 64 jobs) via try_recv()
//!                          ↓
//!               store.append_blocks_batch() — single WriteBatch for all blocks
//!                          ↓
//!               batched persist_final() for all finalized blocks
//! ```

use crate::background_activity::ActivityJob;
use pms_storage::{DagStorage, StoredBlock, UtxoDelta};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Message sent to the background persist task.
pub struct PersistJob {
    /// The block to persist.
    pub block: StoredBlock,
    /// UTXO delta (spends and creates).
    pub delta: Option<UtxoDelta>,
    /// Newly finalized block IDs to persist.
    pub newly_finalized: Vec<String>,
    /// New `children_count` values for this block's parents, snapshotted
    /// from the in-memory DAG immediately after `insert_block` returns.
    ///
    /// Pre-0.7.4-step2 the consumer re-read each parent's count from
    /// RocksDB inside `append_blocks_batch` (`multi_get_cf` on `cf_count`),
    /// which became the dominant scaling cost as the DAG grew: parent
    /// blocks aged out of the memtable into L0 SSTs and the lookup walked
    /// the LSM tree on every batch. Per-block consumer cost was rising
    /// from ~50µs at boot to ~150µs after 1.5M UTXOs.
    ///
    /// Now the producer captures the post-insert count via
    /// `ConcurrentDag::get_children_count` (a lock-free atomic load) and
    /// hands the value to the consumer here, eliminating the LSM read
    /// entirely. Single-consumer FIFO order guarantees the last `put_cf`
    /// for any given parent reflects the highest count, so RocksDB
    /// converges to the in-memory truth.
    pub parent_count_updates: Vec<(String, u64)>,
}

/// Maximum blocks drained per batch iteration.
/// Higher = more WAL amortization, but higher per-block latency.
const MAX_BATCH_SIZE: usize = 64;

/// Default exponential backoff delays between `append_blocks_batch` retries
/// after a transient storage failure. Six attempts spread over ~52 seconds.
/// Tests can pass shorter delays via [`spawn_background_persist_with_retry`].
pub const DEFAULT_RETRY_DELAYS_MS: [u64; 6] = [100, 500, 2_000, 5_000, 15_000, 30_000];

/// Spawns the background persistence task with batch draining.
///
/// The consumer drains up to [`MAX_BATCH_SIZE`] jobs per iteration using
/// non-blocking `try_recv()` after the initial `recv().await`. All blocks
/// in the batch are persisted in a **single** `WriteBatch` via
/// [`DagStorage::append_blocks_batch`], reducing WAL appends and DB mutex
/// acquisitions by up to 64×.
///
/// Returns a sender that can be used to queue blocks for persistence.
///
/// # Arguments
/// * `store` - The storage backend (RocksDB)
/// * `buffer_size` - How many blocks can be queued before backpressure
///
/// # Example
/// ```ignore
/// let (tx, handle) = spawn_background_persist(store.clone(), 10_000);
/// tx.send(PersistJob {
///     block,
///     delta,
///     newly_finalized: vec![],
///     parent_count_updates: vec![],
/// }).await?;
/// ```
pub fn spawn_background_persist<S>(
    store: Arc<S>,
    buffer_size: usize,
) -> (mpsc::Sender<PersistJob>, tokio::task::JoinHandle<()>)
where
    S: DagStorage + Send + Sync + 'static,
{
    spawn_background_persist_with_retry_and_activity(
        store,
        buffer_size,
        DEFAULT_RETRY_DELAYS_MS.to_vec(),
        None,
    )
}

/// Same as [`spawn_background_persist`] but with a side-channel to the
/// activity writer task. Each block that successfully persists also emits
/// an `ActivityJob` to `activity_tx` (fire-and-forget — see
/// `pms_core::background_activity` for failure semantics).
pub fn spawn_background_persist_with_activity<S>(
    store: Arc<S>,
    buffer_size: usize,
    activity_tx: mpsc::Sender<ActivityJob>,
) -> (mpsc::Sender<PersistJob>, tokio::task::JoinHandle<()>)
where
    S: DagStorage + Send + Sync + 'static,
{
    spawn_background_persist_with_retry_and_activity(
        store,
        buffer_size,
        DEFAULT_RETRY_DELAYS_MS.to_vec(),
        Some(activity_tx),
    )
}

/// Test-oriented variant of [`spawn_background_persist`] that accepts custom
/// retry delays. Production code should call [`spawn_background_persist`].
pub fn spawn_background_persist_with_retry<S>(
    store: Arc<S>,
    buffer_size: usize,
    retry_delays_ms: Vec<u64>,
) -> (mpsc::Sender<PersistJob>, tokio::task::JoinHandle<()>)
where
    S: DagStorage + Send + Sync + 'static,
{
    spawn_background_persist_with_retry_and_activity(store, buffer_size, retry_delays_ms, None)
}

/// Inner constructor: accepts both custom retry delays AND an optional
/// activity-writer hand-off channel. The other public entry points are
/// thin wrappers that pick sensible defaults.
pub fn spawn_background_persist_with_retry_and_activity<S>(
    store: Arc<S>,
    buffer_size: usize,
    retry_delays_ms: Vec<u64>,
    activity_tx: Option<mpsc::Sender<ActivityJob>>,
) -> (mpsc::Sender<PersistJob>, tokio::task::JoinHandle<()>)
where
    S: DagStorage + Send + Sync + 'static,
{
    let (tx, mut rx) = mpsc::channel::<PersistJob>(buffer_size);

    let handle = tokio::spawn(async move {
        // Counters for logging
        let mut persisted_count: u64 = 0;
        let mut error_count: u64 = 0;

        while let Some(first_job) = rx.recv().await {
            // Batch drain: collect up to MAX_BATCH_SIZE-1 additional jobs
            // without blocking. This amortizes WAL and finality overhead.
            let mut batch = Vec::with_capacity(MAX_BATCH_SIZE);
            batch.push(first_job);
            while batch.len() < MAX_BATCH_SIZE {
                match rx.try_recv() {
                    Ok(job) => batch.push(job),
                    Err(_) => break,
                }
            }

            let batch_size = batch.len();

            // Build slice of references for the batch persist call.
            // This avoids cloning StoredBlock/UtxoDelta — just borrows.
            // The third tuple field is the producer-supplied
            // `parent_count_updates`; the consumer uses it to write the
            // children_count CF without any LSM reads.
            let block_refs: Vec<(
                &StoredBlock,
                Option<&UtxoDelta>,
                &[(String, u64)],
            )> = batch
                .iter()
                .map(|j| {
                    (
                        &j.block,
                        j.delta.as_ref(),
                        j.parent_count_updates.as_slice(),
                    )
                })
                .collect();

            // Single atomic write for ALL blocks in the batch.
            // RocksStore overrides this with a mega WriteBatch (1 WAL append
            // instead of N), while the default trait impl falls back to
            // per-block writes for non-RocksDB backends.
            //
            // Retry with exponential backoff on transient errors. We cannot
            // silently drop a batch — the producer already received
            // `PutResult::Inserted` for these blocks. After exhausting
            // retries we break out of the main loop, which drops `rx`; the
            // channel then reports closed to every subsequent `send().await`
            // so `do_persist_block` returns an error to the HTTP caller
            // instead of another false success.
            let mut persisted_this_batch = false;
            for (attempt, delay_ms) in std::iter::once(0u64)
                .chain(retry_delays_ms.iter().copied())
                .enumerate()
            {
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                let batch_start = std::time::Instant::now();
                match store.append_blocks_batch(&block_refs).await {
                    Ok(new_count) => {
                        let batch_us = batch_start.elapsed().as_micros() as u64;
                        crate::metrics::PERSIST_CONSUMER_US.inc_by(batch_us);
                        crate::metrics::PERSIST_CONSUMER_BATCHES.inc();
                        crate::metrics::PERSIST_CONSUMER_BLOCKS
                            .inc_by(batch_size as u64);
                        persisted_count += new_count as u64;
                        if attempt > 0 {
                            tracing::warn!(
                                target = "pms_persist",
                                attempt,
                                batch_size,
                                "Recovered from transient persist failure"
                            );
                        }
                        persisted_this_batch = true;
                        break;
                    }
                    Err(e) => {
                        // Count every retry attempt — a non-zero rate of
                        // `pms_persist_retries_total` is the early-warning
                        // signal that RocksDB is stalling.
                        crate::metrics::PERSIST_RETRIES.inc();
                        tracing::error!(
                            target = "pms_persist",
                            error = %e,
                            attempt,
                            batch_size,
                            "Failed to persist block batch (will retry)"
                        );
                    }
                }
            }

            if !persisted_this_batch {
                error_count += batch_size as u64;
                // Each terminal failure increments the alerting counter.
                // The batch may contain N blocks; we count it as 1 batch
                // because the consumer is about to shut down — granularity
                // is moot at that point.
                crate::metrics::PERSIST_FAILURES.inc();
                tracing::error!(
                    target = "pms_persist",
                    batch_size,
                    total_errors = error_count,
                    "Persist batch FAILED after all retries — shutting down background task. \
                     New send() calls will return an error so callers stop acknowledging blocks."
                );
                break;
            }

            // Forward the freshly-persisted blocks to the activity writer
            // (if wired). The activity writer task drains its own channel
            // and writes the addr_activity / addr_type_activity /
            // activity_items CFs out-of-band, freeing the persist
            // consumer from competing with non-critical writes for the
            // same db.write() slot. try_send is intentional: activity is
            // best-effort, and we'd rather drop dashboard rows than
            // back-pressure the producer through the persist channel.
            if let Some(ref atx) = activity_tx {
                let now_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                for job in batch.iter() {
                    let aj = ActivityJob {
                        block: job.block.clone(),
                        ts: now_ts,
                    };
                    if atx.try_send(aj).is_err() {
                        // Channel full → log periodically and drop. The
                        // dashboard will miss these blocks; balances are
                        // unaffected.
                        static DROPPED: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);
                        let n = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if n.is_multiple_of(1000) {
                            tracing::warn!(
                                target = "pms_activity",
                                total_dropped = n + 1,
                                "Activity channel saturated — dropping events"
                            );
                        }
                    }
                }
            }

            // Batched finality persist: collect all newly_finalized from the batch
            // into a single persist_final() call instead of N separate calls.
            let all_finalized: Vec<String> = batch
                .iter()
                .flat_map(|j| j.newly_finalized.iter().cloned())
                .collect();
            if !all_finalized.is_empty() {
                if let Err(e) = store.persist_final(&all_finalized).await {
                    tracing::warn!(
                        target = "pms_persist",
                        error = %e,
                        count = all_finalized.len(),
                        "Failed to persist finality batch"
                    );
                }
            }

            // Log every ~1000 blocks to avoid spam
            if persisted_count % 1000 < batch_size as u64 {
                tracing::info!(
                    target = "pms_persist",
                    count = persisted_count,
                    errors = error_count,
                    last_batch = batch_size,
                    "Background persist progress"
                );
            }
        }

        tracing::info!(
            target = "pms_persist",
            total_persisted = persisted_count,
            total_errors = error_count,
            "Background persist task shutting down"
        );
    });

    (tx, handle)
}
