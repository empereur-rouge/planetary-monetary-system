use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose;
use hex::FromHex;
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::atomic::Ordering;

use crate::api::AppState;
use crate::fee_distribution::{
    BlockRewardConfig, FeeDistributionConfig, compute_block_reward_outputs, compute_fee_outputs,
};
use pms_storage::PutResult;
use pms_types::TxOutput;
use pms_types_block::Block;
use pms_types_payload::{EncryptedPayload, EncryptedRewardOutput, PayloadEnvelope, PlainPayload};
use pms_utils::check_pow::check_pow_leading_zero_bits;
use pms_utils::compute_block_id;
use pms_wallet::decode_address;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireBlock;

use axum::response::IntoResponse;

pub async fn submit_block(
    State(st): State<AppState>,
    Json(wb): Json<WireBlock>,
) -> impl IntoResponse {
    // 0) vérif network_id + version
    if wb.network_id != st._cfg.network.network_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("Invalid NetworkID: expected {}", st._cfg.network.network_id),
        )
            .into_response();
    }
    // Protocol Check
    if u32::from(wb.protocol_version) != st._cfg.network.protocol_version {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Invalid Protocol Version: expected {}",
                st._cfg.network.protocol_version
            ),
        )
            .into_response();
    }

    // 0b) vérif signature
    let has_sig_fields = !wb.signer_pk_hex.is_empty();

    if st._cfg.auth.require_signed_submit || has_sig_fields {
        if let Err(code) = verify_wireblock_signature(&wb) {
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            return (code, "Signature verification failed").into_response();
        }
    }

    // 0c) Coordinator Filter: Reject regular TXs if configured (force use of Worker Nodes)
    if st.settings.validation.coordinator_tx_only {
        let is_privileged = if let Some(ref json) = wb.payload_json {
            if let Ok(envelope) = serde_json::from_str::<PayloadEnvelope>(json) {
                match envelope {
                    PayloadEnvelope::Plain(PlainPayload::Milestone { .. })
                    | PayloadEnvelope::Plain(PlainPayload::ConfigUpdate(_)) => true,
                    _ => false,
                }
            } else {
                false // Unparseable or empty -> treat as non-privileged/invalid
            }
        } else {
            false // No payload -> treat as non-privileged
        };

        if !is_privileged {
            tracing::warn!(
                "⛔️ Coordinator rejected non-privileged block from {}",
                &wb.signer_pk_hex
            );
            return (
                StatusCode::FORBIDDEN,
                "Coordinator only accepts Milestones. Use a Worker Node.",
            )
                .into_response();
        }
    }

    // 1) Persist block
    match st.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            st.stats.persisted_ok.fetch_add(1, Ordering::Relaxed);
            let _ = st.srv.enqueue_broadcast(wb.id.clone()).await;

            crate::metrics::BLOCKS_PERSISTED.inc();
            crate::metrics::PMS_BLOCKS_TOTAL.inc();

            // ============================================================
            // FEE POOL ACCUMULATION (Distributed TX Processing)
            // ============================================================
            // Instead of creating a reward block immediately, we accumulate
            // fees in the pool. Distribution happens via Milestone.
            // This removes the Coordinator bottleneck for horizontal scaling.
            accumulate_fee_if_tx(&st, &wb).await;

            // ============================================================
            // BURN REFUND PROCESSING (Cube NFT -> Token Conversion)
            // ============================================================
            // Check if this block contains a valid cube burn and process refund
            process_burn_refund_if_applicable(&st, &wb).await;

            StatusCode::ACCEPTED.into_response()
        }
        Ok(PutResult::AlreadyExists) => {
            st.stats.persisted_dup.fetch_add(1, Ordering::Relaxed);
            StatusCode::CONFLICT.into_response()
        }
        Ok(PutResult::Rejected(reason)) => {
            tracing::warn!("❌ Block Rejected: {}", reason);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            (StatusCode::BAD_REQUEST, reason).into_response()
        }
        Err(e) => {
            tracing::error!("❌ Internal Persist Error: {:#}", e);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Accumulates transaction fee in the pool for later Milestone distribution
/// This replaces immediate reward block creation for horizontal scaling
async fn accumulate_fee_if_tx(st: &AppState, wb: &WireBlock) {
    // Extract transaction fee from payload
    let fee = match extract_tx_fee(wb) {
        Some(f) if f > Decimal::ZERO => f,
        _ => return, // No tx or no fee, nothing to accumulate
    };

    // Get the block signer (node that created/submitted this block)
    // Si absent, on utilise "unknown" mais on logue un warning car c'est anormal.
    let signer_pk = if wb.signer_pk_hex.is_empty() {
        // ⚠️ ALERTE SÉCURITÉ : Un bloc de transaction sans signer est suspect !
        // En production, tous les blocs devraient être signés.
        // Cela peut indiquer :
        //   1. Un bug côté client SDK
        //   2. Une tentative de soumission anonyme
        //   3. Une configuration require_signed_submit=false en dev
        tracing::warn!(
            "⚠️ SECURITY: TX block {} has no signer_pk! Fee credited to 'unknown'. \
             This should not happen in production.",
            &wb.id[..16.min(wb.id.len())]
        );
        "unknown".to_string()
    } else {
        wb.signer_pk_hex.clone()
    };

    // Add fee to pool with node contribution tracking
    {
        let mut pool = st.fee_pool.write().await;
        pool.add_fee(fee, &signer_pk);
        tracing::debug!(
            "💰 Fee accumulated: {} PMS from node {}... (pool total: {} PMS, {} txs)",
            fee,
            &signer_pk[..20.min(signer_pk.len())],
            pool.total_fees,
            pool.tx_count
        );
    }

    // Also increment block count in node registry for this signer
    {
        let mut registry = st.node_registry.write().await;
        registry.increment_block_count(&signer_pk);
    }
}

/// Processes burn refunds for cube NFTs if applicable
/// Adds validated refunds to the fee pool for later distribution
async fn process_burn_refund_if_applicable(st: &AppState, wb: &WireBlock) {
    // 1. Parse payload for NFT Burn action
    let burn_action = match extract_nft_burn_action(wb) {
        Some(action) => action,
        None => return, // Not an NFT burn, nothing to do
    };

    // 2. Get authority public keys from config
    let authority_pks = &st.settings.fees.authority_public_keys;

    // 3. Calculate refund (if valid cube with valid signature)
    let refund = match crate::burn_refund::calculate_burn_refund(
        &burn_action.token_id,
        &burn_action.burner,
        st.store.as_ref(),
        authority_pks,
    ) {
        Ok(Some(r)) => r,
        Ok(None) => return, // No refund (not a cube, invalid sig, etc.)
        Err(e) => {
            tracing::warn!("Burn refund calculation failed: {}", e);
            return;
        }
    };

    // 4. Add refund to fee pool (will be distributed via Milestone)
    {
        let mut pool = st.fee_pool.write().await;
        pool.add_fee(refund.amount, &refund.recipient);
        tracing::info!(
            "🔥 Cube burn refund queued: {} -> {} PMS (token: {})",
            &refund.recipient[..20.min(refund.recipient.len())],
            refund.amount,
            &refund.token_id[..16.min(refund.token_id.len())]
        );
    }
}

/// Simple struct to hold extracted burn action data
struct NftBurnAction {
    token_id: String,
    burner: String,
}

/// Extract NFT Burn action from WireBlock payload if present
fn extract_nft_burn_action(wb: &WireBlock) -> Option<NftBurnAction> {
    let payload_json = wb.payload_json.as_ref()?;
    let envelope: PayloadEnvelope = serde_json::from_str(payload_json).ok()?;

    match envelope {
        PayloadEnvelope::Plain(PlainPayload::Nft(action)) => match action {
            pms_types_nft::NftAction::Burn { token_id, burner } => {
                Some(NftBurnAction { token_id, burner })
            }
            _ => None,
        },
        _ => None,
    }
}

// ============================================================================
// LEGACY: create_reward_block_if_coordinator is no longer used
// Rewards are now distributed via Milestone with distribute_node_rewards=true
// ============================================================================

/// [LEGACY] Creates a reward block if this node is the Coordinator
/// This function is kept for reference but no longer called.
/// Rewards are now accumulated in FeePool and distributed via Milestone.
#[allow(dead_code)]
async fn _create_reward_block_if_coordinator_legacy(st: &AppState, wb: &WireBlock) {
    // Extract transaction fee from payload
    let fee = match extract_tx_fee(wb) {
        Some(f) if f > Decimal::ZERO => f,
        _ => return, // No tx or no fee, skip reward block
    };

    let settings = st.settings.as_ref();
    let node_wallet = &st.node_wallet;

    // Check if this node is the Coordinator
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true // Dev mode
    };

    if !is_coordinator {
        return;
    }

    // Get treasury addresses: prefer signed list from AppState, fallback to config
    let treasury_addrs: Vec<String> = if !st.treasury_wallets.is_empty() {
        st.treasury_wallets.list.clone()
    } else {
        settings.admin.wallet_addresses.clone()
    };

    if treasury_addrs.is_empty() {
        tracing::debug!("No treasury wallets configured, skipping reward block");
        return;
    }

    let creator_address = node_wallet.get_address("8e");

    // Config from settings
    let fee_config = FeeDistributionConfig::from_percents(
        settings.fees.treasury_fee_percent,
        settings.fees.creator_fee_percent,
        settings.fees.parents_fee_percent,
    );

    // Compute fee outputs (15% treasury, 45% creator, 40% parents)
    let fee_outputs_raw = compute_fee_outputs(
        fee,
        &treasury_addrs,
        &creator_address,
        &wb.parents,
        &st.store,
        "8e",
        &fee_config,
    )
    .await;

    // Compute block reward outputs (70% creator, 20% treasury, 10% burn)
    let reward_config = BlockRewardConfig::default();
    let treasury_addr = st
        .treasury_wallets
        .first()
        .cloned()
        .or_else(|| settings.admin.wallet_addresses.first().cloned())
        .unwrap_or_else(|| creator_address.clone());
    let (reward_outputs_raw, burned_amount) =
        compute_block_reward_outputs(&creator_address, &treasury_addr, &reward_config);

    // Log fee distribution details
    tracing::info!(
        "📊 Fee distribution: {} fee outputs, {} reward outputs, {} treasury addrs",
        fee_outputs_raw.len(),
        reward_outputs_raw.len(),
        treasury_addrs.len()
    );
    for fo in &fee_outputs_raw {
        tracing::debug!(
            "  Fee output: {} -> {}",
            &fo.address[..20.min(fo.address.len())],
            fo.amount
        );
    }

    // Create reward block if we have outputs to distribute
    if fee_outputs_raw.is_empty() && reward_outputs_raw.is_empty() {
        return;
    }

    // Convert to TxOutput
    let fee_txouts: Vec<TxOutput> = fee_outputs_raw
        .iter()
        .map(|fo| TxOutput {
            address: fo.address.clone(),
            amount: fo.amount.clone(),
        })
        .collect();

    let reward_txouts: Vec<TxOutput> = reward_outputs_raw
        .iter()
        .map(|fo| TxOutput {
            address: fo.address.clone(),
            amount: fo.amount.clone(),
        })
        .collect();

    // Get coordinator's X25519 public key for encryption
    let coordinator_x25519 = st.node_wallet.x25519_pub_hex().to_string();

    // Encrypt each output for [recipient, coordinator]
    // This ensures only the recipient and coordinator can see the output details
    let mut encrypted_outputs = Vec::new();
    let all_outputs: Vec<&TxOutput> = fee_txouts.iter().chain(reward_txouts.iter()).collect();

    for out in &all_outputs {
        // Extract recipient's X25519 public key from their bech32 address
        let recipient_x25519 = match decode_address(&out.address) {
            Ok((_, x25519_hex)) => x25519_hex,
            Err(e) => {
                tracing::warn!("Failed to decode address {}: {}", &out.address, e);
                continue;
            }
        };

        // Create a simple payload to encrypt (just address + amount)
        let output_data = serde_json::json!({
            "address": out.address,
            "amount": out.amount
        });
        let output_bytes = serde_json::to_vec(&output_data).unwrap_or_default();

        // Encrypt for both recipient and coordinator
        let recipients = vec![recipient_x25519, coordinator_x25519.clone()];
        match EncryptedPayload::encrypt_for(&output_bytes, &recipients, output_bytes.len() as u32) {
            Ok(encrypted) => {
                encrypted_outputs.push(EncryptedRewardOutput { encrypted });
            }
            Err(e) => {
                tracing::warn!("Failed to encrypt output: {}", e);
                continue;
            }
        }
    }

    // Create EncryptedReward payload (for privacy on chain)
    let reward_payload = PlainPayload::EncryptedReward {
        encrypted_outputs,
        burned: burned_amount.to_string(),
        tx_block_id: wb.id.clone(),
    };

    // Create the reward block (parent = the TX block we just created)
    let mut reward_block = Block {
        id: String::new(),
        parents: vec![wb.id.clone()],
        payload: Some(PayloadEnvelope::Plain(reward_payload)),
        nonce: 0,
        metadata: Some(pms_types_block::BlockMetadata {
            signer_x25519_hex: Some(st.node_wallet.x25519_pub_hex().to_string()),
            description: Some("Reward distribution".to_string()),
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

    // PoW (minimal for reward blocks)
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

    // Build WireBlock for reward
    let reward_payload_json = serde_json::to_string(&reward_block.payload).ok();
    let mut reward_wb = WireBlock {
        id: reward_block.id.clone(),
        parents: reward_block.parents.clone(),
        payload_json: reward_payload_json,
        nonce: reward_block.nonce,
        network_id: wb.network_id.clone(),
        protocol_version: wb.protocol_version,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: reward_block.metadata.clone(),
    };

    // Sign
    let reward_msg = canonical_wireblock_message(&reward_wb);
    if let Ok(sig) = node_wallet.sign(&reward_msg) {
        reward_wb.signature_hex = sig;

        // Persist (net_adapter won't create UTXOs for EncryptedReward since it can't decrypt)
        if let Ok(PutResult::Inserted) = st.srv.adapter_arc().persist_block(&reward_wb).await {
            let _ = st.srv.enqueue_broadcast(reward_wb.id.clone()).await;

            // IMPORTANT: Manually create UTXOs from the plaintext outputs
            // The coordinator knows the plaintext but the broadcast payload is encrypted
            // Other nodes will receive the encrypted block but only care about their own UTXOs
            let mut idx = 0u32;
            for out in all_outputs {
                // Add to RAM UTXO set via trait method
                st.srv
                    .adapter_arc()
                    .add_utxo(
                        reward_wb.id.clone(),
                        idx,
                        out.address.clone(),
                        out.amount.clone(),
                    )
                    .await;
                idx += 1;
            }

            tracing::info!(
                "📦 Encrypted reward block created: {} ({} outputs, fees from TX {})",
                &reward_wb.id[..16],
                idx,
                &wb.id[..16]
            );
        }
    }
}

/// Extract transaction fee from a WireBlock's payload
fn extract_tx_fee(wb: &WireBlock) -> Option<Decimal> {
    let payload_json = wb.payload_json.as_ref()?;
    let envelope: PayloadEnvelope = serde_json::from_str(payload_json).ok()?;

    match envelope {
        PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)) => Decimal::from_str(&tx.fee).ok(),
        _ => None,
    }
}

fn verify_wireblock_signature(wb: &WireBlock) -> Result<(), StatusCode> {
    // 1) signer / signature présents ?
    if wb.signer_pk_hex.is_empty() || wb.signature_hex.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 2) reconstruire la clé publique
    let pk_bytes = <Vec<u8>>::from_hex(&wb.signer_pk_hex).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let verify_key =
        VerifyingKey::from_sec1_bytes(&pk_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    // 3) reconstruire la signature (Base64 DER)
    let sig_bytes = general_purpose::STANDARD
        .decode(&wb.signature_hex)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let sig = Signature::from_der(&sig_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    // 4) message canonique identique à celui du CLI
    let msg = canonical_wireblock_message(wb);

    // 5) vérif
    verify_key
        .verify(msg.as_bytes(), &sig)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}
