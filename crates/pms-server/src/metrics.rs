// pms-server/src/metrics.rs
use once_cell::sync::Lazy;
use prometheus::{
    CounterVec, Encoder, HistogramVec, TextEncoder, register_counter_vec, register_histogram_vec,
};

pub static _NET_MSGS_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!("net_msgs_total", "Messages réseau par type", &["type"]).unwrap()
});
pub static _PERSIST_OK_TOTAL: Lazy<CounterVec> =
    Lazy::new(|| register_counter_vec!("persist_ok_total", "Persist OK", &["kind"]).unwrap());
pub static _PERSIST_ERR_TOTAL: Lazy<CounterVec> =
    Lazy::new(|| register_counter_vec!("persist_err_total", "Persist erreurs", &["kind"]).unwrap());
pub static _PERSIST_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!("persist_latency_seconds", "Latence persistance", &["kind"]).unwrap()
});

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

#[allow(dead_code)]
pub static BLOCKS_BROADCAST: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "pms_blocks_broadcast_total",
        "Blocs broadcastés sur le réseau"
    )
    .unwrap()
});

pub static PMS_BLOCKS_TOTAL: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "pms_blocks_total",
        "Nombre total de blocs connus (DAG size)"
    )
    .unwrap()
});
#[allow(dead_code)]
pub static PMS_TIPS_COUNT: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        // Gauge car ça peut monter et descendre
        "pms_tips_count",
        "Nombre de tips actifs dans le DAG"
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
