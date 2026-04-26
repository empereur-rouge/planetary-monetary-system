//! Background activity-index writer.
//!
//! The dashboard's history API reads from three "activity" RocksDB CFs:
//! `addr_activity`, `addr_type_activity`, and `activity_items`. None of
//! those are on the consensus / balance / UTXO path — they only tell the
//! UI "this address was involved in this block at this timestamp". They
//! tolerate seconds of lag without any user-visible breakage.
//!
//! Pre-step3 these writes lived inside `RocksStore::append_blocks_batch`,
//! which meant the hot persist consumer had to serialize them on the same
//! RocksDB write that committed the critical block / UTXO / children-count
//! state. As the DAG grew, the per-batch `db.write()` time grew too,
//! throttling the producer through `persist_tx.send().await` back-pressure.
//!
//! This module spawns a second consumer task that drains a dedicated
//! channel and writes the activity CFs in its own `WriteBatch`. RocksDB's
//! pipelined writes let the two `db.write()` calls overlap, doubling
//! effective consumer throughput without any sharding or correctness
//! gymnastics.
//!
//! ## Failure semantics
//!
//! The producer side uses `try_send` — if the activity channel ever fills
//! up (e.g. the writer task stalls), we log and drop. The dashboard will
//! miss those blocks in its history view, but balances and the DAG itself
//! are unaffected. This is a deliberate trade: dashboard correctness is
//! eventual, not strict.

use pms_storage::{DagStorage, StoredBlock};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Message sent to the activity writer task.
pub struct ActivityJob {
    /// The block whose activity indices should be written. Cloned out of
    /// the StoredBlock that the persist consumer just wrote — pre-clone
    /// keeps the activity task lock-free against the persist consumer.
    pub block: StoredBlock,
    /// Persistence timestamp in milliseconds since UNIX epoch. Captured
    /// in the producer (next to where the block was sent to persist_tx)
    /// so all activity entries for the same block share the same
    /// timestamp regardless of when the writer drains them.
    pub ts: i64,
}

/// Maximum jobs drained per writer iteration. Higher than the persist
/// consumer's 64 because each activity write is small (3 puts per block
/// per address) and benefits from amortizing the WAL fsync over more
/// keys per `db.write()`.
const MAX_ACTIVITY_BATCH: usize = 256;

/// Spawn the activity writer task. Returns the producer-side sender.
///
/// `buffer_size` should be generous — the activity channel is non-
/// critical, so we'd rather over-allocate RAM than start dropping events
/// during a transient stall.
pub fn spawn_activity_writer<S>(
    store: Arc<S>,
    buffer_size: usize,
) -> (mpsc::Sender<ActivityJob>, tokio::task::JoinHandle<()>)
where
    S: DagStorage + Send + Sync + 'static,
{
    let (tx, mut rx) = mpsc::channel::<ActivityJob>(buffer_size);

    let handle = tokio::spawn(async move {
        let mut written: u64 = 0;
        let mut errors: u64 = 0;
        while let Some(first) = rx.recv().await {
            let mut batch: Vec<ActivityJob> = Vec::with_capacity(MAX_ACTIVITY_BATCH);
            batch.push(first);
            while batch.len() < MAX_ACTIVITY_BATCH {
                match rx.try_recv() {
                    Ok(j) => batch.push(j),
                    Err(_) => break,
                }
            }

            let n = batch.len();
            let refs: Vec<(&StoredBlock, i64)> =
                batch.iter().map(|j| (&j.block, j.ts)).collect();
            match store.append_activity_batch(&refs).await {
                Ok(_) => {
                    written += n as u64;
                    if written % 5000 < n as u64 {
                        tracing::info!(
                            target = "pms_activity",
                            written,
                            errors,
                            "Activity writer progress"
                        );
                    }
                }
                Err(e) => {
                    errors += 1;
                    tracing::warn!(
                        target = "pms_activity",
                        error = %e,
                        batch_size = n,
                        "Activity batch write failed (non-fatal)"
                    );
                }
            }
        }
        tracing::info!(
            target = "pms_activity",
            written,
            errors,
            "Activity writer shutting down"
        );
    });

    (tx, handle)
}
