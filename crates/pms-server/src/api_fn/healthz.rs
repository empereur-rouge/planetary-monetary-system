//! Enriched `/healthz` endpoint (audit finding H-healthz, v0.7.4).
//!
//! The pre-0.7.4 endpoint just returned `200 "ok"` based on the `_ready`
//! atomic flag, which only flipped once at boot — it told monitoring
//! "the server has started", not "the server is functional right now".
//! That's the gap this module fills: the new handler returns a JSON
//! status with four concrete checks an operator can act on:
//!
//!   - `rocksdb_writable`    — the storage handle is open and queryable.
//!   - `persist_queue_depth` — the background persist channel isn't
//!                             back-pressuring (queue depth < high-water
//!                             threshold from `[health]` config).
//!   - `last_block_age_ms`   — the most recent block was persisted
//!                             recently (configurable via
//!                             `max_last_block_age_seconds`).
//!   - `disk_free_percent`   — the filesystem holding RocksDB has more
//!                             than `min_disk_free_percent` free.
//!
//! HTTP code:
//!   - `200` — every check passed (`status: "ok"`).
//!   - `503` — at least one check failed (`status: "degraded"` or
//!             `"fail"`). Body still contains the per-check breakdown
//!             so monitoring can show *which* one tripped.
//!
//! `/livez` keeps its trivial behaviour (just "is the process alive?")
//! so a Kubernetes liveness probe doesn't restart the pod the moment
//! the persist queue spikes.

use crate::api::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use pms_storage::DagStorage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: &'static str, // "ok" | "degraded" | "fail"
    pub uptime_ready: bool,
    pub checks: Vec<HealthCheck>,
}

/// Handler for `GET /healthz`.
pub async fn enriched_healthz(State(state): State<AppState>) -> impl IntoResponse {
    use std::sync::atomic::Ordering;

    let ready = state._ready.load(Ordering::Relaxed);
    if !ready {
        // Still booting — don't even run the checks.
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "starting",
                "uptime_ready": false,
                "checks": [],
            })),
        )
            .into_response();
    }

    let mut checks: Vec<HealthCheck> = Vec::with_capacity(4);

    // 1. rocksdb_writable — read a cheap, O(1) property to prove the
    //    handle is open and responding. We don't actually write because
    //    that would pollute the DB with a marker every health-check.
    {
        let detail = match state.store.block_count_estimate().await {
            Ok(n) => json!({"block_count_estimate": n}),
            Err(e) => json!({"error": format!("{e}")}),
        };
        let ok = detail.get("error").is_none();
        checks.push(HealthCheck {
            name: "rocksdb_writable",
            ok,
            detail,
        });
    }

    // 2. persist_queue_depth — pull from the adapter (None if the mock
    //    backend doesn't expose it).
    {
        let high_water = state.settings.health.persist_queue_high_water.clamp(0.0, 1.0);
        let (ok, detail) = match state.srv.adapter_arc().persist_queue_depth() {
            Some((depth, capacity)) => {
                let usage = if capacity == 0 {
                    0.0
                } else {
                    depth as f64 / capacity as f64
                };
                let healthy = usage < high_water;
                (
                    healthy,
                    json!({
                        "depth": depth,
                        "capacity": capacity,
                        "usage": format!("{:.2}", usage),
                        "high_water": format!("{:.2}", high_water),
                    }),
                )
            }
            None => (
                true, // no telemetry → don't fail health on missing data
                json!({"unavailable": "adapter does not expose persist queue depth"}),
            ),
        };
        checks.push(HealthCheck {
            name: "persist_queue_depth",
            ok,
            detail,
        });
    }

    // 3. last_block_age — pull the newest block's timestamp from the
    //    by-time index. Skip the check if the configured threshold is
    //    `0` (operator opt-out for tiny / paused testnets).
    {
        let max_age_ms: u64 = state.settings.health.max_last_block_age_seconds.saturating_mul(1000);
        let detail_and_ok = match state
            .store
            .recent_ids_by_time(None, None, 1)
            .await
        {
            Ok((ids, _next)) if !ids.is_empty() => {
                let id = ids[0].clone();
                match state.store.block_ts_ms(&id).await {
                    Ok(Some(ts_ms)) => {
                        let now_ms = now_ms();
                        let age_ms = now_ms.saturating_sub(ts_ms).max(0) as u64;
                        let ok = max_age_ms == 0 || age_ms <= max_age_ms;
                        (
                            ok,
                            json!({
                                "block_id": &id[..16.min(id.len())],
                                "ts_ms": ts_ms,
                                "age_ms": age_ms,
                                "max_age_ms": max_age_ms,
                            }),
                        )
                    }
                    Ok(None) => (
                        true,
                        json!({"unavailable": "newest block has no timestamp index entry"}),
                    ),
                    Err(e) => (false, json!({"error": format!("{e}")})),
                }
            }
            Ok(_) => (
                // Empty DAG: not failing — a node that just booted on a
                // fresh DB is healthy, just empty.
                true,
                json!({"empty_dag": true}),
            ),
            Err(e) => (false, json!({"error": format!("{e}")})),
        };
        checks.push(HealthCheck {
            name: "last_block_age",
            ok: detail_and_ok.0,
            detail: detail_and_ok.1,
        });
    }

    // 4. disk_free_percent — best-effort check on the FS holding the
    //    RocksDB directory. Falls back to "unavailable" when statvfs
    //    isn't supported (non-Unix) or fails.
    {
        let min_free = state.settings.health.min_disk_free_percent.max(0.0);
        let path = std::path::PathBuf::from(&state.settings.rocks.path);
        let (ok, detail) = match disk_free_percent(&path) {
            Some(pct) => {
                let healthy = pct >= min_free;
                (
                    healthy,
                    json!({
                        "free_percent": format!("{:.1}", pct),
                        "min_free_percent": min_free,
                        "path": path.display().to_string(),
                    }),
                )
            }
            None => (
                true,
                json!({
                    "unavailable": "statvfs not supported or failed",
                    "path": path.display().to_string(),
                }),
            ),
        };
        checks.push(HealthCheck {
            name: "disk_free_percent",
            ok,
            detail,
        });
    }

    let any_failed = checks.iter().any(|c| !c.ok);
    let status = if any_failed { "degraded" } else { "ok" };
    let http = if any_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    let response = HealthResponse {
        status,
        uptime_ready: ready,
        checks,
    };

    (http, Json(serde_json::to_value(&response).unwrap())).into_response()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Returns the percentage of free space on the filesystem holding `path`,
/// or `None` if we can't determine it (not Unix, statvfs failure, etc.).
#[cfg(unix)]
fn disk_free_percent(path: &std::path::Path) -> Option<f64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // statvfs requires the path to exist; if it doesn't (test scenarios,
    // first boot before RocksDB has been created), walk up to the first
    // existing parent so we still get filesystem info from the real
    // volume. Falls back to "." (current working dir) if we run out of
    // ancestors — that path always exists for a running process.
    let mut probe = path.to_path_buf();
    loop {
        if probe.as_os_str().is_empty() {
            probe = std::path::PathBuf::from(".");
            break;
        }
        if probe.exists() {
            break;
        }
        match probe.parent() {
            Some(p) if p != probe.as_path() => probe = p.to_path_buf(),
            _ => {
                probe = std::path::PathBuf::from(".");
                break;
            }
        }
    }

    let cpath = CString::new(probe.as_os_str().as_bytes()).ok()?;
    // SAFETY: cpath is a valid NUL-terminated path; statvfs writes into
    // a mut buf we own; we check rc.
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut buf) };
    if rc != 0 {
        return None;
    }
    if buf.f_blocks == 0 {
        return None;
    }
    let total = buf.f_blocks as f64;
    let avail = buf.f_bavail as f64;
    Some(avail / total * 100.0)
}

#[cfg(not(unix))]
fn disk_free_percent(_path: &std::path::Path) -> Option<f64> {
    None
}
