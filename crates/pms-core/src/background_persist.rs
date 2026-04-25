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
/// tx.send(PersistJob { block, delta, newly_finalized: vec![] }).await?;
/// ```
pub fn spawn_background_persist<S>(
    store: Arc<S>,
    buffer_size: usize,
) -> (mpsc::Sender<PersistJob>, tokio::task::JoinHandle<()>)
where
    S: DagStorage + Send + Sync + 'static,
{
    spawn_background_persist_with_retry(store, buffer_size, DEFAULT_RETRY_DELAYS_MS.to_vec())
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
            let block_refs: Vec<(&StoredBlock, Option<&UtxoDelta>)> = batch
                .iter()
                .map(|j| (&j.block, j.delta.as_ref()))
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
                match store.append_blocks_batch(&block_refs).await {
                    Ok(new_count) => {
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
