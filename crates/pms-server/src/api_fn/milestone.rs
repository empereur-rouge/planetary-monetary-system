// ============================================================================
// Milestone Distribution - Distributes accumulated fees to nodes
// ============================================================================
//
// Called when Coordinator emits Milestone with distribute_node_rewards=true
// Creates EncryptedReward block with proportional outputs based on block counts

use crate::api::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use pms_wallet::SignerBackend;
use serde::{Deserialize, Serialize};

/// Request to trigger fee distribution via Milestone
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DistributeFeesRequest {
    /// Parent block ID (optionnel - si absent, récupère automatiquement un tip)
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// POST /admin/distribute_fees
/// Coordinator-only: Distributes accumulated fees to nodes
pub async fn distribute_fees(
    State(st): State<AppState>,
    Json(req): Json<DistributeFeesRequest>,
) -> impl IntoResponse {
    // Only Coordinator can distribute
    let settings = &st.settings;
    let node_wallet = &st.node_wallet;

    // Check permission logic duplicated here or just rely on perform_fee_distribution check?
    // perform_fee_distribution returns success=false if not coordinator.
    // However, the original handler returned 403 Forbidden.
    // Let's keep the explicit check for 403.

    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true // Dev mode
    };

    if !is_coordinator {
        return (
            StatusCode::FORBIDDEN,
            Json(crate::fee_distribution::DistributeFeesResult {
                success: false,
                reward_block_id: None,
                total_distributed: "0".to_string(),
                num_recipients: 0,
            }),
        )
            .into_response();
    }

    match crate::fee_distribution::perform_fee_distribution(&st, req.parent_id).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "success": false,
                "reward_block_id": null,
                "total_distributed": "0",
                "num_recipients": 0,
                "error": e.to_string()
            })),
        )
            .into_response(),
    }
}

/// GET /v1/fee_pool
/// Returns current fee pool status (public endpoint)
pub async fn get_fee_pool_status(State(st): State<AppState>) -> impl IntoResponse {
    let pool = st.fee_pool.read().await;

    Json(serde_json::json!({
        "total_fees": pool.total_fees.to_string(),
        "total_burn_refunds": pool.total_burn_refunds().to_string(),
        "burn_refund_count": pool.burn_refunds.len(),
        "tx_count": pool.tx_count,
        "num_contributors": pool.node_contributions.len(),
    }))
}
