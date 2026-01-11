// ============================================================================
// Milestone Distribution - Distributes accumulated fees to nodes
// ============================================================================
//
// Called when Coordinator emits Milestone with distribute_node_rewards=true
// Creates EncryptedReward block with proportional outputs based on block counts

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::api::AppState;
use crate::fee_distribution::{BlockRewardConfig, FeeDistributionConfig, compute_fee_outputs};
use pms_storage::PutResult;
use pms_types::TxOutput;
use pms_types_block::Block;
use pms_types_payload::{EncryptedPayload, EncryptedRewardOutput, PayloadEnvelope, PlainPayload};
use pms_utils::check_pow::check_pow_leading_zero_bits;
use pms_utils::compute_block_id;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, decode_address};
use pms_wire::WireBlock;

/// Request to trigger fee distribution via Milestone
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributeFeesRequest {
    /// Parent block ID for the reward block (usually current tip)
    pub parent_id: String,
}

/// Response from distribution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributeFeesResponse {
    pub success: bool,
    pub reward_block_id: Option<String>,
    pub total_distributed: String,
    pub num_recipients: usize,
}

/// POST /admin/distribute_fees
/// Coordinator-only: Distributes accumulated fees to nodes
pub async fn distribute_fees(
    State(st): State<AppState>,
    Json(req): Json<DistributeFeesRequest>,
) -> impl IntoResponse {
    let settings = st.settings.as_ref();
    let node_wallet = &st.node_wallet;

    // Only Coordinator can distribute
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true // Dev mode
    };

    if !is_coordinator {
        return (
            StatusCode::FORBIDDEN,
            Json(DistributeFeesResponse {
                success: false,
                reward_block_id: None,
                total_distributed: "0".to_string(),
                num_recipients: 0,
            }),
        );
    }

    // Read fee pool
    let (total_fees, shares) = {
        let pool = st.fee_pool.read().await;
        if !pool.has_fees() {
            return (
                StatusCode::OK,
                Json(DistributeFeesResponse {
                    success: true,
                    reward_block_id: None,
                    total_distributed: "0".to_string(),
                    num_recipients: 0,
                }),
            );
        }
        (pool.total_fees, pool.calculate_shares())
    };

    // Get node addresses from registry
    let node_addresses: Vec<(String, Decimal)> = {
        let registry = st.node_registry.read().await;
        let nodes = registry.get_active_nodes();

        shares
            .iter()
            .filter_map(|(node_pk, _, amount)| {
                // Try to find node's API URL (which might contain their receiving address)
                // For now, use the node_pk as a placeholder
                // In production, nodes should register with their reward address
                nodes
                    .iter()
                    .find(|n| n.node_pk == *node_pk)
                    .map(|n| (n.api_url.clone(), *amount))
            })
            .collect()
    };

    if node_addresses.is_empty() {
        return (
            StatusCode::OK,
            Json(DistributeFeesResponse {
                success: true,
                reward_block_id: None,
                total_distributed: "0".to_string(),
                num_recipients: 0,
            }),
        );
    }

    // Create reward outputs
    // Note: For simplicity, we use node_pk as placeholder.
    // In production, nodes should register with their bech32 reward address.
    let coordinator_x25519 = st.node_wallet.x25519_pub_hex().to_string();
    let mut encrypted_outputs = Vec::new();

    for (node_pk, _share_pct, share_amount) in &shares {
        if *share_amount <= Decimal::ZERO {
            continue;
        }

        // Note: In production, look up node's bech32 address from registry
        // For now, skip encryption since we don't have proper addresses
        let output_data = serde_json::json!({
            "node_pk": node_pk,
            "amount": share_amount.to_string()
        });
        let output_bytes = serde_json::to_vec(&output_data).unwrap_or_default();

        // Encrypt for coordinator only (node should provide their x25519 key)
        let recipients = vec![coordinator_x25519.clone()];
        match EncryptedPayload::encrypt_for(&output_bytes, &recipients, output_bytes.len() as u32) {
            Ok(encrypted) => {
                encrypted_outputs.push(EncryptedRewardOutput { encrypted });
            }
            Err(e) => {
                tracing::warn!("Failed to encrypt output for {}: {}", node_pk, e);
            }
        }
    }

    // Create reward block
    let reward_payload = PlainPayload::EncryptedReward {
        encrypted_outputs,
        burned: "0".to_string(),
        tx_block_id: req.parent_id.clone(),
    };

    let mut reward_block = Block {
        id: String::new(),
        parents: vec![req.parent_id.clone()],
        payload: Some(PayloadEnvelope::Plain(reward_payload)),
        nonce: 0,
        metadata: Some(pms_types_block::BlockMetadata {
            signer_x25519_hex: Some(coordinator_x25519),
            description: Some(format!(
                "Fee distribution: {} PMS to {} nodes",
                total_fees,
                shares.len()
            )),
            ..Default::default()
        }),
        signer_pk: None,
        signature: None,
    };
    reward_block.id = compute_block_id(
        &reward_block.parents,
        &reward_block.payload,
        reward_block.nonce,
    );

    // PoW
    let min_bits = st.srv.adapter_arc().min_pow_leading_zero_bits();
    if min_bits > 0 {
        while !check_pow_leading_zero_bits(&reward_block.id, min_bits) {
            reward_block.nonce += 1;
            reward_block.id = compute_block_id(
                &reward_block.parents,
                &reward_block.payload,
                reward_block.nonce,
            );
        }
    }

    // Build WireBlock
    let reward_payload_json = serde_json::to_string(&reward_block.payload).ok();
    let mut reward_wb = WireBlock {
        id: reward_block.id.clone(),
        parents: reward_block.parents.clone(),
        payload_json: reward_payload_json,
        nonce: reward_block.nonce,
        network_id: st._cfg.network.network_id.clone(),
        protocol_version: st._cfg.network.protocol_version as u16,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: reward_block.metadata.clone(),
    };

    // Sign
    let reward_msg = canonical_wireblock_message(&reward_wb);
    if let Ok(sig) = node_wallet.sign(&reward_msg) {
        reward_wb.signature_hex = sig;

        if let Ok(PutResult::Inserted) = st.srv.adapter_arc().persist_block(&reward_wb).await {
            let _ = st.srv.enqueue_broadcast(reward_wb.id.clone()).await;

            // Reset fee pool after successful distribution
            {
                let mut pool = st.fee_pool.write().await;
                pool.reset();
            }

            tracing::info!(
                "📦 Fee distribution complete: {} PMS to {} nodes (block: {})",
                total_fees,
                shares.len(),
                &reward_wb.id[..16]
            );

            return (
                StatusCode::OK,
                Json(DistributeFeesResponse {
                    success: true,
                    reward_block_id: Some(reward_wb.id),
                    total_distributed: total_fees.to_string(),
                    num_recipients: shares.len(),
                }),
            );
        }
    }

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(DistributeFeesResponse {
            success: false,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        }),
    )
}

/// GET /v1/fee_pool
/// Returns current fee pool status (public endpoint)
pub async fn get_fee_pool_status(State(st): State<AppState>) -> impl IntoResponse {
    let pool = st.fee_pool.read().await;

    Json(serde_json::json!({
        "total_fees": pool.total_fees.to_string(),
        "tx_count": pool.tx_count,
        "num_contributors": pool.node_contributions.len(),
    }))
}
