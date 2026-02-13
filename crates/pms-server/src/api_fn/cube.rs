use crate::api::AppState;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use pms_storage::{DagStorage, PutResult};
use pms_types::{Block, OutputId, TxOutput};
use pms_types_payload::{PayloadEnvelope, PlainPayload, TokenMetadata};
use pms_utils::{check_pow_leading_zero_bits, compute_block_id};
use pms_wallet::SignerBackend;
use pms_wallet::Wallet;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ════════════════════════════════════════════════════════════════════════════
// Constants
// ════════════════════════════════════════════════════════════════════════════

const CUBE_ASSET_ID: &str = "cube";
const CUBE_SYMBOL: &str = "CUBE";
const CUBE_NAME: &str = "DAG Cube";
const CUBE_DECIMALS: u8 = 2;
const CUBES_PER_CLAIM: &str = "1000.00";
/// Exchange rate: 10 CUBE = 1 PMS
const CUBE_TO_PMS_DIVISOR: u64 = 10;

// ════════════════════════════════════════════════════════════════════════════
// POST /v1/cube/claim — Mint CUBE tokens to an address (simulates mining)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CubeClaimRequest {
    /// Adresse Bech32 du destinataire
    pub to: String,
}

#[derive(Debug, Serialize)]
pub struct CubeClaimResponse {
    pub block_id: String,
    pub amount: String,
    pub asset_id: String,
}

pub async fn cube_claim(
    State(state): State<AppState>,
    Json(req): Json<CubeClaimRequest>,
) -> impl IntoResponse {
    let adapter = state.srv.adapter_arc();
    let settings = &*state.settings;
    let meta = WireMeta::from(settings);
    let node_wallet = &state.node_wallet;

    // 1) Auto-register CUBE token if not exists
    if let Ok(None) = state.store.get_token(CUBE_ASSET_ID) {
        let coordinator_pk = node_wallet.encoded_public_key();
        let metadata = TokenMetadata {
            asset_id: CUBE_ASSET_ID.to_string(),
            symbol: CUBE_SYMBOL.to_string(),
            name: CUBE_NAME.to_string(),
            decimals: CUBE_DECIMALS,
            max_supply: None,
            creator: coordinator_pk.clone(),
            mint_authority: coordinator_pk,
        };
        if let Err(e) = state.store.register_token(&metadata) {
            tracing::warn!("CUBE token registration: {e}"); // Already exists race = OK
        }
    }

    // 2) Build Mint payload for CUBE tokens
    let mint_output = TxOutput {
        address: req.to.clone(),
        amount: CUBES_PER_CLAIM.to_string(),
        asset_id: Some(CUBE_ASSET_ID.to_string()),
    };

    let mint_payload = PlainPayload::Mint {
        outputs: vec![mint_output.clone()],
    };
    let payload = Some(PayloadEnvelope::Plain(mint_payload));

    // 3) Get parents
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

    if settings.validation.enforce_single_writer {
        parents.truncate(1);
    }

    // 4) Build block + PoW + sign
    let block_metadata = pms_types_block::BlockMetadata {
        signer_x25519_hex: Some(node_wallet.x25519_pub_hex().to_string()),
        description: Some("Cube claim".to_string()),
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

    let min_bits = adapter.min_pow_leading_zero_bits();
    if min_bits > 0 {
        loop {
            if check_pow_leading_zero_bits(&block.id, min_bits) {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
            if block.nonce == u64::MAX {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "mining failed" })),
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
                    Json(json!({ "error": format!("payload serialize: {e:#}") })),
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
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = match node_wallet.sign(&msg) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("block sign error: {e:?}") })),
            );
        }
    };

    // 5) Persist + UTXO + broadcast
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
            crate::metrics::BLOCKS_PERSISTED.with_label_values(&[&state.ledger_id]).inc();
            crate::metrics::PMS_BLOCKS_TOTAL.with_label_values(&[&state.ledger_id]).inc();
            adapter
                .add_utxo(
                    wb.id.clone(),
                    0,
                    mint_output.address.clone(),
                    mint_output.amount.clone(),
                    Some(CUBE_ASSET_ID.to_string()),
                )
                .await;

            state.srv.enqueue_broadcast(wb.id.clone()).await;

            (
                StatusCode::CREATED,
                Json(json!(CubeClaimResponse {
                    block_id: wb.id,
                    amount: CUBES_PER_CLAIM.to_string(),
                    asset_id: CUBE_ASSET_ID.to_string(),
                })),
            )
        }
        PutResult::AlreadyExists => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "block already exists" })),
        ),
        PutResult::Rejected(reason) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("rejected: {reason}") })),
        ),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// POST /v1/cube/burn — Burn CUBE tokens → receive PMS
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CubeBurnRequest {
    /// Clé privée base64 de l'agent (pour prouver qu'il possède les CUBEs)
    pub private_key_b64: String,
    /// Quantité de CUBEs à brûler (string décimale)
    pub amount: String,
}

#[derive(Debug, Serialize)]
pub struct CubeBurnResponse {
    pub block_id: String,
    pub cubes_burned: String,
    pub pms_received: String,
}

pub async fn cube_burn(
    State(state): State<AppState>,
    Json(req): Json<CubeBurnRequest>,
) -> impl IntoResponse {
    // 1) Reconstruct wallet
    let wallet = match wallet_from_b64(&req.private_key_b64) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid wallet key: {e}") })),
            );
        }
    };

    let hrp = &state.settings.address.hrp;
    let address = wallet.get_address(hrp);

    // 2) Parse burn amount
    let burn_amount = match Decimal::from_str_exact(&req.amount) {
        Ok(d) if d > Decimal::ZERO => d,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "amount must be > 0" })),
            );
        }
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid amount decimal format" })),
            );
        }
    };

    // 3) Calculate PMS to mint (CUBE / rate)
    let pms_amount = burn_amount / Decimal::from(CUBE_TO_PMS_DIVISOR);
    if pms_amount <= Decimal::ZERO {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "burn amount too small to produce any PMS" })),
        );
    }

    // 4) Find CUBE UTXOs
    let adapter = state.srv.adapter_arc();
    let all_utxos = adapter.utxos_by_address(&address).await;

    let cube_utxos: Vec<_> = all_utxos
        .into_iter()
        .filter(|(_, tx_output)| {
            tx_output
                .asset_id
                .as_deref()
                .map(|a| a == CUBE_ASSET_ID)
                .unwrap_or(false)
        })
        .collect();

    if cube_utxos.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "no CUBE UTXOs found",
                "address": address
            })),
        );
    }

    // 5) Coin selection (largest-first)
    let mut utxo_list: Vec<_> = cube_utxos
        .into_iter()
        .filter_map(|(output_id, tx_output)| {
            Decimal::from_str_exact(&tx_output.amount)
                .ok()
                .map(|amt| (output_id, tx_output, amt))
        })
        .collect();
    utxo_list.sort_by(|a, b| b.2.cmp(&a.2));

    let mut selected: Vec<(OutputId, TxOutput, Decimal)> = Vec::new();
    let mut selected_sum = Decimal::ZERO;
    for (output_id, tx_output, amt) in utxo_list {
        if selected_sum >= burn_amount {
            break;
        }
        selected_sum += amt;
        selected.push((output_id, tx_output, amt));
    }

    if selected_sum < burn_amount {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "insufficient CUBE balance",
                "available": selected_sum.to_string(),
                "required": burn_amount.to_string()
            })),
        );
    }

    // 6) Remove CUBE UTXOs (burn them)
    for (output_id, _, _) in &selected {
        adapter.remove_utxo(output_id).await;
    }

    // 6b) If there's CUBE change, we need to return it
    let cube_change = selected_sum - burn_amount;

    // 7) Build Mint block for PMS + optional CUBE change
    let settings = &*state.settings;
    let meta = WireMeta::from(settings);
    let node_wallet = &state.node_wallet;

    let mut mint_outputs = vec![TxOutput {
        address: address.clone(),
        amount: pms_amount.to_string(),
        asset_id: None, // PMS natif
    }];

    if cube_change > Decimal::ZERO {
        mint_outputs.push(TxOutput {
            address: address.clone(),
            amount: cube_change.to_string(),
            asset_id: Some(CUBE_ASSET_ID.to_string()),
        });
    }

    let mint_payload = PlainPayload::Mint {
        outputs: mint_outputs.clone(),
    };
    let payload = Some(PayloadEnvelope::Plain(mint_payload));

    // 8) Get parents + build block
    let mut parents = match state.store.top_tips(2).await {
        Ok(tips) if !tips.is_empty() => tips,
        _ => match state.store.all_block_ids().await {
            Ok(ids) if !ids.is_empty() => vec![ids[0].clone()],
            _ => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "no parents available" })),
                );
            }
        },
    };

    if settings.validation.enforce_single_writer {
        parents.truncate(1);
    }

    let block_metadata = pms_types_block::BlockMetadata {
        signer_x25519_hex: Some(node_wallet.x25519_pub_hex().to_string()),
        description: Some(format!(
            "Cube burn: {} CUBE -> {} PMS",
            burn_amount, pms_amount
        )),
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

    let min_bits = adapter.min_pow_leading_zero_bits();
    if min_bits > 0 {
        loop {
            if check_pow_leading_zero_bits(&block.id, min_bits) {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
            if block.nonce == u64::MAX {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "mining failed" })),
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
                    Json(json!({ "error": format!("payload serialize: {e:#}") })),
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
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = match node_wallet.sign(&msg) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("block sign error: {e:?}") })),
            );
        }
    };

    // 9) Persist + add UTXOs + broadcast
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
            crate::metrics::BLOCKS_PERSISTED.with_label_values(&[&state.ledger_id]).inc();
            crate::metrics::PMS_BLOCKS_TOTAL.with_label_values(&[&state.ledger_id]).inc();
            // Add minted UTXOs
            for (idx, output) in mint_outputs.iter().enumerate() {
                adapter
                    .add_utxo(
                        wb.id.clone(),
                        idx as u32,
                        output.address.clone(),
                        output.amount.clone(),
                        output.asset_id.clone(),
                    )
                    .await;
            }

            state.srv.enqueue_broadcast(wb.id.clone()).await;

            (
                StatusCode::CREATED,
                Json(json!(CubeBurnResponse {
                    block_id: wb.id,
                    cubes_burned: burn_amount.to_string(),
                    pms_received: pms_amount.to_string(),
                })),
            )
        }
        PutResult::AlreadyExists => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "block already exists" })),
        ),
        PutResult::Rejected(reason) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("rejected: {reason}") })),
        ),
    }
}

/// Reconstruit un Wallet depuis private_key_b64
fn wallet_from_b64(priv_b64: &str) -> Result<Wallet, String> {
    let priv_bytes = STANDARD
        .decode(priv_b64)
        .map_err(|e| format!("invalid base64: {e}"))?;
    let priv_hex = hex::encode(&priv_bytes);
    Wallet::from_hex(&priv_hex)
}
