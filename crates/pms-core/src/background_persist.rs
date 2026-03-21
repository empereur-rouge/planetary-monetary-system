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
            match store.append_blocks_batch(&block_refs).await {
                Ok(new_count) => {
                    persisted_count += new_count as u64;
                }
                Err(e) => {
                    error_count += batch_size as u64;
                    tracing::error!(
                        target = "pms_persist",
                        error = %e,
                        batch_size,
                        "Failed to persist block batch"
                    );
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
