use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use pms_config::load_config;
use pms_wallet::decode_address;
use pms_wallet::utxo_store::gather_wallet_utxos_dec;
use crate::api::AppState;

#[derive(serde::Deserialize)]
struct BalanceReq { bech32_addr: String, x25519_sk_hex: String, scan_limit: Option<usize> }
#[derive(serde::Serialize)]
struct UtxoView { txid: String, index: u32, amount: String }
#[derive(serde::Serialize)]
struct BalanceResp { balance: String, utxos: Vec<UtxoView> }

async fn wallet_balance(
    State(app): State<AppState>,
    Json(req): Json<BalanceReq>,
) -> Result<Json<BalanceResp>, (StatusCode, String)> {
    let settings = load_config().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let hrp = settings.address.hrp;
    // déduire pub ECDSA & x25519 pub depuis l’adresse
    let (_h20, xpk_hex) = decode_address(&req.bech32_addr)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid address: {e}")))?;
    // ⚠️ côté serveur on n’a pas la pub ECDSA du wallet → le client l’envoie dans l’adresse (hash20)
    // pour les candidats on se limite à l’adresse bech32 donnée
    let scan = req.scan_limit.unwrap_or(2000);

    let utxos = gather_wallet_utxos_dec(
        &app.store,
        /*wallet_pub_hex=*/"",          // inconnu ici, pas requis si on matche sur addr bech32
        &xpk_hex,
        &req.x25519_sk_hex,
        &hrp,
        scan,
    ).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use rust_decimal::Decimal;
    let mut sum = Decimal::ZERO;
    let list = utxos.into_iter().map(|u| {
        sum += u.amount;
        UtxoView { txid: u.txid, index: u.index, amount: u.amount.to_string() }
    }).collect::<Vec<_>>();

    Ok(Json(BalanceResp { balance: sum.to_string(), utxos: list }))
}