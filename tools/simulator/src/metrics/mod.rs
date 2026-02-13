pub mod aggregator;

use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum MetricEvent {
    TransactionSent {
        agent_name: String,
        block_id: String,
        amount: String,
        latency: Duration,
    },
    AgentError {
        agent_name: String,
        error: String,
    },
    AgentFunded {
        agent_name: String,
        amount: String,
    },
    TipsCount(usize),
    SupplyUpdate {
        circulating: String,
        utxo_count: u64,
    },
    BalanceUpdate {
        agent_name: String,
        balance: String,
    },
    GeminiDecision {
        agent_name: String,
        directive: String,
    },
}

/// Snapshot of aggregated metrics for TUI rendering
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub tps: f64,
    pub total_tx: u64,
    pub total_errors: u64,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_p99_ms: f64,
    pub tips_count: usize,
    pub circulating_supply: String,
    pub utxo_count: u64,
    pub agent_stats: HashMap<String, AgentStat>,
    pub tps_history: Vec<f64>,
    pub latency_history: Vec<f64>,
    pub recent_events: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentStat {
    pub name: String,
    pub tx_count: u64,
    pub error_count: u64,
    pub last_latency_ms: f64,
    pub balance: String,
}
