use super::{MetricEvent, MetricsSnapshot};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub type SharedMetrics = Arc<Mutex<MetricsSnapshot>>;

pub fn create_shared_metrics() -> SharedMetrics {
    Arc::new(Mutex::new(MetricsSnapshot::default()))
}

pub async fn run_aggregator(
    mut rx: mpsc::Receiver<MetricEvent>,
    shared: SharedMetrics,
) {
    let mut tx_timestamps: VecDeque<Instant> = VecDeque::new();
    let mut latencies: VecDeque<f64> = VecDeque::with_capacity(1000);
    let window = Duration::from_secs(10);

    let mut snapshot_interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            Some(event) = rx.recv() => {
                let mut snap = shared.lock().unwrap();
                match event {
                    MetricEvent::TransactionSent { agent_name, block_id, amount, latency } => {
                        snap.total_tx += 1;
                        tx_timestamps.push_back(Instant::now());
                        let lat_ms = latency.as_secs_f64() * 1000.0;
                        latencies.push_back(lat_ms);
                        if latencies.len() > 1000 { latencies.pop_front(); }

                        let stat = snap.agent_stats.entry(agent_name.clone()).or_default();
                        stat.name.clone_from(&agent_name);
                        stat.tx_count += 1;
                        stat.last_latency_ms = lat_ms;

                        let short_id = if block_id.len() > 12 { &block_id[..12] } else { &block_id };
                        snap.recent_events.push(format!(
                            "TX {} PMS [{}] -> block {}...", amount, agent_name, short_id
                        ));
                        if snap.recent_events.len() > 50 { snap.recent_events.remove(0); }
                    }
                    MetricEvent::AgentError { agent_name, error } => {
                        snap.total_errors += 1;
                        let stat = snap.agent_stats.entry(agent_name.clone()).or_default();
                        stat.name.clone_from(&agent_name);
                        stat.error_count += 1;

                        snap.recent_events.push(format!("ERR [{}]: {}", agent_name, error));
                        if snap.recent_events.len() > 50 { snap.recent_events.remove(0); }
                    }
                    MetricEvent::AgentFunded { agent_name, amount } => {
                        snap.recent_events.push(format!("FUND [{}]: {} PMS", agent_name, amount));
                        if snap.recent_events.len() > 50 { snap.recent_events.remove(0); }
                    }
                    MetricEvent::TipsCount(n) => { snap.tips_count = n; }
                    MetricEvent::SupplyUpdate { circulating, utxo_count } => {
                        snap.circulating_supply = circulating;
                        snap.utxo_count = utxo_count;
                    }
                    MetricEvent::BalanceUpdate { agent_name, balance } => {
                        let stat = snap.agent_stats.entry(agent_name.clone()).or_default();
                        stat.name.clone_from(&agent_name);
                        stat.balance = balance;
                    }
                    MetricEvent::GeminiDecision { agent_name, directive } => {
                        snap.recent_events.push(format!(
                            "AI [{}]: {}", agent_name, directive
                        ));
                        if snap.recent_events.len() > 50 { snap.recent_events.remove(0); }
                    }
                }
            }
            _ = snapshot_interval.tick() => {
                let now = Instant::now();
                while tx_timestamps.front().is_some_and(|t| now.duration_since(*t) > window) {
                    tx_timestamps.pop_front();
                }

                let mut snap = shared.lock().unwrap();
                let tps = tx_timestamps.len() as f64 / window.as_secs_f64();
                snap.tps = tps;
                snap.tps_history.push(tps);
                if snap.tps_history.len() > 120 { snap.tps_history.remove(0); }

                if !latencies.is_empty() {
                    let mut sorted: Vec<f64> = latencies.iter().copied().collect();
                    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let len = sorted.len();
                    snap.latency_p50_ms = sorted[len / 2];
                    snap.latency_p95_ms = sorted[((len as f64 * 0.95) as usize).min(len - 1)];
                    snap.latency_p99_ms = sorted[((len as f64 * 0.99) as usize).min(len - 1)];
                    let p50 = snap.latency_p50_ms;
                    snap.latency_history.push(p50);
                    if snap.latency_history.len() > 120 { snap.latency_history.remove(0); }
                }
            }
            else => break,
        }
    }
}
