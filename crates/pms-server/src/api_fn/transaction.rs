use axum::extract::State;
use axum::Json;
use axum::response::IntoResponse;
use http::StatusCode;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use pms_ledger::pick_fee_recipient_address;
use pms_storage::{DagStorage, PutResult};
use pms_types::{Block, Transaction, TxOutput};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};
use crate::api::AppState;

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
    // 0) Charger config + meta réseau
    // ============================================================
    let settings = match pms_config::load_config() {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("config error: {e:#}") })),
            );
        }
    };
    let meta = WireMeta::from(&settings);

    // ============================================================
    // 1) Parse fee (string -> Decimal) et normalise
    // ============================================================
    let mut tx = body.tx;

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
    // 2) Option B: fee = output(s) + tx.fee mis à "0" AVANT chiffrement
    //    + injection non-répétable
    // ============================================================
    let mut recipients_xpk = body.recipients_xpk.clone();

    if fee_dec > Decimal::ZERO {
        // 2.a) Choisir UN destinataire fee (une seule fois)
        let fee_addr = match pick_fee_recipient_address(&settings) {
            Ok(a) => a,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("fee recipient error: {e:#}") })),
                );
            }
        };

        // 2.b) Injection NON-RÉPÉTABLE :
        //      si un output fee existe déjà (même adresse + même amount), on n'ajoute rien.
        let fee_amount_str = tx.fee.clone();
        let fee_already_materialized = tx.outputs.iter().any(|o| {
            o.address.eq_ignore_ascii_case(&fee_addr)
                && o.amount.trim() == fee_amount_str.trim()
        });

        if !fee_already_materialized {
            tx.outputs.push(TxOutput {
                address: fee_addr.clone(),
                amount: fee_amount_str.clone(),
            });
        }

        // 2.c) OPTION B : on met fee à 0 (car matérialisée en output)
        tx.fee = "0".to_string();

        // 2.d) Le fee recipient doit pouvoir déchiffrer => ajouter son xpk
        let fee_xpk = match pms_wallet::decode_address(&fee_addr) {
            Ok((_h20, xpk)) => xpk,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("fee_recipient address invalid (cannot decode xpk): {e}") })),
                );
            }
        };

        let already = recipients_xpk
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&fee_xpk));
        if !already {
            recipients_xpk.push(fee_xpk);
        }

        // (debug utile pendant stabilisation)
        eprintln!("[WALLET_SEND] fee_addr={fee_addr}");
        eprintln!("[WALLET_SEND] recipients_xpk(final)={recipients_xpk:?}");
        eprintln!("[WALLET_SEND] tx.outputs(final)={:?}", tx.outputs);
        eprintln!("[WALLET_SEND] tx.fee(after materialize)={}", tx.fee);
    }

    // ============================================================
    // 3) Chiffrement avec recipients_xpk FINAL (pas body.recipients_xpk)
    // ============================================================
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
    // 4) Parents via store
    // ============================================================
    let parents = match state.store.top_tips(2).await {
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

    // ============================================================
    // 5) Forge bloc + WireBlock + signature + persist
    // ============================================================
    let mut block = Block {
        id: String::new(),
        parents: parents.clone(),
        payload: payload.clone(),
        nonce: 0,
    };
    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

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
        PutResult::Inserted => (
            StatusCode::CREATED,
            Json(json!({ "id": wb.id, "status": "inserted" })),
        ),
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