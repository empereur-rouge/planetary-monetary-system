//! Prometheus counters for the persist pipeline (item 4, v0.7.4).
//!
//! These are declared in `pms-core` (instead of `pms-server`) because the
//! sites that drive them — the background persist consumer and the producer
//! back-pressure loop — live here. Declaring them in the global default
//! registry means `pms_server::metrics::render()` collects them through
//! `prometheus::gather()` without any explicit wiring.
//!
//! All three are unlabeled `IntCounter`s. We intentionally don't label by
//! `ledger_id`: the persist pipeline is per-`CoreAdapter`, but in practice a
//! single RocksDB process serves every ledger, so retries / failures /
//! stalls correlate across ledgers. A single global counter is the right
//! grain for "should I page the operator?" alerting. Per-ledger queue depth
//! and capacity gauges live in `pms-server::metrics` and are sampled by the
//! metrics sampler task — those keep the per-ledger view.

use once_cell::sync::Lazy;
use prometheus::IntCounter;

/// Number of `append_blocks_batch` retry attempts. Incremented every time a
/// transient storage error fires the retry path in
/// [`crate::background_persist::spawn_background_persist_with_retry`]. A
/// non-zero rate means RocksDB is stalling under load — investigate disk
/// throughput, compaction, or `[rocks]` tuning.
pub static PERSIST_RETRIES: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_retries_total",
        "Background persist batch retries after transient storage errors"
    )
    .unwrap()
});

/// Number of batches that exhausted every retry and gave up. Each
/// increment is a fatal-for-the-batch event: the consumer task shuts
/// down and subsequent `persist_block().await` calls return an error
/// to the HTTP caller. This should alert immediately — anything > 0
/// in production means at least one block failed to persist.
pub static PERSIST_FAILURES: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_failures_total",
        "Persist batches that failed after all retry attempts"
    )
    .unwrap()
});

/// Cumulative seconds spent in the back-pressure stall loop in
/// [`crate::net_adapter::persist`]. Every second the producer is blocked on
/// `persist_tx.send().await` (channel saturated), the warn-interval fires
/// and adds 1 here. A growing rate means the consumer can't drain the
/// queue fast enough — pair with `pms_persist_queue_depth` to see how
/// close we are to the buffer limit.
pub static PERSIST_STALL_SECONDS: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_stall_seconds_total",
        "Cumulative seconds spent back-pressured on persist_tx.send().await"
    )
    .unwrap()
});
