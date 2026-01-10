use crate::api::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use pms_config::load_config;
use pms_wallet::decode_address;
use pms_wallet::utxo_store::gather_wallet_utxos_dec;

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
