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
use prometheus::{IntCounter, IntCounterVec};

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

/// Cumulative microseconds spent in each stage of `do_persist_block_internal`.
/// Used by the TPS-degradation profile to identify which stage scales with
/// total UTXO / block count. Pair with `pms_persist_blocks_total` to compute
/// the average µs/block per stage between two scrapes.
///
/// Stages:
///   - `parents`    — parent validation (find/check parent existence + uniqueness)
///   - `utxo_val`   — UTXO validation (double-spend check on inputs)
///   - `dag_val`    — DAG-level structural validation
///   - `utxo_ram`   — UTXO RAM apply (`apply_diff` on `ShardedUtxoSet`)
///   - `dag_insert` — DAG insert (`ConcurrentDag::insert_block` + finality)
///   - `send`       — `persist_tx.send().await` (channel back-pressure)
pub static PERSIST_STAGE_US: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_persist_stage_us_total",
        "Cumulative microseconds spent in each stage of persist_block",
        &["stage"]
    )
    .unwrap()
});

/// Total number of blocks that completed `do_persist_block_internal`
/// successfully. Used as the denominator when computing the per-stage
/// average latency from `pms_persist_stage_us_total`.
pub static PERSIST_BLOCKS_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_blocks_total",
        "Blocks that completed do_persist_block successfully"
    )
    .unwrap()
});

/// Cumulative microseconds spent inside `store.append_blocks_batch()` in
/// the background persist consumer. Pair with `pms_persist_consumer_batches_total`
/// to compute the avg µs/batch and with `pms_persist_blocks_total` to
/// compute the avg µs/block on the consumer side.
pub static PERSIST_CONSUMER_US: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_consumer_us_total",
        "Cumulative microseconds spent in store.append_blocks_batch() (consumer)"
    )
    .unwrap()
});

/// Number of batches the background persist consumer has drained from
/// the channel and successfully written. Combined with PERSIST_CONSUMER_US,
/// gives the rolling avg batch latency.
pub static PERSIST_CONSUMER_BATCHES: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_consumer_batches_total",
        "Batches the background persist consumer has written"
    )
    .unwrap()
});

/// Sum of the `len()` of every batch the consumer has written. Lets
/// you compute average batch fill (`blocks_in_batches / batches`) — if
/// it stays close to MAX_BATCH_SIZE the consumer is genuinely the
/// bottleneck; if it's well below, producers can't keep up either.
pub static PERSIST_CONSUMER_BLOCKS: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_consumer_blocks_total",
        "Total blocks written by the background persist consumer"
    )
    .unwrap()
});
