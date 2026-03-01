use crate::api::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use pms_wallet::decode_address;
use axum::extract::Path;

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

/// Simple balance query by address only (no keys needed)
/// Uses the RAM UTXO set directly
#[derive(serde::Deserialize)]
pub struct SimpleBalanceReq {
    address: String,
}
#[derive(serde::Serialize)]
pub struct SimpleBalanceResp {
    balance: String,
}

pub async fn balance_by_address(
    State(app): State<AppState>,
    Json(req): Json<SimpleBalanceReq>,
) -> Result<Json<SimpleBalanceResp>, (StatusCode, String)> {
    let balance: rust_decimal::Decimal =
        app.srv.adapter_arc().balance_by_address(&req.address).await;
    Ok(Json(SimpleBalanceResp {
        balance: balance.to_string(),
    }))
}

/// Response format for UTXOs - matches the expected test format
#[derive(serde::Serialize)]
pub struct UtxoFlatItem {
    #[serde(rename = "txId")]
    pub tx_id: String,
    #[serde(rename = "outIdx")]
    pub out_idx: u32,
    pub amount: String,
    pub address: String,
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
        })
        .collect();

    Ok(Json(UtxoFlatResp { utxos: list }))
}
