//! API endpoints pour la gestion des contrats déclaratifs.
//!
//! Tous les endpoints sont coordinator-only (admin).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use pms_storage::ContractStorage;
use pms_types_contract::Contract;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::AppState;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterContractRequest {
    pub name: String,
    pub scope: pms_types_contract::ContractScope,
    pub trigger: pms_types_contract::ContractTrigger,
    pub actions: Vec<pms_types_contract::ContractAction>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToggleContractRequest {
    pub enabled: bool,
    pub reason: String,
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /admin/contracts — Register a new contract
// ═══════════════════════════════════════════════════════════════════════════

pub async fn register_contract(
    State(state): State<AppState>,
    Json(req): Json<RegisterContractRequest>,
) -> impl IntoResponse {
    if req.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "name cannot be empty" })),
        )
            .into_response();
    }
    if req.actions.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "at least one action required" })),
        )
            .into_response();
    }

    // Generate contract_id as SHA-256 of (name + trigger + actions)
    let content_bytes = serde_json::to_vec(&(&req.name, &req.trigger, &req.actions))
        .unwrap_or_default();
    let contract_id = hex::encode(Sha256::digest(&content_bytes));

    let contract = Contract {
        contract_id: contract_id.clone(),
        name: req.name,
        scope: req.scope,
        trigger: req.trigger,
        actions: req.actions,
        enabled: true,
        version: 1,
    };

    if let Err(e) = state.store.put_contract(&contract) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to store contract: {e}") })),
        )
            .into_response();
    }

    tracing::info!(
        "Contract '{}' registered (id={}, scope={:?})",
        contract.name,
        &contract_id[..16],
        contract.scope
    );

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "contract_id": contract_id,
            "name": contract.name,
            "enabled": contract.enabled,
        })),
    )
        .into_response()
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /admin/contracts — List all contracts
// ═══════════════════════════════════════════════════════════════════════════

pub async fn list_contracts(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.list_contracts() {
        Ok(contracts) => (StatusCode::OK, Json(serde_json::json!({ "contracts": contracts }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to list contracts: {e}") })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /admin/contracts/:id — Get contract details
// ═══════════════════════════════════════════════════════════════════════════

pub async fn get_contract(
    State(state): State<AppState>,
    Path(contract_id): Path<String>,
) -> impl IntoResponse {
    match state.store.get_contract(&contract_id) {
        Ok(Some(contract)) => (StatusCode::OK, Json(serde_json::json!(contract))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Contract not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to get contract: {e}") })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// POST /admin/contracts/:id/toggle — Enable/disable a contract
// ═══════════════════════════════════════════════════════════════════════════

pub async fn toggle_contract(
    State(state): State<AppState>,
    Path(contract_id): Path<String>,
    Json(req): Json<ToggleContractRequest>,
) -> impl IntoResponse {
    // Verify contract exists
    match state.store.get_contract(&contract_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Contract not found" })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("{e}") })),
            )
                .into_response();
        }
    }

    if let Err(e) = state.store.set_enabled(&contract_id, req.enabled) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to toggle: {e}") })),
        )
            .into_response();
    }

    tracing::info!(
        "Contract {} toggled to enabled={} (reason: {})",
        &contract_id[..16.min(contract_id.len())],
        req.enabled,
        req.reason
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "contract_id": contract_id,
            "enabled": req.enabled,
        })),
    )
        .into_response()
}
