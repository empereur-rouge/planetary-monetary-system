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
) -> impl IntoResponse {
    // Vérification du token admin
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    // Générer un ID unique pour cette mise à jour admin (pas un vrai block)
    let admin_update_id = format!("admin-{}", chrono::Utc::now().timestamp_millis());
    let timestamp = chrono::Utc::now().timestamp_millis();

    // Log de l'action admin
    tracing::warn!(
        "[ADMIN] Config update requested: {} by admin",
        update.description()
    );

    // Appliquer la mise à jour via le trait ConfigStorage
    match state
        .store
        .apply_config_update(&update, &admin_update_id, timestamp)
    {
        Ok(new_config) => {
            tracing::info!(
                "[ADMIN] Config updated successfully: {}",
                update.description()
            );
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "update_applied": update.description(),
                    "config": new_config
                })),
            )
        }
        Err(e) => {
            tracing::error!("[ADMIN] Failed to update config: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to update config: {}", e) })),
            )
        }
    }
}
