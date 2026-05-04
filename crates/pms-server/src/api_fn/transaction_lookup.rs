//! `GET /v1/transaction/{block_id}` — unified transaction lookup.
//!
//! Pre-existing endpoint `GET /v1/blocks/{id}` returns the raw `WireBlock`
//! and leaves the client to parse `payload_json`. The SaaS payment rail
//! needs a single call returning `{from, to, amount, fee, depth, is_finalized,
//! ...}` ready to render — that's this handler.
//!
//! In PMS, **1 TX = 1 block** (every signed transaction creates exactly one
//! DAG block), so the path parameter `{block_id}` is the public TX
//! identifier. The handler:
//!
//! 1. Fetches the block by id.
//! 2. Decodes `PlainPayload::TxUtxo(tx)` (other payload types → 404).
//! 3. Resolves each input UTXO to recover the sender address (`from`).
//! 4. Computes finality: `depth = count_descendants(block_id, k)` and
//!    `is_finalized = adapter.is_finalized(block_id)`.
//! 5. Returns a flat response. Encrypted payloads return `kind: "encrypted"`
//!    with no input/output details (server can't decrypt).

use crate::api::AppState;
use axum::extract::{Path, State};
use axum::{Json, http::StatusCode};
use pms_storage::DagStorage;
use pms_types::{PayloadEnvelope, PlainPayload, TxOutput};
use serde::Serialize;
use serde_json::{Value, json};

/// k-depth threshold used to compute the "depth" field. Capped here so a
/// popular old block doesn't BFS the whole DAG. The SaaS uses this as a
/// lower bound on confirmations — `depth >= K_DEPTH_CAP` means "at least
/// `K_DEPTH_CAP` descendants exist", not "exactly that many".
const K_DEPTH_CAP: usize = 64;

#[derive(Serialize, Debug)]
pub struct TxLookupInput {
    pub txid: String,
    pub index: u32,
    /// Resolved from the source UTXO; `None` if the parent block is no
    /// longer available (pruned / encrypted).
    pub address: Option<String>,
    /// Resolved input amount; `None` for the same reasons as `address`.
    pub amount: Option<String>,
    pub asset_id: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct TxLookupResponse {
    pub tx_hash: String,
    pub block_id: String,
    /// First resolvable input address (the "sender" for single-signer TXs).
    pub from: Option<String>,
    /// First output address (the "recipient" by PMS convention — change
    /// outputs come after the recipient).
    pub to: Option<String>,
    /// Amount of the first output. String decimal 8 places.
    pub amount: Option<String>,
    pub asset_id: Option<String>,
    pub fee: String,
    pub timestamp_ms: Option<i64>,
    pub is_finalized: bool,
    /// Number of distinct descendants observed (capped at `K_DEPTH_CAP`).
    pub depth: usize,
    /// `pending` (no descendants yet) | `confirmed` (>=1 descendant) |
    /// `finalized` (k-depth reached / milestone).
    pub status: &'static str,
    pub inputs: Vec<TxLookupInput>,
    pub outputs: Vec<TxOutput>,
}

pub async fn get_transaction_by_block_id(
    State(state): State<AppState>,
    Path(block_id): Path<String>,
) -> Result<Json<TxLookupResponse>, (StatusCode, Json<Value>)> {
    let stored = state
        .store
        .get_block(&block_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "code": 9001, "message": format!("storage error: {e}") })),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "code": 3040, "message": "transaction not found" })),
            )
        })?;

    let payload_str = stored.payload_json.as_deref().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "code": 3040, "message": "block has no payload" })),
        )
    })?;

    let envelope: PayloadEnvelope = serde_json::from_str(payload_str).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "code": 9001, "message": format!("malformed payload: {e}") })),
        )
    })?;

    // Encrypted payloads are private — the server has no key. Point the
    // caller to the per-wallet activity endpoint which decrypts using the
    // recipient's stored key.
    let plain = match envelope {
        PayloadEnvelope::Plain(p) => p,
        PayloadEnvelope::Encrypted(_) => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "code": 1010,
                    "message": "encrypted transaction (server cannot decrypt). Use GET /v1/wallet/{address}/activity which decrypts using the recipient's stored key."
                })),
            ));
        }
    };

    // Inputs only exist for `TxUtxo` (UTXO-spending transactions); Mint /
    // Reward / Bridge have no inputs.
    let (inputs, fee) = if let PlainPayload::TxUtxo(tx) = &plain {
        (resolve_inputs(&state, tx).await, tx.fee.clone())
    } else {
        (Vec::new(), "0".to_string())
    };

    // Single source of truth for "outputs created by this payload" — see
    // `PlainPayload::outputs` in pms-types-payload.
    let outputs = plain.outputs().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "code": 2030,
                "message": "block is not a transaction (Mint/TxUtxo/Reward) — use GET /v1/blocks/{id} for raw access"
            })),
        )
    })?;

    let from = inputs.iter().find_map(|i| i.address.clone());
    let first_out = outputs.first();
    let to = first_out.map(|o| o.address.clone());
    let amount = first_out.map(|o| o.amount.clone());
    let asset_id = first_out.and_then(|o| o.asset_id.clone());

    let adapter = state.srv.adapter_arc();
    let is_finalized = adapter.is_finalized(&block_id).await;
    let depth = adapter.count_descendants(&block_id, K_DEPTH_CAP).await;
    let status = if is_finalized {
        "finalized"
    } else if depth > 0 {
        "confirmed"
    } else {
        "pending"
    };

    let timestamp_ms = state
        .store
        .ts_for_ids(&[block_id.clone()])
        .await
        .ok()
        .and_then(|m| m.get(&block_id).copied());

    Ok(Json(TxLookupResponse {
        // PMS convention: 1 TX = 1 block, so block_id is the canonical TX id
        // exposed to clients. The cryptographic `tx_hash` (signing message
        // hash) is internal and not surfaced — it would just confuse SaaS
        // integrators.
        tx_hash: block_id.clone(),
        block_id,
        from,
        to,
        amount,
        asset_id,
        fee,
        timestamp_ms,
        is_finalized,
        depth,
        status,
        inputs,
        outputs,
    }))
}

/// Resolve each `TxInput` to the address+amount of the parent block's
/// matching output. Best-effort: missing parents (pruned / encrypted) yield
/// `None` fields rather than failing the whole lookup.
async fn resolve_inputs(
    state: &AppState,
    tx: &pms_types::Transaction,
) -> Vec<TxLookupInput> {
    let mut out = Vec::with_capacity(tx.inputs.len());
    for input in &tx.inputs {
        let txid = input.out.txid.clone();
        let index = input.out.index;
        let mut addr = None;
        let mut amount = None;
        let mut asset_id = None;

        if let Ok(Some(parent)) = state.store.get_block(&txid).await {
            if let Some(p_json) = parent.payload_json.as_deref() {
                if let Ok(PayloadEnvelope::Plain(plain)) =
                    serde_json::from_str::<PayloadEnvelope>(p_json)
                {
                    if let Some(outputs) = plain.outputs() {
                        if let Some(o) = outputs.get(index as usize) {
                            addr = Some(o.address.clone());
                            amount = Some(o.amount.clone());
                            asset_id = o.asset_id.clone();
                        }
                    }
                }
            }
        }

        out.push(TxLookupInput {
            txid,
            index,
            address: addr,
            amount,
            asset_id,
        });
    }
    out
}
