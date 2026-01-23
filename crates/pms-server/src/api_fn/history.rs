use crate::api::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use pms_storage::DagStorage;
use pms_types_payload::PayloadEnvelope;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct PageQ {
    pub after_ts: Option<i64>,
    pub after_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct PageResp<T> {
    pub items: Vec<T>,
    pub next_after_ts: Option<i64>,
    pub next_after_id: Option<String>,
    pub has_more: bool,
}

pub async fn get_encrypted_history(
    State(app): State<AppState>,
    Query(q): Query<PageQ>,
) -> Result<Json<PageResp<pms_wire::WireBlock>>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(200).min(500);

    // ⚙️ Infos réseau à partir de la config du serveur
    let nid = app._cfg.network.network_id.clone();
    let pv = app._cfg.network.protocol_version;

    let (ids, next_cursor) = app
        .store
        .recent_ids_by_time(q.after_ts, q.after_id.clone(), limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // ⬇️ Ici `stored` est un Vec<StoredBlock>
    let stored = app
        .store
        .get_blocks_by_ids(&ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // mapping StoredBlock -> WireBlock avec network/proto
    let mut blocks: Vec<pms_wire::WireBlock> = stored
        .into_iter()
        .map(|sb| pms_wire::WireBlock {
            id: sb.id,
            parents: sb.parents,
            payload_json: sb.payload_json,
            nonce: sb.nonce,
            network_id: nid.clone(),
            protocol_version: pv as u16,
            // pour l’instant, historique sans signature
            signer_pk_hex: String::new(),
            signature_hex: String::new(),
            metadata: sb.metadata,
        })
        .collect();

    // ne garder que les Encrypted
    blocks.retain(|wb| {
        wb.payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
            .map(|env| matches!(env, PayloadEnvelope::Encrypted(_)))
            .unwrap_or(false)
    });

    let (next_after_ts, next_after_id, has_more) = next_cursor
        .map(|(ts, id, more)| (Some(ts), Some(id), more))
        .unwrap_or((None, None, false));

    Ok(Json(PageResp {
        items: blocks,
        next_after_ts,
        next_after_id,
        has_more,
    }))
}

pub async fn get_plain_history(
    State(app): State<AppState>,
    Query(q): Query<PageQ>,
) -> Result<Json<PageResp<pms_wire::WireBlock>>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(200).min(500);
    let nid = app._cfg.network.network_id.clone();
    let pv = app._cfg.network.protocol_version;

    let (ids, next_cursor) = app
        .store
        .recent_ids_by_time(q.after_ts, q.after_id.clone(), limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let stored = app
        .store
        .get_blocks_by_ids(&ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut blocks: Vec<pms_wire::WireBlock> = stored
        .into_iter()
        .map(|sb| pms_wire::WireBlock {
            id: sb.id,
            parents: sb.parents,
            payload_json: sb.payload_json,
            nonce: sb.nonce,
            network_id: nid.clone(),
            protocol_version: pv as u16,
            signer_pk_hex: String::new(),
            signature_hex: String::new(),
            metadata: sb.metadata,
        })
        .collect();

    // ne garder que les Plain (aka Mint pour l'instant)
    blocks.retain(|wb| {
        wb.payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
            .map(|env| matches!(env, PayloadEnvelope::Plain(_)))
            .unwrap_or(false)
    });

    let (next_after_ts, next_after_id, has_more) = next_cursor
        .map(|(ts, id, more)| (Some(ts), Some(id), more))
        .unwrap_or((None, None, false));

    Ok(Json(PageResp {
        items: blocks,
        next_after_ts,
        next_after_id,
        has_more,
    }))
}

// ===============================================================
// /wallet/history - Get transaction history for an address (Plain payloads)
// ===============================================================

use pms_wallet::history::history_plain_for_address;
use serde_json::Value;

#[derive(Deserialize)]
pub struct WalletHistoryQ {
    pub bech32_addr: String,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct HistoryItem {
    pub block_id: String,
    pub ts_ms: i64,
    pub payload_type: String, // "Mint" or "TxUtxo"
    pub payload: Value,       // The plain payload as JSON
}

#[derive(Serialize)]
pub struct WalletHistoryResp {
    pub address: String,
    pub items: Vec<HistoryItem>,
    pub count: usize,
}

pub async fn get_wallet_history(
    State(app): State<AppState>,
    Json(q): Json<WalletHistoryQ>,
) -> Result<Json<WalletHistoryResp>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(100).min(500);

    let entries = history_plain_for_address(&app.store, &q.bech32_addr, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items: Vec<HistoryItem> = entries
        .into_iter()
        .map(|e| {
            let (payload_type, payload) = match &e.plain {
                pms_types_payload::PlainPayload::Mint { outputs } => (
                    "Mint".to_string(),
                    serde_json::json!({ "outputs": outputs }),
                ),
                pms_types_payload::PlainPayload::TxUtxo(tx) => (
                    "TxUtxo".to_string(),
                    serde_json::to_value(tx).unwrap_or_default(),
                ),
                pms_types_payload::PlainPayload::Reward {
                    fee_outputs,
                    reward_outputs,
                    burned,
                    tx_block_id,
                } => (
                    "Reward".to_string(),
                    serde_json::json!({
                        "fee_outputs": fee_outputs,
                        "reward_outputs": reward_outputs,
                        "burned": burned,
                        "tx_block_id": tx_block_id
                    }),
                ),
                pms_types_payload::PlainPayload::EncryptedReward {
                    encrypted_outputs,
                    burned,
                    tx_block_id,
                } => (
                    "EncryptedReward".to_string(),
                    serde_json::json!({
                        "encrypted_outputs": encrypted_outputs,
                        "burned": burned,
                        "tx_block_id": tx_block_id
                    }),
                ),
                pms_types_payload::PlainPayload::Nft(action) => (
                    "Nft".to_string(),
                    serde_json::to_value(action).unwrap_or_default(),
                ),
                _ => ("Unknown".to_string(), serde_json::json!({})),
            };
            HistoryItem {
                block_id: e.id,
                ts_ms: e.ts_ms,
                payload_type,
                payload,
            }
        })
        .collect();

    let count = items.len();
    Ok(Json(WalletHistoryResp {
        address: q.bech32_addr,
        items,
        count,
    }))
}
