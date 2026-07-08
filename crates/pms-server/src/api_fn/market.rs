//! Marketplace settlement endpoint (protocole 2.7).
//!
//! `POST /v1/market/settle` — **one-shot custodial** atomic sale/resale.
//!
//! creator-studio (custodial) fournit les clés privées du vendeur ET de
//! l'acheteur + les paramètres de vente ; le serveur :
//!   1. résout la politique royalty de l'`asset_sold` (registre : token OU
//!      classe SFT), calcule `royalty = price × bps / 10000` (dans l'asset de
//!      PAIEMENT, pas forcément PMS) ;
//!   2. sélectionne les UTXOs (item du vendeur, paiement + gas de l'acheteur) ;
//!   3. construit UNE transaction UTXO : item→acheteur, royalty→bénéficiaire,
//!      reste→vendeur, changes ; gas PMS brûlé ;
//!   4. la fait co-signer (vendeur sur ses inputs item, acheteur sur ses inputs
//!      paiement/gas) — deux signatures sur le MÊME `signing_message` ;
//!   5. l'emballe dans un [`PlainPayload::MarketSettle`] (payload **plain**,
//!      vente publique-by-design) et forge un bloc signé Coordinator.
//!
//! La royalty est **enforced au consensus** : `do_persist_block_internal`
//! ré-dérive la politique du registre et REJETTE tout settlement dont les
//! outputs ne respectent pas la forme — impossible de sous-payer le créateur,
//! même si ce handler était contourné.

use crate::api::AppState;
use crate::api_fn::tx_helpers;
use crate::api_fn::wallet_factory::wallet_from_b64;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use pms_core::validations::market;
use pms_storage::{PutResult, SftClassStorage};
use pms_types::{PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock};
use pms_types_payload::TokenMetadata;
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Requête `POST /v1/market/settle`.
#[derive(Debug, Deserialize)]
pub struct SettleRequest {
    /// Clé privée base64 du VENDEUR (custodiée) — signe la cession de l'item.
    pub seller_private_key_b64: String,
    /// Clé privée base64 de l'ACHETEUR (custodiée) — signe le paiement.
    pub buyer_private_key_b64: String,
    /// Asset vendu : token `asset_id` OU classe SFT `"collection:class"`.
    pub asset_sold: String,
    /// Quantité de l'item (décimal string, > 0). Typiquement `"1"`.
    pub quantity: String,
    /// Asset de paiement (`None` = PMS natif, `Some(x)` = token/SFT quelconque).
    #[serde(default)]
    pub price_asset: Option<String>,
    /// Prix total payé par l'acheteur au vendeur, AVANT split royalty (> 0).
    pub price: String,
}

/// Réponse `POST /v1/market/settle`.
#[derive(Debug, Serialize)]
pub struct SettleResponse {
    pub block_id: String,
    /// Montant royalty prélevé, dans l'asset de paiement (`"0"` si aucune).
    pub royalty: String,
    /// Bénéficiaire de la royalty (`null` si aucune).
    pub royalty_beneficiary: Option<String>,
    /// Reste versé au vendeur (`price − royalty`), dans l'asset de paiement.
    pub net_to_seller: String,
    /// Frais de gas PMS brûlé.
    pub fee: String,
}

/// Résout les métadonnées d'un asset custom (token OU classe SFT, vue
/// `TokenMetadata`). Mutuellement exclusifs (namespace `:`), donc au plus un
/// match. Miroir serveur de `CoreAdapter::resolve_asset_metadata`.
fn resolve_meta(state: &AppState, asset_id: &str) -> Option<TokenMetadata> {
    if let Ok(Some(m)) = state.store.get_token(asset_id) {
        return Some(m);
    }
    state
        .store
        .get_sft_class(asset_id)
        .ok()
        .flatten()
        .map(|c| c.to_token_metadata())
}

fn bad(msg: impl Into<String>) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.into() })))
}

pub async fn market_settle(
    State(state): State<AppState>,
    Json(req): Json<SettleRequest>,
) -> impl IntoResponse {
    // 0) Gas pool (custom ledgers only).
    if let Err(e) = tx_helpers::try_consume_gas(&state) {
        return (StatusCode::PAYMENT_REQUIRED, Json(json!({ "error": e })));
    }

    // 1) Wallets + adresses.
    let seller_wallet = match wallet_from_b64(&req.seller_private_key_b64) {
        Ok(w) => w,
        Err(e) => return bad(format!("invalid seller key: {e}")),
    };
    let buyer_wallet = match wallet_from_b64(&req.buyer_private_key_b64) {
        Ok(w) => w,
        Err(e) => return bad(format!("invalid buyer key: {e}")),
    };
    let hrp = &state.settings.address.hrp;
    let seller = seller_wallet.get_address(hrp);
    let buyer = buyer_wallet.get_address(hrp);

    // 2) Montants + sanity (le validateur re-vérifie tout au consensus).
    let max_amount = Decimal::from(market::MAX_SETTLEMENT_AMOUNT);
    let quantity = match Decimal::from_str_exact(&req.quantity) {
        Ok(d) if d > Decimal::ZERO && d <= max_amount => d,
        _ => return bad("quantity must be a positive decimal within range"),
    };
    let price = match Decimal::from_str_exact(&req.price) {
        Ok(d) if d > Decimal::ZERO && d <= max_amount => d,
        _ => return bad("price must be a positive decimal within range"),
    };
    if seller == buyer {
        return bad("buyer and seller must differ");
    }
    if req.price_asset.as_deref() == Some(req.asset_sold.as_str()) {
        return bad("asset_sold and price_asset must differ");
    }

    // 3) Politique royalty de l'ASSET VENDU (registre) → montant + bénéficiaire.
    //    Pas de politique → royalty 0 (swap atomique pur). Décimales du prix
    //    résolues via le MÊME chemin que le validateur (aucune divergence).
    let (royalty, beneficiary) = match resolve_meta(&state, &req.asset_sold)
        .and_then(|m| m.effective_royalty())
    {
        Some((bps, b)) => {
            let price_dec_places = market::price_decimals(
                req.price_asset
                    .as_deref()
                    .and_then(|a| resolve_meta(&state, a))
                    .map(|m| m.decimals),
            );
            // Shares the validator's exact formula; None = overflow (rejected there too).
            match market::compute_royalty(price, bps, price_dec_places) {
                Some(r) => (r, Some(b)),
                None => return bad("price too large: royalty computation overflow"),
            }
        }
        None => (Decimal::ZERO, None),
    };
    let net_to_seller = price - royalty;

    // 4) Frais de gas (PMS, brûlé) — calculé sur le prix, comme un transfert.
    let (fee_policy, _r) = tx_helpers::load_fee_policy(&state.store);
    let mut gas = fee_policy
        .compute_fee(&price.to_string())
        .map(|a| a.inner())
        .unwrap_or(Decimal::ZERO);

    let adapter = state.srv.adapter_arc();
    let price_asset = req.price_asset.clone();
    let sold = Some(req.asset_sold.clone());

    // 5) Sélection des UTXOs.
    // 5.a) Item du VENDEUR.
    let (item_inputs, item_sum) =
        match tx_helpers::select_utxos(&adapter, &seller, quantity, &sold).await {
            Ok(r) => r,
            Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": format!("seller item: {e}") }))),
        };
    let item_change = item_sum - quantity;

    // 5.b) Paiement de l'ACHETEUR (+ gas selon l'asset de paiement).
    let pms_native_price = price_asset.is_none();
    let buyer_pay_target = if pms_native_price { price + gas } else { price };
    let (pay_inputs, pay_sum) =
        match tx_helpers::select_utxos(&adapter, &buyer, buyer_pay_target, &price_asset).await {
            Ok(r) => r,
            Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": format!("buyer payment: {e}") }))),
        };

    // 5.c) Gas séparé en PMS pour un paiement en asset custom (waivable si l'acheteur
    //      n'a pas de PMS sur ce ledger — parité avec send-simple).
    let mut gas_inputs: Vec<(pms_types::OutputId, TxOutput, Decimal)> = Vec::new();
    let mut gas_change = Decimal::ZERO;
    if !pms_native_price && gas > Decimal::ZERO {
        match tx_helpers::select_utxos(&adapter, &buyer, gas, &None).await {
            Ok((gi, gs)) => {
                gas_change = gs - gas;
                gas_inputs = gi;
            }
            Err(_) => {
                tracing::info!(
                    "market_settle: no PMS for gas on ledger '{}', waiving fee",
                    state.ledger_id
                );
                gas = Decimal::ZERO;
            }
        }
    }

    // 6) Inputs (ordre : item[vendeur] ++ paiement[acheteur] ++ gas[acheteur]).
    //    On mémorise combien d'inputs appartiennent au vendeur pour l'unlock.
    let seller_input_count = item_inputs.len();
    let mut tx_inputs: Vec<TxInput> = Vec::new();
    for (oid, _, _) in item_inputs.iter().chain(pay_inputs.iter()).chain(gas_inputs.iter()) {
        tx_inputs.push(TxInput { out: oid.clone() });
    }

    // 7) Outputs. `push` n'émet que les montants strictement positifs (pas de
    //    sortie 0, pas de `.to_string()` répété).
    let mut tx_outputs: Vec<TxOutput> = Vec::new();
    let mut push = |addr: &str, amt: Decimal, asset: Option<String>| {
        if amt > Decimal::ZERO {
            tx_outputs.push(TxOutput::new(addr, amt.to_string(), asset));
        }
    };
    // Item → acheteur (quantity toujours > 0) ; change item → vendeur.
    push(&buyer, quantity, sold.clone());
    push(&seller, item_change, sold.clone());
    // Royalty → bénéficiaire, reste → vendeur (dans l'asset de paiement).
    // `beneficiary` est toujours `Some` quand `royalty > 0` (invariant §3) → le
    // guard `amt > 0` de `push` suffit, pas de branche imbriquée.
    if let Some(b) = &beneficiary {
        push(b, royalty, price_asset.clone());
    }
    push(&seller, net_to_seller, price_asset.clone());
    // Change de paiement → acheteur (PMS natif : le gas est brûlé, in − out).
    let pay_change = if pms_native_price { pay_sum - price - gas } else { pay_sum - price };
    push(&buyer, pay_change, price_asset.clone());
    // Change de gas PMS → acheteur (paiement en asset custom seulement).
    push(&buyer, gas_change, None);

    // 8) Transaction non-signée + hash canonique.
    let unsigned = Transaction {
        inputs: tx_inputs,
        outputs: tx_outputs,
        fee: gas.to_string(),
        unlocks: vec![],
    };
    let tx_hash = match unsigned.signing_message(&state.settings.network.network_id) {
        Ok(h) => h,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("tx hash: {e}") }))),
    };

    // 9) Co-signature : le MÊME message est signé par les DEUX parties ; chaque
    //    input porte l'unlock de SON propriétaire (vendeur pour l'item, acheteur
    //    pour le paiement/gas) — appariement positionnel `input[i] ↔ unlock[i]`.
    let seller_sig = match seller_wallet.sign(&tx_hash) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("seller sign: {e:?}") }))),
    };
    let buyer_sig = match buyer_wallet.sign(&tx_hash) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("buyer sign: {e:?}") }))),
    };
    let unlocks: Vec<Unlock> = (0..unsigned.inputs.len())
        .map(|i| {
            if i < seller_input_count {
                Unlock::new(seller_wallet.public_key_hex.clone(), seller_sig.clone())
            } else {
                Unlock::new(buyer_wallet.public_key_hex.clone(), buyer_sig.clone())
            }
        })
        .collect();
    let signed = Transaction { unlocks, ..unsigned };

    // 10) Payload MarketSettle (PLAIN) + forge bloc Coordinator + persist.
    let payload = PlainPayload::MarketSettle {
        tx: signed,
        asset_sold: req.asset_sold.clone(),
        quantity: quantity.to_string(),
        price_asset: price_asset.clone(),
        price: price.to_string(),
        seller: seller.clone(),
        buyer: buyer.clone(),
    };

    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e }))),
    };
    let wb = match tx_helpers::forge_and_sign_block(
        Some(PayloadEnvelope::Plain(payload)),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("MarketSettle"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))),
    };

    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => {
            tx_helpers::accumulate_tx_fee(&state, gas).await;
            (
                StatusCode::CREATED,
                Json(json!(SettleResponse {
                    block_id: wb.id,
                    royalty: royalty.to_string(),
                    royalty_beneficiary: beneficiary,
                    net_to_seller: net_to_seller.to_string(),
                    fee: gas.to_string(),
                })),
            )
        }
        Ok(PutResult::AlreadyExists) => (StatusCode::CONFLICT, Json(json!({ "error": "block already exists" }))),
        Ok(PutResult::Rejected(reason)) => (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("rejected: {reason}") }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))),
    }
}
