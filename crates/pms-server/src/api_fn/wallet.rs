use crate::api::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use pms_config::load_config;
use pms_wallet::decode_address;
use pms_wallet::utxo_store::gather_wallet_utxos_dec;
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
    x25519_sk_hex: String,
    ecdsa_pk_hex: String,
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
    let settings = load_config().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let hrp = settings.address.hrp;

    // déduire x25519 pub depuis l’adresse pour sanity check (optionnel)
    let (_h20, xpk_hex) = decode_address(&req.bech32_addr)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid address: {e}")))?;

    let scan = req.scan_limit.unwrap_or(2000);

    let utxos = gather_wallet_utxos_dec(
        &app.store,
        &req.ecdsa_pk_hex, // Pass true public key from request
        &xpk_hex,
        &req.x25519_sk_hex,
        &hrp,
        scan,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use rust_decimal::Decimal;
    let mut sum = Decimal::ZERO;
    let list = utxos
        .into_iter()
        .map(|u| {
            sum += u.amount;
            UtxoView {
                txid: u.txid,
                index: u.index,
                amount: u.amount.to_string(),
            }
        })
        .collect::<Vec<_>>();

    Ok(Json(BalanceResp {
        balance: sum.to_string(),
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
