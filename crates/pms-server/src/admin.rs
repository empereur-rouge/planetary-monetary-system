use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
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

    // 👉 Étape 1 : flush WAL
    // (à activer une fois que tu exposes flush() dans RocksStore)
    //
    // if let Err(e) = state.store.flush_wal().await {
    //     eprintln!("[ADMIN] flush_wal failed: {e:#}");
    //     return (
    //         StatusCode::INTERNAL_SERVER_ERROR,
    //         Json(json!({ "error": format!("flush_wal failed: {e}") }))
    //     );
    // }

    // 👉 Étape 2 : compaction
    // (idem : une fois expose compact_all())
    //
    // if let Err(e) = state.store.compact_everything().await {
    //     eprintln!("[ADMIN] compaction failed: {e:#}");
    //     return (
    //         StatusCode::INTERNAL_SERVER_ERROR,
    //         Json(json!({ "error": format!("compaction failed: {e}") }))
    //     );
    // }

    // 👉 Pour l’instant, on renvoie juste un JSON-noop
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "action": "compact",
            "message": "noop (compaction not implemented yet)"
        })),
    )
}
