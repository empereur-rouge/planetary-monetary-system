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
use prometheus::{Histogram, IntCounter, IntCounterVec};

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

/// Number of producer-side `persist_tx.send().await` calls that waited
/// ≥ 500 ms before completing — i.e. the persist channel was full and
/// the producer was back-pressured (v0.8.0).
///
/// Pair this with `pms_persist_queue_depth` to detect bursts that
/// saturate the channel for less than the 5 s sampling cadence:
/// the depth gauge will show 0 across two consecutive samples but
/// this counter will increment for every block that hit the wall in
/// between. The resource guard task in pms-server consumes the
/// process-global accumulator (see [`crate::back_pressure`]) on its
/// own tick and arms read-only mode proactively.
///
/// Testnet 2026-05-04 baseline: 6728 events in 24 h before the
/// producer-signal was wired into the resource guard. Expectation
/// post-fix: this counter still increments (signal SOURCE), but
/// each event triggers a fast-arm of `pms_engine_read_only` so
/// downstream clients see clean 503s instead of waiting up to 6 s.
pub static PERSIST_BACK_PRESSURE_EVENTS: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_back_pressure_events_total",
        "Producer-side persist_tx.send().await calls that waited >= 500ms (channel saturation)"
    )
    .unwrap()
});

/// Distribution of consumer batch sizes (v0.7.30). Cumulative counters
/// (`PERSIST_CONSUMER_BLOCKS / PERSIST_CONSUMER_BATCHES`) only give us
/// the **average** batch size. Under bursty load the distribution
/// bimodal: idle ticks process 1-block batches while saturated ticks
/// process MAX_BATCH_SIZE blocks. Average hides this. Knowing the p50
/// vs p99 batch size tells us whether RocksDB is starved (p99 = 1) or
/// fully amortizing fsync (p99 = MAX_BATCH_SIZE).
///
/// Buckets cover 1..256 in geometric steps, matching the typical batch
/// distribution we expect to observe.
pub static PERSIST_CONSUMER_BATCH_SIZE: Lazy<Histogram> = Lazy::new(|| {
    prometheus::register_histogram!(
        "pms_persist_consumer_batch_size",
        "Distribution of consumer batch sizes per `append_blocks_batch` call",
        vec![1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0]
    )
    .unwrap()
});

/// Distribution of `append_blocks_batch` wall-time durations in
/// milliseconds (v0.7.30). Pair with `PERSIST_CONSUMER_BATCH_SIZE` to
/// detect compaction stalls: a single batch taking >1 s means RocksDB
/// is holding a lock during compaction or memtable flush. The bucket
/// edges are tuned for the expected range — sub-ms typical, with a
/// long tail up to 30 s for the worst stalls observed on testnet.
pub static PERSIST_CONSUMER_BATCH_DURATION_MS: Lazy<Histogram> = Lazy::new(|| {
    prometheus::register_histogram!(
        "pms_persist_consumer_batch_duration_ms",
        "Wall-time duration of each `append_blocks_batch` call, in milliseconds",
        vec![
            0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0,
            1000.0, 2500.0, 5000.0, 10_000.0, 30_000.0,
        ]
    )
    .unwrap()
});

/// Number of dedup checks that the in-RAM Bloom filter answered
/// authoritatively negative — i.e. the block_id was definitely not
/// in RocksDB so the consumer skipped the LSM read entirely. The
/// dominant outcome under steady state once warmed.
pub static PERSIST_BLOOM_SKIPS: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_bloom_skips_total",
        "Dedup checks resolved by the bloom (LSM read skipped)"
    )
    .unwrap()
});

/// Number of dedup checks where the Bloom filter returned a positive
/// (and the consumer therefore had to confirm via `multi_get_cf`).
/// Most of these are actual duplicates from network-level redelivery
/// or producer races; the rest are false positives (≈0.8% by design).
pub static PERSIST_BLOOM_HITS: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_persist_bloom_hits_total",
        "Dedup checks where the bloom said 'maybe' and triggered a fallback multi_get_cf"
    )
    .unwrap()
});
