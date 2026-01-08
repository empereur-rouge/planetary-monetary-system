use crate::api::AppState;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::StatusCode;
use pms_storage::{DagStorage, PutResult};
use pms_token::FeePolicy;
use pms_types::{Block, Transaction};
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
    let mut block = Block {
        id: String::new(),
        parents: parents.clone(),
        payload: payload.clone(),
        nonce: 0,
        metadata: None, // Pas de métadonnées pour les transactions normales
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
