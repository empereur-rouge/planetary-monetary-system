use crate::api::AppState;
use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use pms_wallet::decode_address;

#[derive(serde::Serialize)]
pub struct Outpoint {
    pub txid: String,
    pub index: u32,
}

#[derive(serde::Serialize)]
pub struct UtxoItem {
    pub address: String,
    pub amount: String,
    pub outpoint: Outpoint,
}

#[derive(serde::Serialize)]
pub struct UtxoResp {
    pub utxos: Vec<UtxoItem>,
}

#[derive(serde::Deserialize)]
pub struct BalanceReq {
    bech32_addr: String,
    // Kept for backward compatibility (no longer used server-side)
    #[allow(dead_code)]
    x25519_sk_hex: String,
    #[allow(dead_code)]
    ecdsa_pk_hex: String,
    #[allow(dead_code)]
    scan_limit: Option<usize>,
}
#[derive(serde::Serialize)]
pub struct UtxoView {
    txid: String,
    index: u32,
    amount: String,
}
#[derive(serde::Serialize)]
pub struct BalanceResp {
    balance: String,
    utxos: Vec<UtxoView>,
}

pub async fn wallet_balance(
    State(app): State<AppState>,
    Json(req): Json<BalanceReq>,
) -> Result<Json<BalanceResp>, (StatusCode, String)> {
    // Validate address
    let (_h20, _xpk_hex) = decode_address(&req.bech32_addr)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid address: {e}")))?;

    // Balance from O(1) cache + UTXO list from shard-batched lookup
    let adapter = app.srv.adapter_arc();
    let balance = adapter.balance_by_address(&req.bech32_addr).await;
    let utxos = adapter.utxos_by_address(&req.bech32_addr).await;

    let list = utxos
        .into_iter()
        .map(|(oid, txo)| UtxoView {
            txid: oid.txid,
            index: oid.index,
            amount: txo.amount,
        })
        .collect::<Vec<_>>();

    Ok(Json(BalanceResp {
        balance: balance.to_string(),
        utxos: list,
    }))
}

/// Simple balance query by address.
///
/// Supports optional `ledger_id` to query a custom ledger's balance from the
/// main endpoint, and optional `asset_id` to query custom token balances.
///
/// # Examples
/// ```json
/// { "address": "8e1abc..." }                                       // PMS on current ledger
/// { "address": "8e1abc...", "asset_id": "edenite" }                // EDN on current ledger
/// { "address": "8e1abc...", "ledger_id": "eden" }                  // PMS on eden
/// { "address": "8e1abc...", "ledger_id": "eden", "asset_id": "edenite" } // EDN on eden
/// ```
#[derive(serde::Deserialize)]
pub struct SimpleBalanceReq {
    address: String,
    /// Optional: query a specific ledger (e.g. "eden"). If omitted, uses the
    /// current ledger (main, or the one from the `/l/{id}/` URL prefix).
    ledger_id: Option<String>,
    /// Optional: query balance for a specific asset (e.g. "edenite").
    /// If omitted, returns native PMS balance.
    asset_id: Option<String>,
}
#[derive(serde::Serialize)]
pub struct SimpleBalanceResp {
    balance: String,
    /// Echoes back the ledger that was queried.
    ledger_id: String,
    /// Echoes back the asset that was queried (null = PMS native).
    asset_id: Option<String>,
}

/// `POST /v1/balance` — query wallet balance by address, with optional
/// `ledger_id` and `asset_id` parameters.
pub async fn balance_by_address(
    State(app): State<AppState>,
    Json(req): Json<SimpleBalanceReq>,
) -> Result<Json<SimpleBalanceResp>, (StatusCode, String)> {
    let effective_ledger = req.ledger_id.as_deref().unwrap_or(&app.ledger_id);

    // Resolve the adapter for the target ledger.
    let adapter = if req.ledger_id.is_some() && effective_ledger != app.ledger_id {
        // Cross-ledger query — look up from LedgerManager
        let mgr = app.ledger_mgr.as_ref().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "multi-ledger not enabled".to_string(),
            )
        })?;
        let instance = mgr.get(effective_ledger).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("ledger '{}' not found", effective_ledger),
            )
        })?;
        instance.adapter.clone()
    } else {
        app.srv.adapter_arc()
    };

    let balance = adapter
        .balance_by_address_and_asset(&req.address, req.asset_id.as_deref())
        .await;

    Ok(Json(SimpleBalanceResp {
        balance: balance.to_string(),
        ledger_id: effective_ledger.to_string(),
        asset_id: req.asset_id,
    }))
}

/// Response format for UTXOs — flat structure returned by `GET /v1/wallet/{address}/utxos`.
///
/// Includes `asset_id` for multi-asset support (PMS native = `null`, custom token = `"edenite"`).
#[derive(serde::Serialize)]
pub struct UtxoFlatItem {
    #[serde(rename = "txId")]
    pub tx_id: String,
    #[serde(rename = "outIdx")]
    pub out_idx: u32,
    pub amount: String,
    pub address: String,
    /// `null` for PMS native, `"edenite"` (etc.) for custom tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    /// Time-lock (protocole 2.1) : timestamp UNIX ms avant lequel l'UTXO est
    /// indépensable. Absent = dépensable immédiatement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_until: Option<u64>,
    /// Condition de déverrouillage (protocole 2.2) — absent = PubKey simple.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_condition: Option<pms_types::SpendCondition>,
    /// Timestamp de création système (protocole 2.5, base du demurrage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
}

#[derive(serde::Serialize)]
pub struct UtxoFlatResp {
    pub utxos: Vec<UtxoFlatItem>,
}

pub async fn get_utxos_by_address(
    State(app): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<UtxoFlatResp>, (StatusCode, String)> {
    // Read from in-memory UTXO set (RAM) instead of RocksDB for consistency with /v1/balance
    let utxos = app.srv.adapter_arc().utxos_by_address(&address).await;

    let list = utxos
        .into_iter()
        .map(|(output_id, tx_output)| UtxoFlatItem {
            tx_id: output_id.txid,
            out_idx: output_id.index,
            amount: tx_output.amount,
            address: tx_output.address,
            asset_id: tx_output.asset_id,
            locked_until: tx_output.locked_until,
            spend_condition: tx_output.spend_condition,
            created_at: tx_output.created_at,
        })
        .collect();

    Ok(Json(UtxoFlatResp { utxos: list }))
}
