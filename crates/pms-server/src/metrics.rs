// pms-server/src/metrics.rs
use once_cell::sync::Lazy;
use prometheus::{Encoder, GaugeVec, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, TextEncoder};

pub static BLOCKS_REJECTED: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_blocks_rejected_total",
        "Blocs rejetés lors de la persistance",
        &["ledger_id"]
    )
    .unwrap()
});

pub static BLOCKS_PERSISTED: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_blocks_persisted_total",
        "Blocs validés et persistés",
        &["ledger_id"]
    )
    .unwrap()
});

pub static PMS_BLOCKS_TOTAL: Lazy<IntGaugeVec> = Lazy::new(|| {
    prometheus::register_int_gauge_vec!(
        "pms_blocks_total",
        "Nombre total de blocs connus (DAG size)",
        &["ledger_id"]
    )
    .unwrap()
});

/// Admin-auth failures counter (unauthorized hits on admin routes).
///
/// Incremented by the admin middlewares on every 401/403 so that an operator
/// can alert on sudden bursts (brute force / token leak attempts). Labelled
/// by `reason` so that IP-allowlist rejections, missing token, and wrong
/// token can be differentiated without exploding cardinality on route.
pub static ADMIN_AUTH_FAILURES: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_admin_auth_failures_total",
        "Admin endpoint authentication failures",
        &["reason"] // "ip_not_allowed" | "missing_token" | "wrong_token"
    )
    .unwrap()
});

/// Current depth of the background persist queue, sampled every 5s.
/// Labelled by `ledger_id` because each `CoreAdapter` owns a separate
/// channel — operators want to see which ledger is back-pressured.
pub static PERSIST_QUEUE_DEPTH: Lazy<IntGaugeVec> = Lazy::new(|| {
    prometheus::register_int_gauge_vec!(
        "pms_persist_queue_depth",
        "Current pending jobs in the persist channel (used capacity)",
        &["ledger_id"]
    )
    .unwrap()
});

/// Configured maximum capacity of the persist channel. Sampled together
/// with `pms_persist_queue_depth` so dashboards can compute the usage
/// ratio without hard-coding the buffer size.
pub static PERSIST_QUEUE_CAPACITY: Lazy<IntGaugeVec> = Lazy::new(|| {
    prometheus::register_int_gauge_vec!(
        "pms_persist_queue_capacity",
        "Configured capacity of the persist channel buffer",
        &["ledger_id"]
    )
    .unwrap()
});

/// Total fees (in PMS, native units) currently sitting in the in-memory
/// fee pool waiting for the next distribution round. Sampled every 5s
/// from `FeePoolRegistry`. A gauge that grows without bouncing back means
/// `perform_fee_distribution` is failing — pair with the error logs from
/// the fee distributor task.
///
/// Recorded as `f64` because `Decimal` serialises poorly into Prometheus;
/// the conversion is lossy beyond ~15 significant digits but operators
/// monitor magnitudes here, not exact values.
pub static FEE_POOL_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    prometheus::register_gauge_vec!(
        "pms_fee_pool_total",
        "PMS in the fee pool waiting for distribution (native units)",
        &["ledger_id"]
    )
    .unwrap()
});

/// Counter of PMS distributed by `perform_fee_distribution`, broken
/// down by where it went. `recipient_type` is one of:
///
///   - `burn_refund` — refund to a user wallet (e.g. cube burn payouts).
///     Note that custom-asset refunds (Edenite) are recorded with
///     `recipient_type = "burn_refund"` but the *value* is still in PMS
///     because we use a single counter unit; the actual payout amount
///     differs and is best read from the distribution logs.
///   - `treasury` — coordinator-tax cut.
///   - `node` — share allocated to a registered node.
///
/// Lossy `f64` like `FEE_POOL_TOTAL`.
pub static FEES_DISTRIBUTED: Lazy<prometheus::CounterVec> = Lazy::new(|| {
    prometheus::register_counter_vec!(
        "pms_fees_distributed_total",
        "Cumulative PMS distributed by fee distribution rounds",
        &["ledger_id", "recipient_type"]
    )
    .unwrap()
});

/// Current size of the in-memory UTXO set per ledger, sampled every 5s.
/// Bumps up against the `[rocks].max_utxos` cap mean the LRU is evicting
/// — `balance_by_address` will start falling through to the storage
/// layer (slower) but stays correct.
pub static UTXO_SET_SIZE: Lazy<IntGaugeVec> = Lazy::new(|| {
    prometheus::register_int_gauge_vec!(
        "pms_utxo_set_size",
        "Number of unspent outputs in the in-memory UTXO set",
        &["ledger_id"]
    )
    .unwrap()
});

/// Cumulative seconds during which RocksDB had `is-write-stopped == 1`.
/// Sampled every 5s by the metrics task: each tick where the property
/// reads `1` adds the sampling interval to the counter. A growing rate
/// means the L0 file count tripped `level0_stop_writes_trigger` — that
/// is the canonical "writes are blocked" signal RocksDB exposes to its
/// host process.
pub static ROCKSDB_WRITE_STALLED_SECONDS: Lazy<IntCounter> = Lazy::new(|| {
    prometheus::register_int_counter!(
        "pms_rocksdb_write_stalled_seconds_total",
        "Cumulative seconds RocksDB was reporting is-write-stopped == 1"
    )
    .unwrap()
});

/// 1 when the engine has flipped into read-only mode (writes return
/// 503), 0 otherwise. Sampled by the resource-guard task on every
/// state transition. Pair with `pms_read_only_rejections_total` to
/// see how much traffic the guard is shedding while armed.
///
/// The reason ("memory" / "disk" / "rocksdb" / "manual") is intentionally
/// NOT a label — it would multiply gauge cardinality with no useful
/// dashboard value. The reason is exposed instead via `/healthz` and
/// `/admin/read-only/status`, and is logged on every transition.
pub static ENGINE_READ_ONLY: Lazy<IntGauge> = Lazy::new(|| {
    prometheus::register_int_gauge!(
        "pms_engine_read_only",
        "1 when the engine is in read-only mode (writes return 503), 0 otherwise"
    )
    .unwrap()
});

/// Cumulative count of write requests rejected with 503 because the
/// engine was in read-only mode. Labelled by `reason` so an operator
/// can see whether the rejections are driven by memory, disk, or
/// RocksDB pressure (or were operator-initiated via `Manual`).
pub static READ_ONLY_REJECTIONS: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_read_only_rejections_total",
        "Write requests rejected because the engine was in read-only mode",
        &["reason"]
    )
    .unwrap()
});

/// Cumulative count of API errors returned to clients, broken down by
/// numeric error code (see `api_error::ApiError`). Bounded cardinality
/// (~30 codes) — safe for Prometheus. Pair with `pms_api_request_duration_seconds`
/// to compute error rate per route.
///
/// Operators alert on:
///   - `rate(pms_api_errors_total{code="9999"}[5m]) > 0` — internal errors
///     surfacing means a handler still returns generic anyhow (migration target).
///   - `rate(pms_api_errors_total{code=~"4...""}[5m]) > 1` — sustained crypto /
///     auth failures usually mean replay / brute force.
pub static API_ERRORS: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_api_errors_total",
        "API errors returned to clients, labelled by stable numeric code",
        &["code"]
    )
    .unwrap()
});

/// API request latency histogram (seconds) — labels: method, route.
///
/// Uses `MatchedPath` from axum to get route templates (e.g. `/v1/wallet/{addr}/balance`)
/// instead of actual paths, preventing label cardinality explosion.
pub static API_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    prometheus::register_histogram_vec!(
        "pms_api_request_duration_seconds",
        "API request latency in seconds",
        &["method", "route"],
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]
    )
    .unwrap()
});

/// Render ALL metrics in standard Prometheus text format (with labels).
/// Used by `/metrics/all` for ops/Grafana scraping.
pub fn render() -> String {
    let mut buf = Vec::new();
    let encoder = TextEncoder::new();
    let mf = prometheus::gather();
    encoder.encode(&mf, &mut buf).ok();
    String::from_utf8(buf).unwrap_or_default()
}

/// Render metrics for a specific ledger in dashboard-compatible format.
/// Outputs plain metric names (no labels) so the dashboard parseMetrics() works unchanged.
pub fn render_for_ledger(ledger_id: &str) -> String {
    let blocks_total = PMS_BLOCKS_TOTAL.with_label_values(&[ledger_id]).get();
    let persisted = BLOCKS_PERSISTED.with_label_values(&[ledger_id]).get();
    let rejected = BLOCKS_REJECTED.with_label_values(&[ledger_id]).get();

    format!(
        "# HELP pms_blocks_total Nombre total de blocs connus (DAG size)\n\
         # TYPE pms_blocks_total gauge\n\
         pms_blocks_total {}\n\
         # HELP pms_blocks_persisted_total Blocs validés et persistés\n\
         # TYPE pms_blocks_persisted_total counter\n\
         pms_blocks_persisted_total {}\n\
         # HELP pms_blocks_rejected_total Blocs rejetés lors de la persistance\n\
         # TYPE pms_blocks_rejected_total counter\n\
         pms_blocks_rejected_total {}\n",
        blocks_total, persisted, rejected
    )
}
