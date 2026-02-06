// pms-server/src/metrics.rs
use once_cell::sync::Lazy;
use prometheus::{Encoder, TextEncoder};

pub static BLOCKS_REJECTED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "pms_blocks_rejected_total",
        "Blocs rejetés lors de la persistance"
    )
    .unwrap()
});

use prometheus::{IntCounter, IntGauge, register_int_counter, register_int_gauge};

pub static BLOCKS_PERSISTED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("pms_blocks_persisted_total", "Blocs validés et persistés").unwrap()
});

pub static PMS_BLOCKS_TOTAL: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "pms_blocks_total",
        "Nombre total de blocs connus (DAG size)"
    )
    .unwrap()
});

pub fn render() -> String {
    let mut buf = Vec::new();
    let encoder = TextEncoder::new();
    let mf = prometheus::gather();
    encoder.encode(&mf, &mut buf).ok();
    String::from_utf8(buf).unwrap_or_default()
}
