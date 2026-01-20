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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DistributeFeesRequest {
    /// Parent block ID (optionnel - si absent, récupère automatiquement un tip)
    #[serde(default)]
    pub parent_id: Option<String>,
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

    // ========================================================================
    // 0. RESOLVE PARENT: Get tip if parent_id not provided
    // ========================================================================
    let parent_id = match &req.parent_id {
        Some(id) if !id.is_empty() => id.clone(),
        _ => {
            // Fetch current tip from DAG
            match st.srv.adapter_arc().top_tips(1).await {
                Ok(tips) if !tips.is_empty() => tips[0].clone(),
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(DistributeFeesResponse {
                            success: false,
                            reward_block_id: None,
                            total_distributed: "0".to_string(),
                            num_recipients: 0,
                        }),
                    );
                }
            }
        }
    };

    // ========================================================================
    // 1. READ FEE POOL: Collect burn refunds and node fees
    // ========================================================================
    let (total_node_fees, shares, burn_refunds, total_burn_refunds) = {
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
        (
            pool.total_fees,
            pool.calculate_shares(),
            pool.get_burn_refunds(),
            pool.total_burn_refunds(),
        )
    };

    // ========================================================================
    // 2. BUILD OUTPUTS: Burn refunds + Node fees
    // ========================================================================
    let coordinator_x25519 = st.node_wallet.x25519_pub_hex().to_string();
    let mut all_outputs: Vec<TxOutput> = Vec::new();
    let mut total_distributed = Decimal::ZERO;

    // 2a. BURN REFUNDS: Direct outputs to user wallets
    for (wallet_address, amount) in &burn_refunds {
        if *amount <= Decimal::ZERO {
            continue;
        }
        all_outputs.push(TxOutput {
            address: wallet_address.clone(),
            amount: amount.to_string(),
        });
        total_distributed += *amount;
        tracing::info!(
            "💰 Burn refund output: {} -> {} PMS",
            &wallet_address[..20.min(wallet_address.len())],
            amount
        );
    }

    // 2b. NODE FEES: Look up node addresses from registry (if any)
    {
        let registry = st.node_registry.read().await;
        let nodes = registry.get_active_nodes();

        for (node_pk, _share_pct, share_amount) in &shares {
            if *share_amount <= Decimal::ZERO {
                continue;
            }
            // Try to find node's reward address from registry
            if let Some(node) = nodes.iter().find(|n| &n.node_pk == node_pk) {
                // TODO: Use node's registered reward_address instead of api_url
                // For now, we skip node rewards if they haven't registered a proper address
                tracing::debug!(
                    "Found node {} with api_url {}, but no reward address yet",
                    &node_pk[..16.min(node_pk.len())],
                    node.api_url
                );
            }
        }
    }

    // If no outputs to distribute, return early
    if all_outputs.is_empty() {
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

    let num_recipients = all_outputs.len();

    // ========================================================================
    // 3. CREATE MINT BLOCK: Direct token distribution
    // ========================================================================
    // We use a Mint payload to create new tokens for refunds
    // Mint expects outputs: Vec<TxOutput> which creates actual UTXOs

    let mint_payload = PlainPayload::Mint {
        outputs: all_outputs.clone(),
    };

    let mut reward_block = Block {
        id: String::new(),
        parents: vec![parent_id.clone()],
        payload: Some(PayloadEnvelope::Plain(mint_payload)),
        nonce: 0,
        metadata: Some(pms_types_block::BlockMetadata {
            signer_x25519_hex: Some(coordinator_x25519.clone()),
            description: Some(format!(
                "Burn refunds: {} PMS to {} wallets",
                total_distributed, num_recipients
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

            // ================================================================
            // 4. CREATE UTXOs: Directly add to UTXO set for each recipient
            // ================================================================
            let mut idx = 0u32;
            for output in &all_outputs {
                st.srv
                    .adapter_arc()
                    .add_utxo(
                        reward_wb.id.clone(),
                        idx,
                        output.address.clone(),
                        output.amount.clone(),
                    )
                    .await;
                idx += 1;
            }

            // Reset fee pool after successful distribution
            {
                let mut pool = st.fee_pool.write().await;
                pool.reset();
            }

            tracing::info!(
                "📦 Burn refunds distributed: {} PMS to {} wallets (block: {})",
                total_distributed,
                num_recipients,
                &reward_wb.id[..16]
            );

            return (
                StatusCode::OK,
                Json(DistributeFeesResponse {
                    success: true,
                    reward_block_id: Some(reward_wb.id),
                    total_distributed: total_distributed.to_string(),
                    num_recipients,
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
        "total_burn_refunds": pool.total_burn_refunds().to_string(),
        "burn_refund_count": pool.burn_refunds.len(),
        "tx_count": pool.tx_count,
        "num_contributors": pool.node_contributions.len(),
    }))
}
