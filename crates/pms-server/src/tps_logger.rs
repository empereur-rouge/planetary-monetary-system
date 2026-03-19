//! TPS Logger — Periodic throughput recording for production diagnostics.
//!
//! Spawns a background task that writes a JSONL line every 10 minutes to
//! `{rocks.path}/tps_log.jsonl`. Each deployment gets a unique UUID so
//! operators (and AI assistants reviewing logs) can distinguish restarts
//! from sustained runs.
//!
//! ## Log Format (one JSON object per line)
//!
//! ```json
//! {
//!   "ts": "2026-03-19T16:50:00Z",
//!   "epoch_ms": 1742403000000,
//!   "deployment_id": "a1b2c3d4-...",
//!   "ledger": "main",
//!   "tps_60s": 1842.5,
//!   "block_count": 15234567,
//!   "circulating_supply": "1000000.00000000",
//!   "total_burned": "42000.12345678",
//!   "node_pk": "04a1b2c3d4e5f6g7",
//!   "uptime_min": 30
//! }
//! ```

use crate::api::AppState;
use pms_storage::DagStorage;
use pms_wallet::SignerBackend;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::AsyncWriteExt;

/// Interval between TPS log entries (10 minutes).
const LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// TPS log entry written as a single JSON line.
#[derive(Debug, Serialize)]
struct TpsLogEntry {
    /// ISO 8601 timestamp.
    ts: String,
    /// Unix epoch in milliseconds.
    epoch_ms: u64,
    /// Unique ID for this deployment (generated at spawn time).
    deployment_id: String,
    /// Ledger ID ("main" or custom).
    ledger: String,
    /// Current TPS measured over the last 60 seconds.
    tps_60s: f64,
    /// Approximate block count in RocksDB.
    block_count: u64,
    /// Circulating supply of native PMS token.
    circulating_supply: String,
    /// Total PMS burned (cumulative).
    total_burned: String,
    /// Node public key (first 16 chars for identification).
    node_pk: String,
    /// Minutes since this logger was started.
    uptime_min: u64,
}

/// Resolves the TPS log file path: `{rocks_path}/tps_log.jsonl`.
///
/// Creates the parent directory if it doesn't exist.
fn resolve_log_path(rocks_path: &str) -> PathBuf {
    let parent = Path::new(rocks_path);
    // Place the log file next to the RocksDB directory, not inside it.
    // If rocks_path = "data/rocks", log goes to "data/tps_log.jsonl".
    if let Some(data_dir) = parent.parent() {
        if !data_dir.as_os_str().is_empty() {
            return data_dir.join("tps_log.jsonl");
        }
    }
    // Fallback: same directory level
    PathBuf::from("tps_log.jsonl")
}

/// Spawns a background task that logs TPS metrics every 10 minutes.
///
/// The log file is append-only JSONL at `{data_dir}/tps_log.jsonl`.
/// Each line is self-contained and includes a `deployment_id` UUID so
/// operators can distinguish between restarts.
///
/// First entry is written ~10 seconds after startup for immediate
/// verification, then every 10 minutes thereafter.
///
/// # Arguments
/// * `state` — Main AppState (for TpsTracker, store, adapter access).
/// * `rocks_path` — The `rocks.path` config value, used to locate the log file.
pub fn spawn_tps_logger(state: AppState, rocks_path: String) {
    let deployment_id = generate_deployment_id();
    let log_path = resolve_log_path(&rocks_path);
    let started_at = Instant::now();

    tracing::info!(
        deployment_id = %deployment_id,
        log_path = %log_path.display(),
        "TPS logger started (interval: {}min)",
        LOG_INTERVAL.as_secs() / 60
    );

    tokio::spawn(async move {
        // First entry after 10s so operators can immediately verify the logger works.
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        loop {
            let entry = collect_entry(&state, &deployment_id, &started_at).await;

            if let Err(e) = append_entry(&log_path, &entry).await {
                tracing::warn!(
                    error = %e,
                    "TPS logger: failed to write entry"
                );
            } else {
                tracing::info!(
                    tps = entry.tps_60s,
                    blocks = entry.block_count,
                    uptime_min = entry.uptime_min,
                    deployment = %deployment_id,
                    "TPS log entry written"
                );
            }

            // Subsequent entries every 10 minutes.
            tokio::time::sleep(LOG_INTERVAL).await;
        }
    });
}

/// Collects a single TPS log entry from the current server state.
async fn collect_entry(
    state: &AppState,
    deployment_id: &str,
    started_at: &Instant,
) -> TpsLogEntry {
    let now = std::time::SystemTime::now();
    let epoch_ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // ISO 8601 timestamp (UTC)
    let ts = {
        let secs = epoch_ms / 1000;
        let d = chrono_lite_utc(secs);
        d
    };

    let tps_60s = state.tps_tracker.current_tps();
    let block_count = state.store.block_count_estimate().await.unwrap_or(0);
    let (circulating_supply, _) = state.srv.adapter_arc().circulating_supply().await;
    let total_burned = state
        .store
        .get_total_burned()
        .unwrap_or(rust_decimal::Decimal::ZERO);
    let node_pk = state.node_wallet.encoded_public_key();
    let uptime_min = started_at.elapsed().as_secs() / 60;

    TpsLogEntry {
        ts,
        epoch_ms,
        deployment_id: deployment_id.to_string(),
        ledger: state.ledger_id.clone(),
        tps_60s,
        block_count,
        circulating_supply: circulating_supply.to_string(),
        total_burned: total_burned.to_string(),
        node_pk: node_pk[..16.min(node_pk.len())].to_string(),
        uptime_min,
    }
}

/// Appends a single JSON line to the log file.
async fn append_entry(path: &Path, entry: &TpsLogEntry) -> std::io::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut line = serde_json::to_string(entry).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    line.push('\n');

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;

    file.write_all(line.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

/// Generates a short unique deployment ID.
///
/// Format: 8 hex chars from timestamp + 8 random hex chars.
/// Example: `6604a1b2-3c4d5e6f`
fn generate_deployment_id() -> String {
    use std::time::SystemTime;

    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple random bytes (no uuid crate dependency needed)
    let random: u32 = {
        // Use timestamp nanoseconds + process ID as entropy
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let pid = std::process::id();
        nanos.wrapping_mul(pid).wrapping_add(0x9e3779b9)
    };

    format!("{:08x}-{:08x}", ts as u32, random)
}

/// Minimal UTC timestamp formatter (no chrono dependency).
///
/// Produces ISO 8601 format: `2026-03-19T16:50:00Z`
fn chrono_lite_utc(epoch_secs: u64) -> String {
    // Days since 1970-01-01
    let days = (epoch_secs / 86400) as i64;
    let remaining = epoch_secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;

    // Civil date from days since epoch (algorithm from Howard Hinnant)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hours, minutes, seconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deployment_id_format() {
        let id = generate_deployment_id();
        println!("deployment_id: {}", id);
        assert_eq!(id.len(), 17); // 8 + '-' + 8
        assert_eq!(id.as_bytes()[8], b'-');
    }

    #[test]
    fn test_chrono_lite_utc() {
        // 2026-03-19T16:50:00Z = 1774043400 (approx)
        let ts = chrono_lite_utc(0);
        println!("epoch 0: {}", ts);
        assert_eq!(ts, "1970-01-01T00:00:00Z");

        let ts2 = chrono_lite_utc(1710000000);
        println!("1710000000: {}", ts2);
        // Should be 2024-03-09T16:00:00Z
        assert!(ts2.starts_with("2024-03-"));
    }

    #[test]
    fn test_resolve_log_path() {
        let p = resolve_log_path("data/rocks");
        println!("log path: {}", p.display());
        assert_eq!(p, PathBuf::from("data/tps_log.jsonl"));

        let p2 = resolve_log_path("/var/pms/db");
        println!("log path 2: {}", p2.display());
        assert_eq!(p2, PathBuf::from("/var/pms/tps_log.jsonl"));
    }
}
