//! `GET /admin/memory-profile` — cgroup memory breakdown for forensic
//! analysis (v0.7.29).
//!
//! On the testnet 14 GiB engine cgroup we observed `anon` climbing to
//! 9.6 GiB under load while the documented memtable cap is only 2.1 GiB
//! — leaving ~7.5 GiB unaccounted-for. The read-only safety valve
//! masks the symptom (arms before OOM kill) but doesn't tell us what
//! is actually allocating that memory.
//!
//! This endpoint exposes every category in
//! `/sys/fs/cgroup/memory.stat` (cgroup v2) plus the totals from
//! `memory.current` and `memory.max`, so an operator can correlate
//! allocation patterns with workload changes — without needing
//! `docker exec` + manual `cat` of cgroup files.
//!
//! Read-only on the host kernel state: doesn't write anything. Safe
//! to poll from a dashboard at any cadence. Goes into the
//! `admin_recovery` route group (no read-only gating).
//!
//! **Future work**: integrate `tikv-jemalloc-ctl` to expose
//! `stats.allocated`, `stats.active`, `stats.resident`, `stats.mapped`
//! so we can quantify jemalloc fragmentation specifically. Right now
//! we only see total `anon` from cgroup; we can't tell what fraction
//! is real working set vs allocator overhead.

use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde_json::{Map, Value, json};

/// `GET /admin/memory-profile` — returns the cgroup memory.stat
/// breakdown plus current / max as a JSON object. Linux + cgroup v2
/// only; on other platforms returns `503` with `{"error":"unsupported"}`.
///
/// Response shape:
/// ```json
/// {
///     "current_bytes": 4_169_527_296,
///     "max_bytes": 15_032_385_536,
///     "current_pct": 27.7,
///     "stat": {
///         "anon": 4041814016,
///         "file": 8441856,
///         "kernel": 34308096,
///         ...
///     },
///     "reclaimable_bytes": 110305280,
///     "irreclaimable_bytes": 4059221440,
///     "irreclaimable_pct": 27.0
/// }
/// ```
///
/// `current_pct` matches what `docker stats` displays.
/// `irreclaimable_pct` is what the read-only resource guard uses for
/// its memory watermark check (since v0.7.26): it subtracts file
/// cache + reclaimable slabs because those don't trigger OOM.
pub async fn admin_memory_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    let snapshot = match read_cgroup_v2_snapshot() {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "unsupported",
                    "detail": format!(
                        "Could not read cgroup v2 memory stats: {}. \
                         This endpoint requires Linux + cgroup v2 \
                         (Debian 12 + Docker 29.x is the production target).",
                        e
                    ),
                })),
            );
        }
    };

    (StatusCode::OK, Json(snapshot))
}

/// Read the cgroup v2 memory snapshot and assemble the JSON payload.
///
/// Returns `Err(String)` with a human-readable reason when any of the
/// three input files (`memory.current`, `memory.max`, `memory.stat`)
/// can't be read or parsed. The handler converts that into a 503 with
/// the same message — operators on macOS / non-Linux platforms get a
/// clear signal instead of a misleading 500.
fn read_cgroup_v2_snapshot() -> Result<Value, String> {
    let cur: u64 = std::fs::read_to_string("/sys/fs/cgroup/memory.current")
        .map_err(|e| format!("read memory.current: {}", e))?
        .trim()
        .parse()
        .map_err(|e| format!("parse memory.current: {}", e))?;

    let max_str = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .map_err(|e| format!("read memory.max: {}", e))?;
    let max_trim = max_str.trim();
    let max: Option<u64> = if max_trim == "max" {
        None
    } else {
        Some(
            max_trim
                .parse()
                .map_err(|e| format!("parse memory.max: {}", e))?,
        )
    };

    // Parse every key/value line in memory.stat into a flat map.
    let stat_text = std::fs::read_to_string("/sys/fs/cgroup/memory.stat")
        .map_err(|e| format!("read memory.stat: {}", e))?;
    let mut stat: Map<String, Value> = Map::new();
    for line in stat_text.lines() {
        let mut parts = line.split_whitespace();
        let key = match parts.next() {
            Some(k) => k.to_string(),
            None => continue,
        };
        let val: u64 = match parts.next().and_then(|v| v.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        stat.insert(key, json!(val));
    }

    // Reclaimable = file (page cache) + slab_reclaimable.
    // Irreclaimable = current - reclaimable. Matches the resource
    // guard's `read_cgroup_memory_pct` semantics.
    let file = stat.get("file").and_then(|v| v.as_u64()).unwrap_or(0);
    let slab_reclaimable = stat
        .get("slab_reclaimable")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reclaimable = file + slab_reclaimable;
    let irreclaimable = cur.saturating_sub(reclaimable);

    let mut out = Map::new();
    out.insert("current_bytes".into(), json!(cur));
    out.insert("max_bytes".into(), max.map(|m| json!(m)).unwrap_or(json!(null)));
    if let Some(m) = max {
        if m > 0 {
            out.insert(
                "current_pct".into(),
                json!((cur as f64 / m as f64) * 100.0),
            );
            out.insert(
                "irreclaimable_pct".into(),
                json!((irreclaimable as f64 / m as f64) * 100.0),
            );
        }
    }
    out.insert("reclaimable_bytes".into(), json!(reclaimable));
    out.insert("irreclaimable_bytes".into(), json!(irreclaimable));
    out.insert("stat".into(), Value::Object(stat));

    // Process-level fallback / cross-check via /proc/self/status —
    // helpful to compare cgroup totals against what the kernel sees
    // for THIS process specifically. Useful when the cgroup contains
    // more than one process (it shouldn't on our setup, but
    // defense-in-depth).
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        let mut proc_stat = Map::new();
        for line in status.lines() {
            // Lines look like "VmRSS:  <value> kB"
            for key in [
                "VmRSS",
                "VmPeak",
                "VmSize",
                "VmHWM",
                "VmData",
                "VmStk",
                "VmExe",
                "VmLib",
                "VmPTE",
                "VmSwap",
                "RssAnon",
                "RssFile",
                "RssShmem",
            ] {
                if let Some(rest) = line.strip_prefix(&format!("{}:", key)) {
                    let val_kb: u64 = rest
                        .trim()
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    proc_stat.insert(key.to_string(), json!(val_kb * 1024));
                }
            }
        }
        if !proc_stat.is_empty() {
            out.insert("proc_self_status_bytes".into(), Value::Object(proc_stat));
        }
    }

    Ok(Value::Object(out))
}
