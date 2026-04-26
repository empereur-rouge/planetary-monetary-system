//! Prometheus counters for the simulator's own behaviour.
//!
//! The engine already exposes `/metrics/all` — those numbers describe
//! what got through the validation pipeline. This module adds the
//! producer-side picture: what each agent group attempted, how many
//! requests succeeded vs. failed, why they failed, and how many cubes
//! were minted vs. burned. Pair the two surfaces in Grafana to spot
//! "spammer attempts: 50 RPS, engine accepts: 30 RPS, dropped: 20 RPS".
//!
//! Exposed via `web.rs` at `GET /metrics` on the dashboard port (9090).
//! No auth — this is the operator's local diagnostic, not a public
//! surface.

use once_cell::sync::Lazy;
use prometheus::{Encoder, IntCounterVec, TextEncoder};

/// Submitted transactions, broken down by the agent group that fired
/// them and the kind of operation. `kind` is one of:
///   - `pms_send`     : `wallet/send-simple` to a peer (PMS)
///   - `edn_send`     : `wallet/send-simple` with `asset_id=edenite`
///   - `cube_mint`    : NFT mint via the game engine
///   - `cube_burn`    : NFT burn batch (one tx batches N cubes)
///   - `coordinator_send` : the coordinator agent's bookkeeping send
pub static SIM_TX_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_simulator_tx_sent_total",
        "Successful transactions submitted by the simulator",
        &["agent_group", "kind"]
    )
    .unwrap()
});

/// Failed submissions. `reason` is a coarse bucket — the agent code
/// classifies the error before incrementing so dashboards can alert on
/// e.g. a sudden spike of `network_error`. Buckets:
///   - `http_4xx`        : client error (validation rejected, rate limit)
///   - `http_5xx`        : server error (engine bug)
///   - `network_error`   : reqwest couldn't reach the gateway
///   - `decode_error`    : response wasn't valid JSON
///   - `insufficient_funds` : agent's wallet didn't have enough PMS/EDN
///   - `other`           : everything else
pub static SIM_TX_FAILED: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_simulator_tx_failed_total",
        "Failed transactions submitted by the simulator",
        &["agent_group", "kind", "reason"]
    )
    .unwrap()
});

/// Cumulative cubes minted by the game engine (sum of mint successes
/// across all agents). Pair with `pms_simulator_cubes_burned_total` to
/// derive in-flight inventory.
pub static SIM_CUBES_MINTED: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_simulator_cubes_minted_total",
        "Cubes successfully minted by the simulator",
        &["agent_group"]
    )
    .unwrap()
});

/// Cumulative cubes burned. One burn batch counts the number of cubes
/// it contained — not the number of batches.
pub static SIM_CUBES_BURNED: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_simulator_cubes_burned_total",
        "Cubes successfully burned by the simulator",
        &["agent_group"]
    )
    .unwrap()
});

/// Burn batches (each batch = one tx, regardless of cube count).
pub static SIM_BURN_BATCHES: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "pms_simulator_burn_batches_total",
        "Burn batch transactions submitted by the simulator",
        &["agent_group"]
    )
    .unwrap()
});

/// Render the full simulator registry in Prometheus text format.
pub fn render() -> String {
    let mut buf = Vec::new();
    let encoder = TextEncoder::new();
    let mf = prometheus::gather();
    encoder.encode(&mf, &mut buf).ok();
    String::from_utf8(buf).unwrap_or_default()
}

/// Extract the agent group from the agent's name. The simulator names
/// agents like `casual-0`, `casual-1`, ..., `spammer-0`, ... so the
/// group is everything before the last `<sep><digits>` suffix where
/// `<sep>` is either `-` (current convention used by `funder.rs`) or
/// `_` (legacy convention from earlier code paths). Without this
/// trimming the Prometheus `agent_group` label has cardinality N (one
/// per agent) instead of the intended ~5 (one per group), which blows
/// up dashboard queries and storage.
pub fn group_of(agent_name: &str) -> &str {
    let idx = agent_name
        .rfind(|c: char| c == '-' || c == '_')
        .map(|i| (i, &agent_name[i + 1..]));
    if let Some((i, suffix)) = idx {
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return &agent_name[..i];
        }
    }
    agent_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_of_strips_numeric_suffix() {
        // Underscore separator (legacy).
        assert_eq!(group_of("casual_0"), "casual");
        assert_eq!(group_of("active_42"), "active");
        assert_eq!(group_of("spammer_4"), "spammer");
        // Hyphen separator (current funder convention).
        assert_eq!(group_of("casual-0"), "casual");
        assert_eq!(group_of("active-42"), "active");
        assert_eq!(group_of("adversarial-2"), "adversarial");
        // Non-numeric suffix → name returned as-is.
        assert_eq!(group_of("coordinator"), "coordinator");
        assert_eq!(group_of("agent_main"), "agent_main");
        assert_eq!(group_of("name-with-text"), "name-with-text");
    }

    #[test]
    fn render_is_non_empty_after_inc() {
        SIM_TX_SENT
            .with_label_values(&["test_group", "pms_send"])
            .inc();
        let out = render();
        assert!(out.contains("pms_simulator_tx_sent_total"));
        assert!(out.contains("test_group"));
    }
}
