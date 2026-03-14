//! API endpoints for gas pool management.
//!
//! - `POST /admin/gas-pool/deposit` — Deposit PMS into a ledger's gas pool
//! - `POST /admin/gas-pool/withdraw` — Withdraw PMS from a ledger's gas pool
//! - `GET /v1/gas-pool/{ledger_id}` — Public: view gas pool balance & stats

use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use pms_storage::GasPoolStorage;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct GasPoolDepositRequest {
    pub ledger_id: String,
    pub amount: String,
}

#[derive(Deserialize)]
pub struct GasPoolWithdrawRequest {
    pub ledger_id: String,
    pub amount: String,
}

/// POST /admin/gas-pool/deposit
pub async fn admin_gas_pool_deposit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GasPoolDepositRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let amount: rust_decimal::Decimal = match req.amount.parse() {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid amount"})),
            )
                .into_response();
        }
    };

    if amount <= rust_decimal::Decimal::ZERO {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "amount must be positive"})),
        )
            .into_response();
    }

    match state.store.deposit_gas(&req.ledger_id, amount) {
        Ok(new_balance) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "ledger_id": req.ledger_id,
                "deposited": amount.to_string(),
                "new_balance": new_balance.to_string(),
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

/// POST /admin/gas-pool/withdraw
pub async fn admin_gas_pool_withdraw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GasPoolWithdrawRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let amount: rust_decimal::Decimal = match req.amount.parse() {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid amount"})),
            )
                .into_response();
        }
    };

    if amount <= rust_decimal::Decimal::ZERO {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "amount must be positive"})),
        )
            .into_response();
    }

    match state.store.withdraw_gas(&req.ledger_id, amount) {
        Ok(new_balance) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "ledger_id": req.ledger_id,
                "withdrawn": amount.to_string(),
                "new_balance": new_balance.to_string(),
            })),
        )
            .into_response(),
        Err(e) => {
            let status = if e.to_string().contains("Insufficient") {
                StatusCode::PAYMENT_REQUIRED
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// GET /v1/gas-pool/{ledger_id}
pub async fn get_gas_pool(
    State(state): State<AppState>,
    Path(ledger_id): Path<String>,
) -> impl IntoResponse {
    match state.store.get_gas_pool(&ledger_id) {
        Ok(Some(pool)) => (
            StatusCode::OK,
            Json(json!({
                "ledger_id": pool.ledger_id,
                "balance": pool.balance.to_string(),
                "total_consumed": pool.total_consumed.to_string(),
                "total_deposited": pool.total_deposited.to_string(),
                "created_at": pool.created_at,
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("No gas pool for ledger '{ledger_id}'")})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
