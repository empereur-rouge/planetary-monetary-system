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
//!               store.append_block_atomic_with_utxo()
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

/// Spawns the background persistence task.
///
/// Returns a sender that can be used to queue blocks for persistence.
///
/// # Arguments
/// * `store` - The storage backend (RocksDB)
/// * `buffer_size` - How many blocks can be queued before backpressure
///
/// # Example
/// ```ignore
/// let (tx, handle) = spawn_background_persist(store.clone(), 1000);
/// tx.send(PersistJob { block, delta }).await?;
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
        // Counter for logging
        let mut persisted_count: u64 = 0;
        let mut error_count: u64 = 0;

        while let Some(job) = rx.recv().await {
            // Persist the block to RocksDB
            match store
                .append_block_atomic_with_utxo(&job.block, job.delta.as_ref())
                .await
            {
                Ok(true) => {
                    persisted_count += 1;

                    // Persist finality
                    if !job.newly_finalized.is_empty() {
                        if let Err(e) = store.persist_final(&job.newly_finalized).await {
                            tracing::warn!(
                                target = "pms_persist",
                                error = %e,
                                "Failed to persist finality"
                            );
                        }
                    }

                    // Log every 1000 blocks to avoid spam
                    if persisted_count % 1000 == 0 {
                        tracing::info!(
                            target = "pms_persist",
                            count = persisted_count,
                            errors = error_count,
                            "Background persist progress"
                        );
                    }
                }
                Ok(false) => {
                    // Block already exists, not an error
                }
                Err(e) => {
                    error_count += 1;
                    tracing::error!(
                        target = "pms_persist",
                        block_id = %job.block.id,
                        error = %e,
                        "Failed to persist block"
                    );
                }
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
