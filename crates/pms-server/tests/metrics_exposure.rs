//! Smoke test for the v0.7.4 Prometheus surface (item 4).
//!
//! The audit asked for nine new metrics. The list below is derived from
//! the plan; the test asserts that `crate::metrics::render()` exposes
//! every name we promised. Without this guard a future refactor could
//! silently rename or drop a metric and Grafana / alerting rules would
//! quietly stop firing.
//!
//! `admin_auth_failures_total` already shipped in item 1 — we re-check
//! it here so the surface stays whole.
//!
//! We poke each metric with a non-zero label/value before rendering so
//! Prometheus emits a typed line for it (some metrics are lazy and don't
//! appear in `gather()` output until they're touched).

use pms_server::metrics;

#[test]
fn all_v074_metrics_are_exposed() {
    // 1) Touch every metric so Prometheus emits its definition. We're
    //    not asserting on values here — only on the metric names being
    //    present in the registry once observed.
    metrics::ADMIN_AUTH_FAILURES
        .with_label_values(&["wrong_token"])
        .inc();
    metrics::PERSIST_QUEUE_DEPTH
        .with_label_values(&["main"])
        .set(0);
    metrics::PERSIST_QUEUE_CAPACITY
        .with_label_values(&["main"])
        .set(2000);
    metrics::FEE_POOL_TOTAL.with_label_values(&["main"]).set(0.0);
    metrics::FEES_DISTRIBUTED
        .with_label_values(&["main", "treasury"])
        .inc_by(0.0);
    metrics::UTXO_SET_SIZE.with_label_values(&["main"]).set(0);
    metrics::ROCKSDB_WRITE_STALLED_SECONDS.inc_by(0);

    // 2) These three are declared in pms-core but registered into the
    //    same global Prometheus registry so the same `render()` picks
    //    them up. Touch them via the public path: incrementing through
    //    the static handles is what production code does too.
    pms_core::metrics::PERSIST_RETRIES.inc_by(0);
    pms_core::metrics::PERSIST_FAILURES.inc_by(0);
    pms_core::metrics::PERSIST_STALL_SECONDS.inc_by(0);

    // 3) Render and assert every promised metric name shows up.
    let dump = metrics::render();
    println!("--- /metrics surface ---\n{dump}\n--- end /metrics ---");

    let expected = [
        // shipped earlier in item 1 — re-checked for survival
        "pms_admin_auth_failures_total",
        // queue gauges — driven by the metrics sampler
        "pms_persist_queue_depth",
        "pms_persist_queue_capacity",
        // fee accounting
        "pms_fee_pool_total",
        "pms_fees_distributed_total",
        // RAM-side health
        "pms_utxo_set_size",
        // RocksDB stall signal
        "pms_rocksdb_write_stalled_seconds_total",
        // pipeline counters declared in pms-core
        "pms_persist_retries_total",
        "pms_persist_failures_total",
        "pms_persist_stall_seconds_total",
    ];

    for name in expected {
        assert!(
            dump.contains(name),
            "metric `{name}` is missing from /metrics output"
        );
    }
    println!("OK — all {} expected metrics present.", expected.len());
}
