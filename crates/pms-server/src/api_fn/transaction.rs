use crate::{
    api::AppState,
    fee_distribution::{
        BlockRewardConfig, FeeDistributionConfig, compute_block_reward_outputs, compute_fee_outputs,
    },
};
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::StatusCode;
use pms_storage::{DagStorage, PutResult};
use pms_token::FeePolicy;
use pms_types::{Block, Transaction, TxOutput};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_utils::{check_pow_leading_zero_bits, compute_block_id};
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
pub struct WalletSendTxRequest {
    // Transaction déjà signée côté wallet (unlocks remplis)
    pub tx: Transaction,
    // X25519 pubkeys hex pour le chiffrement du payload (sender + dest)
    pub recipients_xpk: Vec<String>,
}

pub async fn wallet_send_tx(
    State(state): State<AppState>,
    Json(body): Json<WalletSendTxRequest>,
) -> impl IntoResponse {
    // ============================================================
    // 0) Use settings from AppState (configured at startup/test time)
    // ============================================================
    let settings = &*state.settings;
    let meta = WireMeta::from(settings);

    // ============================================================
    // 1) Parse fee (string -> Decimal) et normalise
    // ============================================================
    let tx = body.tx;

    let fee_dec = match Decimal::from_str_exact(&tx.fee) {
        Ok(d) => d,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid tx.fee decimal" })),
            );
        }
    };

    if fee_dec < Decimal::ZERO {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "tx.fee must be >= 0" })),
        );
    }

    // ============================================================
    // 2) Validation des frais (Calcul strict)
    // ============================================================
    // On n'injecte PLUS rien (cela casserait la signature client).
    // On VÉRIFIE que le client a bien inclus l'output de frais vers un admin.

    // a) Charger la policy
    let fee_policy = FeePolicy::new(
        &settings.fees.base_fee,
        &settings.fees.ratio,
        18, // Precision (TODO: put in config provided token decimals?)
    );

    // b) Identifier H20 Sender pour exclure le Change
    //    Unlock[0] contient la pubkey du sender.
    let sender_h20 = if let Some(first_unlock) = tx.unlocks.first() {
        if let Ok(pub_bytes) = hex::decode(&first_unlock.pubkey_hex) {
            let hash = Sha256::digest(&pub_bytes);
            hex::encode(&hash[..20])
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // c) Identifier les outputs de frais (vers un wallet admin)
    //    On tolère n'importe quel admin de la liste
    let mut provided_fee = Decimal::ZERO;
    let mut taxable_amount = Decimal::ZERO;

    for out in &tx.outputs {
        // 1. Check Admin (Fee)
        if settings
            .admin
            .wallet_addresses
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&out.address))
        {
            if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
                provided_fee += amt;
            }
        }
        // 2. Check Sender (Change/Self) - compare H20
        else {
            let is_sender = if !sender_h20.is_empty() {
                match pms_wallet::decode_address(&out.address) {
                    Ok((h20, _xpk)) => h20 == sender_h20,
                    Err(_) => false,
                }
            } else {
                false
            };

            if !is_sender {
                if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
                    taxable_amount += amt;
                }
            }
        }
    }

    // c) Calculer le fee attendu
    let expected_fee_str = fee_policy
        .compute_fee(&taxable_amount.to_string())
        .unwrap_or("0.0".to_string());
    let expected_fee_dec = Decimal::from_str_exact(&expected_fee_str).unwrap_or(Decimal::ZERO);

    // d) Vérifier (avec une petite tolérance epsilon si besoin, mais Decimal est précis)
    if provided_fee < expected_fee_dec {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "insufficient fees",
                "provided": provided_fee.to_string(),
                "expected": expected_fee_dec.to_string(),
                "taxable_amount": taxable_amount.to_string()
            })),
        );
    }

    // ============================================================
    // ============================================================
    // 3) Chiffrement (recipients_xpk de base, le client doit avoir inclus l'admin si besoin)
    // ============================================================
    let recipients_xpk = body.recipients_xpk.clone();
    let plain = PlainPayload::TxUtxo(tx);
    let enc = match EncryptedPayload::encrypt_for_plain(&plain, &recipients_xpk) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("encrypt failed: {e}") })),
            );
        }
    };
    let payload = Some(PayloadEnvelope::Encrypted(enc));

    // ============================================================
    // 4) Parents via store - use genesis as supplementary parent if needed
    // ============================================================
    let mut parents = match state.store.top_tips(2).await {
        Ok(tips) if !tips.is_empty() => tips,
        _ => match state.store.all_block_ids().await {
            Ok(ids) if !ids.is_empty() => vec![ids[0].clone()],
            _ => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "no parents available (empty DAG)" })),
                );
            }
        },
    };

    // If we have fewer than 2 parents and genesis isn't already included,
    // add genesis as supplementary parent
    if parents.len() < 2 {
        let genesis_id = Block::genesis(compute_block_id).id;
        if !parents.contains(&genesis_id) {
            parents.push(genesis_id);
        }
    }

    // ============================================================
    // 5) Forge bloc + WireBlock + signature + persist
    // ============================================================
    // Include signer's X25519 public key in metadata for fee distribution
    // This allows parent block creators to receive their share of fees
    let block_metadata = pms_types_block::BlockMetadata {
        signer_x25519_hex: Some(state.node_wallet.x25519_pub_hex().to_string()),
        ..Default::default()
    };

    let mut block = Block {
        id: String::new(),
        parents: parents.clone(),
        payload: payload.clone(),
        nonce: 0,
        metadata: Some(block_metadata),
        signer_pk: None,
        signature: None,
    };
    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

    // PoW Mining Loop
    // Use the policy from the active adapter to ensure we meet the actual validation requirements
    let min_bits = state.srv.adapter_arc().min_pow_leading_zero_bits();

    if min_bits > 0 {
        loop {
            // Check difficulty using shared util
            if check_pow_leading_zero_bits(&block.id, min_bits) {
                break;
            }

            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

            // Safety break
            if block.nonce == u64::MAX {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "mining failed (nonce exhaustion)" })),
                );
            }
        }
    }

    let payload_json = match &block.payload {
        None => None,
        Some(env) => match serde_json::to_string(env) {
            Ok(s) => Some(s),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("payload serialize error: {e:#}") })),
                );
            }
        },
    };

    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json,
        nonce: block.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let node_wallet = &state.node_wallet;
    wb.signer_pk_hex = node_wallet.encoded_public_key();

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = match node_wallet.sign(&msg) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("sign error: {e:?}") })),
            );
        }
    };

    let adapter = state.srv.adapter_arc();
    let res = match adapter.persist_block(&wb).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("persist error: {e:#}") })),
            );
        }
    };

    match res {
        PutResult::Inserted => {
            // Announce the new block to the network for gossip propagation
            state.srv.enqueue_broadcast(wb.id.clone()).await;

            // ============================================================
            // 6) CREATE REWARD BLOCK (Fee Distribution + Block Rewards)
            // ============================================================
            // SECURITY: Only Coordinator creates Reward blocks
            // This prevents unauthorized token creation

            // Check if this node is the Coordinator
            let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key
            {
                // Compare node's public key with configured coordinator key
                node_wallet.encoded_public_key() == *coord_pk
            } else {
                // Dev mode: no coordinator check
                true
            };

            if is_coordinator {
                // Crée un bloc de reward séparé qui distribue:
                // - 15% Treasury, 45% Creator, 40% Parents (des tx fees)
                // - 70% Creator, 20% Treasury, 10% Burn (block rewards)

                // Config depuis settings
                let fee_config = FeeDistributionConfig::from_percents(
                    settings.fees.treasury_fee_percent,
                    settings.fees.creator_fee_percent,
                    settings.fees.parents_fee_percent,
                );

                let creator_address = node_wallet.get_address("8e");

                // Get treasury addresses: prefer signed list from AppState, fallback to config
                let treasury_addrs: Vec<String> = if !state.treasury_wallets.is_empty() {
                    tracing::debug!(
                        "Using {} treasury wallets from signed list",
                        state.treasury_wallets.len()
                    );
                    state.treasury_wallets.list.clone()
                } else {
                    tracing::debug!(
                        "Treasury wallets empty, using {} admin addresses as fallback",
                        settings.admin.wallet_addresses.len()
                    );
                    settings.admin.wallet_addresses.clone()
                };
                tracing::debug!(
                    "Treasury distribution to {} addresses",
                    treasury_addrs.len()
                );

                // Calcul des fee outputs (15% treasury, 45% creator, 40% parents)
                let fee_outputs_raw = compute_fee_outputs(
                    fee_dec,
                    &treasury_addrs,
                    &creator_address,
                    &parents,
                    &state.store,
                    "8e",
                    &fee_config,
                )
                .await;

                // Calcul des block reward outputs (70% creator, 20% treasury, 10% burn)
                let reward_config = BlockRewardConfig::default();
                let treasury_addr = state
                    .treasury_wallets
                    .first()
                    .cloned()
                    .or_else(|| settings.admin.wallet_addresses.first().cloned())
                    .unwrap_or_else(|| creator_address.clone());
                let (reward_outputs_raw, burned_amount) =
                    compute_block_reward_outputs(&creator_address, &treasury_addr, &reward_config);

                // Log fee distribution details for debugging
                tracing::info!(
                    "Fee distribution: {} fee outputs, {} reward outputs, {} treasury addrs",
                    fee_outputs_raw.len(),
                    reward_outputs_raw.len(),
                    treasury_addrs.len()
                );
                for fo in &fee_outputs_raw {
                    tracing::info!(
                        "  Fee output: {} -> {}",
                        &fo.address[..20.min(fo.address.len())],
                        fo.amount
                    );
                }

                // Si on a des outputs à distribuer, créer un reward block
                if !fee_outputs_raw.is_empty() || !reward_outputs_raw.is_empty() {
                    // Convertir en TxOutput
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

                    // Payload Reward
                    let reward_payload = PlainPayload::Reward {
                        fee_outputs: fee_txouts,
                        reward_outputs: reward_txouts,
                        burned: burned_amount.to_string(),
                        tx_block_id: wb.id.clone(),
                    };

                    // Créer le bloc de reward (parent = le bloc TX qu'on vient de créer)
                    let mut reward_block = Block {
                        id: String::new(),
                        parents: vec![wb.id.clone()],
                        payload: Some(PayloadEnvelope::Plain(reward_payload)),
                        nonce: 0,
                        metadata: Some(pms_types_block::BlockMetadata {
                            signer_x25519_hex: Some(state.node_wallet.x25519_pub_hex().to_string()),
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

                    // PoW (minimal pour reward blocks)
                    let min_bits = state.srv.adapter_arc().min_pow_leading_zero_bits();
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

                    // Build WireBlock pour le reward
                    let reward_payload_json = serde_json::to_string(&reward_block.payload).ok();
                    let mut reward_wb = WireBlock {
                        id: reward_block.id.clone(),
                        parents: reward_block.parents.clone(),
                        payload_json: reward_payload_json,
                        nonce: reward_block.nonce,
                        network_id: meta.network_id.clone(),
                        protocol_version: meta.protocol_version as u16,
                        signer_pk_hex: node_wallet.encoded_public_key(),
                        signature_hex: String::new(),
                        metadata: reward_block.metadata.clone(),
                    };

                    // Signer
                    let reward_msg = canonical_wireblock_message(&reward_wb);
                    if let Ok(sig) = node_wallet.sign(&reward_msg) {
                        reward_wb.signature_hex = sig;

                        // Persister
                        if let Ok(PutResult::Inserted) = adapter.persist_block(&reward_wb).await {
                            state.srv.enqueue_broadcast(reward_wb.id.clone()).await;
                            tracing::info!(
                                "📦 Reward block created: {} (fees distributed from TX {})",
                                &reward_wb.id[..16],
                                &wb.id[..16]
                            );
                        }
                    }
                }
            } // End of if is_coordinator

            (
                StatusCode::CREATED,
                Json(json!({ "id": wb.id, "status": "inserted" })),
            )
        }
        PutResult::AlreadyExists => (
            StatusCode::CONFLICT,
            Json(json!({ "id": wb.id, "status": "duplicate" })),
        ),
        PutResult::Rejected(reason) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "rejected", "reason": reason })),
        ),
    }
}
