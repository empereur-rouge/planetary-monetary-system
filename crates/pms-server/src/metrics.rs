// pms-server/src/metrics.rs
use once_cell::sync::Lazy;
use prometheus::{Encoder, HistogramVec, IntCounterVec, IntGaugeVec, TextEncoder};

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
