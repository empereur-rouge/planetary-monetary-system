use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use pms_config::ConfigUpdate;
use pms_storage::config_store::ConfigStorage;
use serde_json::json;

/// Simple endpoint pour tester le token admin.
pub async fn admin_ping(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "role": "admin",
            "network": state._cfg.network.network_id
        })),
    )
}

/// Endpoint placeholder pour une future action de maintenance (compaction, flush, etc.).
pub async fn admin_compact(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "unauthorized"
            })),
        );
    }

    // Step 1: Flush WAL
    if let Err(e) = state.store.flush_wal().await {
        tracing::error!("admin compact: flush_wal failed: {e:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("flush_wal failed: {e}") })),
        );
    }

    // Step 2: Trigger compaction
    if let Err(e) = state.store.compact_all().await {
        tracing::error!("admin compact: compaction failed: {e:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("compaction failed: {e}") })),
        );
    }

    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "action": "compact",
            "message": "flush_wal + compact_all completed"
        })),
    )
}

/// POST /admin/reindex-activity
///
/// Rebuild `addr_activity` and `addr_type_activity` indexes by scanning all
/// stored blocks. Required after deploying the encrypted-activity fix on a
/// node that already has historical blocks without index entries.
///
/// Only Plain payloads are indexed (Encrypted payloads need the recipient's
/// private key and are handled at creation time by the coordinator).
pub async fn admin_reindex_activity(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    tracing::info!("[ADMIN] Reindex activity requested");

    match state.store.reindex_all_activity() {
        Ok(stats) => {
            tracing::info!(
                "[ADMIN] Reindex complete: {} indexed, {} encrypted skipped, {} total",
                stats.indexed,
                stats.skipped_encrypted,
                stats.total_blocks,
            );
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "action": "reindex-activity",
                    "stats": stats
                })),
            )
        }
        Err(e) => {
            tracing::error!("[ADMIN] Reindex failed: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("reindex failed: {e}") })),
            )
        }
    }
}

/// POST /admin/reindex-activity-items
///
/// Rebuild the `activity_items` CF by scanning all stored blocks and
/// pre-computing per-address activity items. This backfills the fast-path
/// data for blocks that were created before the pre-computation optimization.
///
/// For TxUtxo blocks, sender resolution is best-effort (UTXOs may already be
/// spent), so the fallback classify path is still used at read time for those.
pub async fn admin_reindex_activity_items(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    tracing::info!("[ADMIN] Reindex activity_items requested");

    match state.store.reindex_all_activity_items() {
        Ok(stats) => {
            tracing::info!(
                "[ADMIN] Reindex activity_items complete: {} indexed, {} encrypted skipped, {} total",
                stats.indexed,
                stats.skipped_encrypted,
                stats.total_blocks,
            );
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "action": "reindex-activity-items",
                    "stats": stats
                })),
            )
        }
        Err(e) => {
            tracing::error!("[ADMIN] Reindex activity_items failed: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("reindex failed: {e}") })),
            )
        }
    }
}

/// POST /admin/rebuild-tips
///
/// Curative reconcile of the RocksDB `tips` CF (audit finding H3, item 6,
/// v0.7.4). The 0.7.3 fix to `trim_tips` evicts zombies (entries with
/// `children_count > 0` that lingered after a missed `remove_tip`), but
/// it never **adds** anything — so a tip that's missing because of an
/// older crash between `append_block_atomic` and `add_tip` stays missing
/// forever. This endpoint walks the recent window of `by_time` and adds
/// any block whose `children_count == 0` is absent from `tips`.
///
/// Body (optional): `{ "scan_limit": <usize> }`. Defaults to 8 × the
/// configured `tip_limit` so a fresh production node only touches a few
/// dozen entries. Pass `0` to scan every block (only safe on small dev
/// DBs).
pub async fn admin_rebuild_tips(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    let scan_limit = body
        .as_ref()
        .and_then(|Json(v)| v.get("scan_limit"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        // Default: 8× tip_limit, with a sane floor so brand-new nodes
        // (tip_limit small / unset) still scan enough recent blocks to
        // matter. The intent of the cap is "only the recent window";
        // setting it to 0 explicitly opts into a full scan.
        .unwrap_or_else(|| state.settings.rocks.tip_limit.saturating_mul(8).max(64));

    tracing::info!(
        target = "rocks_tips",
        scan_limit,
        "[ADMIN] rebuild_tips_from_children_count requested"
    );

    let store = state.store.clone();
    let result =
        tokio::task::spawn_blocking(move || store.rebuild_tips_from_children_count(scan_limit))
            .await;

    match result {
        Ok(Ok((scanned, added))) => {
            tracing::info!(
                target = "rocks_tips",
                scanned,
                added,
                "[ADMIN] rebuild_tips complete"
            );
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "action": "rebuild-tips",
                    "scan_limit": scan_limit,
                    "scanned": scanned,
                    "added": added,
                })),
            )
        }
        Ok(Err(e)) => {
            tracing::error!(target = "rocks_tips", error = %e, "rebuild_tips failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("rebuild_tips failed: {e}") })),
            )
        }
        Err(e) => {
            tracing::error!(target = "rocks_tips", error = %e, "rebuild_tips task panicked");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("rebuild_tips task panicked: {e}") })),
            )
        }
    }
}

/// POST /admin/purge-activity
///
/// Manual trigger for activity-index retention. Body:
/// `{"before_days": <u64>}` (mandatory) — entries older than that are
/// deleted from `addr_activity`, `addr_type_activity`, and
/// `activity_items`. The corresponding background task already runs
/// daily when `[health].activity_retention_days` is set; this endpoint
/// lets an operator purge ad-hoc with a different cutoff (e.g. before
/// a backup, before a disk-full incident). Audit follow-up to v0.7.4.
pub async fn admin_purge_activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }
    let Some(before_days) = body.get("before_days").and_then(|v| v.as_u64()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing or invalid `before_days` (u64)" })),
        );
    };

    let cutoff_ms = compute_cutoff_ms(before_days);
    tracing::info!(
        target = "activity_retention",
        before_days,
        cutoff_ms,
        "[ADMIN] purge_activity_before requested"
    );

    let store = state.store.clone();
    let result = tokio::task::spawn_blocking(move || store.purge_activity_before(cutoff_ms)).await;
    match result {
        Ok(Ok(stats)) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "action": "purge-activity",
                "before_days": before_days,
                "stats": stats,
            })),
        ),
        Ok(Err(e)) => {
            tracing::error!(target = "activity_retention", error = %e, "purge failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("purge failed: {e}") })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("purge task panicked: {e}") })),
        ),
    }
}

/// POST /admin/purge-compliance-log
///
/// Operator-only. Audit follow-up to v0.7.4. The compliance log holds
/// freeze/seize/reverse actions and is regulatory data — there is NO
/// background task that auto-purges it. This endpoint exists so the
/// operator can prune the log AFTER an external archive step
/// (regulatory retention windows are typically 5+ years; you do not
/// want to discover during an audit that the log is gone). Body:
/// `{"before_days": <u64>}`.
pub async fn admin_purge_compliance_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }
    let Some(before_days) = body.get("before_days").and_then(|v| v.as_u64()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing or invalid `before_days` (u64)" })),
        );
    };

    let cutoff_ms = compute_cutoff_ms(before_days);
    tracing::warn!(
        target = "compliance_retention",
        before_days,
        cutoff_ms,
        "[ADMIN] purge_compliance_log_before requested — REGULATORY DATA WILL BE REMOVED"
    );

    let store = state.store.clone();
    let result =
        tokio::task::spawn_blocking(move || store.purge_compliance_log_before(cutoff_ms)).await;
    match result {
        Ok(Ok(stats)) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "action": "purge-compliance-log",
                "before_days": before_days,
                "stats": stats,
            })),
        ),
        Ok(Err(e)) => {
            tracing::error!(target = "compliance_retention", error = %e, "purge failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("purge failed: {e}") })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("purge task panicked: {e}") })),
        ),
    }
}

/// GET /admin/rocksdb-stats
///
/// Read-only diagnostic snapshot of the RocksDB properties most useful
/// when investigating write-path slowdowns. Used by the TPS-degradation
/// profile test to correlate TPS drops with compaction / write-stall
/// signals. Cheap (each `property_value` is O(1)) — safe to poll every
/// few seconds in production for an ops dashboard.
///
/// Surfaced fields (all best-effort; missing properties return `null`):
///
///   * `num_files_at_level0` — primary write-stall signal. When this
///     approaches the configured `level0_slowdown_writes_trigger` (default
///     20) the engine throttles writes; at `level0_stop_writes_trigger`
///     (default 36) it stops writing entirely.
///   * `compaction_pending` — `1` when at least one compaction is
///     queued. Sustained `1` means the compactor isn't keeping up.
///   * `is_write_stopped` — `1` when writes are completely halted (the
///     downstream effect of the L0-stop trigger).
///   * `actual_delayed_write_rate` — current write throttle in
///     bytes/sec when the engine is in slowdown mode.
///   * `estimate_num_keys` — O(1) estimate of the `idx_blocks` size.
///   * `mem_table_flush_pending` — `1` if a memtable flush is queued.
///   * `num_running_compactions` / `num_running_flushes` — current
///     parallelism in the background.
pub async fn admin_rocksdb_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    let store = state.store.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        // String-typed properties (parse to u64 where applicable).
        let read_str = |name: &str| -> Option<String> {
            store.db.property_value(name).ok().flatten()
        };
        let read_u64 = |name: &str| -> Option<u64> {
            read_str(name).and_then(|v| v.trim().parse::<u64>().ok())
        };
        let read_bool_as_u64 = |name: &str| -> Option<u64> { read_u64(name) };

        // Per-CF L0 file count for `idx_blocks` (the hot CF).
        let cf_idx = store.cf("idx_blocks");
        let l0_idx_blocks = store
            .db
            .property_value_cf(&cf_idx, "rocksdb.num-files-at-level0")
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<u64>().ok());

        // Cumulative sub-stage timings of `append_blocks_batch`. Profiling
        // diff between two scrapes tells us which sub-stage owns the
        // per-block consumer cost (write_cf vs build vs LSM I/O).
        use std::sync::atomic::Ordering;
        let append_dedup = store.append_us_dedup.load(Ordering::Relaxed);
        let append_build = store.append_us_build.load(Ordering::Relaxed);
        let append_write = store.append_us_write.load(Ordering::Relaxed);
        let append_trim = store.append_us_trim.load(Ordering::Relaxed);
        let bloom_skips = store.bloom_skips.load(Ordering::Relaxed);
        let bloom_hits = store.bloom_hits.load(Ordering::Relaxed);
        let (bloom_front, bloom_back, bloom_capacity, bloom_warmed) =
            store.bloom_filter_status();

        json!({
            "num_files_at_level0": read_u64("rocksdb.num-files-at-level0"),
            "num_files_at_level0_idx_blocks": l0_idx_blocks,
            "compaction_pending": read_bool_as_u64("rocksdb.compaction-pending"),
            "is_write_stopped": read_bool_as_u64("rocksdb.is-write-stopped"),
            "actual_delayed_write_rate": read_u64("rocksdb.actual-delayed-write-rate"),
            "mem_table_flush_pending": read_bool_as_u64("rocksdb.mem-table-flush-pending"),
            "num_running_compactions": read_u64("rocksdb.num-running-compactions"),
            "num_running_flushes": read_u64("rocksdb.num-running-flushes"),
            "estimate_num_keys": read_u64("rocksdb.estimate-num-keys"),
            "estimate_live_data_size": read_u64("rocksdb.estimate-live-data-size"),
            "size_all_mem_tables": read_u64("rocksdb.size-all-mem-tables"),
            "append_us_dedup": append_dedup,
            "append_us_build": append_build,
            "append_us_write": append_write,
            "append_us_trim": append_trim,
            "bloom_skips_total": bloom_skips,
            "bloom_hits_total": bloom_hits,
            "bloom_front_inserted": bloom_front,
            "bloom_back_inserted": bloom_back,
            "bloom_capacity_per_segment": bloom_capacity,
            "bloom_warmed": bloom_warmed,
        })
    })
    .await;

    match snapshot {
        Ok(stats) => (StatusCode::OK, Json(stats)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("rocksdb-stats task panicked: {e}") })),
        ),
    }
}

/// Wall-clock - days, in milliseconds.
fn compute_cutoff_ms(before_days: u64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    now.saturating_sub(before_days.saturating_mul(86_400_000) as i64)
}

// ═══════════════════════════════════════════════════════════════════════════
// ADMIN CONFIG API - Hot-Swap de la RuntimeConfig
// ═══════════════════════════════════════════════════════════════════════════

/// GET /admin/config
///
/// Récupère la configuration runtime actuelle.
/// Nécessite un token admin valide.
///
/// # Response
/// ```json
/// {
///   "fee_rate_bps": 300,
///   "base_fee": "0.0000001",
///   "coordinator_fee_bps": 6700,
///   "treasury_fee_bps": 3300,
///   "min_pow_bits": 8,
///   "max_mint_per_block": 1000000,
///   "mint_enabled": true,
///   "updated_at_block": "...",
///   "updated_at_timestamp": 1234567890
/// }
/// ```
pub async fn admin_get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Vérification du token admin
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    // Récupérer la config depuis RocksDB
    match state.store.get_runtime_config() {
        Ok(config) => {
            tracing::info!("[ADMIN] Config retrieved successfully");
            (StatusCode::OK, Json(json!(config)))
        }
        Err(e) => {
            tracing::error!("[ADMIN] Failed to get config: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to retrieve config: {}", e) })),
            )
        }
    }
}

/// POST /admin/config
///
/// Modifie un ou plusieurs paramètres de la RuntimeConfig.
/// Nécessite un token admin valide.
///
/// # Request Body
/// Un `ConfigUpdate` JSON, par exemple:
/// - `{"SetFeeRate": {"bps": 300}}`
/// - `{"SetMintEnabled": {"enabled": false}}`
/// - `{"BatchUpdate": [{"SetFeeRate": {"bps": 300}}, {"SetMinPow": {"bits": 10}}]}`
///
/// # Response
/// La nouvelle configuration après application de la mise à jour.
pub async fn admin_update_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(update): Json<ConfigUpdate>,
) -> axum::response::Response {
    // Vérification du token admin
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
    }

    // GOUVERNANCE (plan §4) — un changement de config ne s'applique PLUS
    // instantanément hors-DAG. Il passe par le processus gouverné : on forge un
    // `GovernanceProposal` (ancré DAG, timelocké). Le palier est AUTO-ASSIGNÉ au
    // minimum requis pour ce paramètre (table §2). Asymétrie tighten/loosen :
    // - resserrage (couper/réduire le mint, baisser un plafond) ⇒ timelock nul ⇒
    //   on enacte IMMÉDIATEMENT (UX instantanée préservée, mais désormais ancrée
    //   dans le DAG, pas un write synthétique) ;
    // - desserrage ⇒ proposition timelockée, auto-enactée à l'expiration (ou via
    //   `POST /admin/governance/enact/{id}`). C'est la fin du contournement qui
    //   rendait le timelock sans effet.
    let tier = pms_config::min_tier(&update);
    let desc = update.description();
    tracing::warn!("[ADMIN] Config change via governance ({}): {}", tier.as_str(), desc);

    let out = match crate::api_fn::governance::do_propose(
        &state,
        update,
        tier,
        "admin_update_config".to_string(),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };

    if out.instant {
        // Resserrage : enact immédiat (timelock nul).
        match crate::api_fn::governance::do_enact(
            &state,
            &out.proposal_id,
            "admin_update_config (instant tighten)".to_string(),
        )
        .await
        {
            Ok(enact_block_id) => {
                let new_config = state.store.get_runtime_config().ok();
                tracing::info!("[ADMIN] Config applied instantly (tighten): {}", desc);
                (
                    StatusCode::OK,
                    Json(json!({
                        "status": "applied",
                        "mode": "instant (tighten)",
                        "update_applied": desc,
                        "tier": tier.as_str(),
                        "proposal_id": out.proposal_id,
                        "proposal_block_id": out.block_id,
                        "enact_block_id": enact_block_id,
                        "config": new_config,
                    })),
                )
                    .into_response()
            }
            Err(e) => e.into_response(),
        }
    } else {
        // Desserrage : proposition timelockée.
        tracing::info!(
            "[ADMIN] Config change proposed (timelocked until {}): {}",
            out.enact_after_ms,
            desc
        );
        (
            StatusCode::ACCEPTED,
            Json(json!({
                "status": "proposed",
                "mode": "timelocked (loosen)",
                "update": desc,
                "tier": tier.as_str(),
                "proposal_id": out.proposal_id,
                "proposal_block_id": out.block_id,
                "announced_at_ms": out.announced_at_ms,
                "enact_after_ms": out.enact_after_ms,
                "message": "change is timelocked under governance; it auto-enacts when the timelock elapses, or POST /admin/governance/enact/{proposal_id}",
            })),
        )
            .into_response()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Read-only mode operator controls (v0.7.23)
// ═══════════════════════════════════════════════════════════════════════════
//
// These three endpoints expose the read-only flag to operators:
//
//   - GET  /admin/read-only/status  — current flag state and reason
//   - POST /admin/read-only/arm     — manually flip into read-only (Manual reason)
//   - POST /admin/read-only/disarm  — clear the flag (works on auto- AND manual-armed)
//
// `Manual` arms are intentionally NOT auto-cleared by the resource-guard
// task — the operator owns the lifecycle so a maintenance window can hold
// the engine in a known state without the watcher fighting them. Calling
// `disarm` from `Manual` returns the engine to normal; if real pressure
// re-asserts itself the watcher will re-arm with the appropriate reason
// on its next tick.

/// `GET /admin/read-only/status` — returns whether the engine is in
/// read-only mode and, if so, the reason ("memory" / "disk" / "rocksdb"
/// / "manual"). Useful as a quick poll target for an ops dashboard,
/// and from the alert runbook.
pub async fn admin_read_only_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    let armed = state.read_only.is_armed();
    let reason = state.read_only.reason();
    (
        StatusCode::OK,
        Json(json!({
            "armed": armed,
            "reason": reason.as_str(),
        })),
    )
}

/// `POST /admin/read-only/arm` — manually flip the engine into read-only
/// mode (reason `manual`). Use during maintenance windows or to drain
/// the persist queue before a planned restart.
pub async fn admin_read_only_arm(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    let prev = state.read_only.arm(crate::read_only::ReadOnlyReason::Manual);
    crate::metrics::ENGINE_READ_ONLY.set(1);
    tracing::warn!(
        target = "read_only_guard",
        prev_reason = prev.as_str(),
        "🛑 Read-only mode armed manually by admin"
    );
    (
        StatusCode::OK,
        Json(json!({
            "armed": true,
            "reason": "manual",
            "previous_reason": prev.as_str(),
        })),
    )
}

/// `POST /admin/read-only/disarm` — clear the read-only flag. Works
/// on both manually-armed and auto-armed states. If real resource
/// pressure is still present the watcher will re-arm on its next tick
/// (within ~10 s) — this isn't a way to override the guard, just to
/// release a manual hold.
pub async fn admin_read_only_disarm(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    let prev = state.read_only.disarm();
    crate::metrics::ENGINE_READ_ONLY.set(0);
    tracing::info!(
        target = "read_only_guard",
        prev_reason = prev.as_str(),
        "✅ Read-only mode disarmed manually by admin"
    );
    (
        StatusCode::OK,
        Json(json!({
            "armed": false,
            "previous_reason": prev.as_str(),
        })),
    )
}
