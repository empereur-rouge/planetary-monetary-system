use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use pms_bridge::engine::BridgeEngine;
use pms_bridge::store::BridgeStore;
use pms_bridge::types::{BridgeDisableRequest, BridgeEnableRequest, BridgeTransferRequest};
use serde_json::json;

/// Helper: construit un BridgeEngine depuis l'AppState.
fn bridge_engine(state: &AppState) -> Option<BridgeEngine> {
    let mgr = state.ledger_mgr.as_ref()?;
    let default = mgr.default_ledger()?;
    let bridge_store = BridgeStore::new(default.store.clone());
    Some(BridgeEngine::new(
        mgr.clone(),
        bridge_store,
        state.node_wallet.clone(),
    ))
}

/// POST /admin/bridge/enable — Active un pont (admin, n'importe quels ledgers)
pub async fn admin_bridge_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BridgeEnableRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.enable_bridge(&req, true, None) {
        Ok(link) => (StatusCode::CREATED, Json(json!(link))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /admin/bridge/disable — Coupe un pont (admin)
pub async fn admin_bridge_disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BridgeDisableRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.disable_bridge(&req, true, None) {
        Ok(link) => (StatusCode::OK, Json(json!(link))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /admin/bridge/transfer — Transfert cross-ledger (admin)
pub async fn admin_bridge_transfer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BridgeTransferRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.execute_transfer(&req).await {
        Ok(resp) => (StatusCode::CREATED, Json(json!(resp))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /v1/bridge/links — Liste tous les ponts
pub async fn list_bridge_links(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.list_bridges() {
        Ok(links) => (StatusCode::OK, Json(json!({"links": links}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /v1/bridge/status/{lock_block_id} — Statut d'un transfert
pub async fn bridge_status(
    State(state): State<AppState>,
    axum::extract::Path(lock_block_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.transfer_status(&lock_block_id) {
        Ok(Some(mint_id)) => (
            StatusCode::OK,
            Json(json!({
                "lock_block_id": lock_block_id,
                "mint_block_id": mint_id,
                "status": "completed"
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "lock_block_id": lock_block_id,
                "status": "not_found"
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
