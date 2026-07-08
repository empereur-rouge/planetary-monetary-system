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
use crate::api_error::ApiError;
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

/// Forge un bloc plain signé Coordinator + persiste. Renvoie le `block_id` sur
/// succès, ou une [`ApiError`] à code numérique stable. Facteur commun des
/// handlers marketplace (settle, royalty update) — le shaping de la réponse de
/// succès (fee accumulation, corps JSON) reste propre à chaque appelant.
///
/// Mapping des rejets consensus : un `PutResult::Rejected` est une VIOLATION des
/// règles de consensus (royalty sous-payée, signature d'autorisation invalide,
/// double-spend) → `ApiError::Conflict` (3070, message public vague, raison
/// interne loggée). Le custodial (creator-studio) tient les deux clés et ne
/// devrait jamais l'atteindre en pratique ; le `label` distingue les deux flux
/// dans les logs.
async fn forge_persist_plain(
    state: &AppState,
    payload: PlainPayload,
    label: &str,
) -> Result<String, ApiError> {
    let parents = tx_helpers::get_block_parents(&state.store, &state.settings)
        .await
        .map_err(|e| ApiError::Internal {
            reason: format!("{label} parents: {e}"),
        })?;
    let wb = tx_helpers::forge_and_sign_block(
        Some(PayloadEnvelope::Plain(payload)),
        parents,
        &state.srv.adapter_arc(),
        &state.node_wallet,
        &state.settings,
        Some(label),
    )
    .await
    .map_err(|e| ApiError::Internal {
        reason: format!("{label} forge: {e}"),
    })?;
    match tx_helpers::persist_and_broadcast(state, &wb).await {
        Ok(PutResult::Inserted) => Ok(wb.id),
        Ok(PutResult::AlreadyExists) => Err(ApiError::AlreadyExists {
            kind: "block",
            id: wb.id,
        }),
        Ok(PutResult::Rejected(r)) => Err(ApiError::Conflict(format!("{label} rejected: {r}"))),
        Err(e) => Err(ApiError::StorageError {
            reason: format!("{label} persist: {e}"),
        }),
    }
}

pub async fn market_settle(
    State(state): State<AppState>,
    Json(req): Json<SettleRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // 0) Gas pool (custom ledgers only).
    tx_helpers::try_consume_gas(&state).map_err(|_| ApiError::GasPoolEmpty(state.ledger_id.clone()))?;

    // 1) Wallets + adresses.
    let seller_wallet = wallet_from_b64(&req.seller_private_key_b64).map_err(|e| {
        ApiError::InvalidField {
            field: "seller_private_key_b64",
            reason: e.to_string(),
        }
    })?;
    let buyer_wallet = wallet_from_b64(&req.buyer_private_key_b64).map_err(|e| {
        ApiError::InvalidField {
            field: "buyer_private_key_b64",
            reason: e.to_string(),
        }
    })?;
    let hrp = &state.settings.address.hrp;
    let seller = seller_wallet.get_address(hrp);
    let buyer = buyer_wallet.get_address(hrp);

    // 2) Montants + sanity (le validateur re-vérifie tout au consensus).
    let max_amount = Decimal::from(market::MAX_SETTLEMENT_AMOUNT);
    let quantity = match Decimal::from_str_exact(&req.quantity) {
        Ok(d) if d > Decimal::ZERO && d <= max_amount => d,
        _ => {
            return Err(ApiError::InvalidAmount {
                reason: "quantity must be a positive decimal within range".into(),
            });
        }
    };
    let price = match Decimal::from_str_exact(&req.price) {
        Ok(d) if d > Decimal::ZERO && d <= max_amount => d,
        _ => {
            return Err(ApiError::InvalidAmount {
                reason: "price must be a positive decimal within range".into(),
            });
        }
    };
    if seller == buyer {
        return Err(ApiError::InvalidField {
            field: "buyer",
            reason: "buyer and seller must differ".into(),
        });
    }
    if req.price_asset.as_deref() == Some(req.asset_sold.as_str()) {
        return Err(ApiError::InvalidField {
            field: "price_asset",
            reason: "asset_sold and price_asset must differ".into(),
        });
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
                None => {
                    return Err(ApiError::InvalidAmount {
                        reason: "price too large: royalty computation overflow".into(),
                    });
                }
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
    let (item_inputs, item_sum) = tx_helpers::select_utxos(&adapter, &seller, quantity, &sold)
        .await
        .map_err(|_| ApiError::InsufficientBalance {
            addr: seller.clone(),
            asset_id: sold.clone(),
            requested: quantity.to_string(),
            available: "0".into(),
        })?;
    let item_change = item_sum - quantity;

    // 5.b) Paiement de l'ACHETEUR (+ gas selon l'asset de paiement).
    let pms_native_price = price_asset.is_none();
    let buyer_pay_target = if pms_native_price { price + gas } else { price };
    let (pay_inputs, pay_sum) =
        tx_helpers::select_utxos(&adapter, &buyer, buyer_pay_target, &price_asset)
            .await
            .map_err(|_| ApiError::InsufficientBalance {
                addr: buyer.clone(),
                asset_id: price_asset.clone(),
                requested: buyer_pay_target.to_string(),
                available: "0".into(),
            })?;

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
    let tx_hash = unsigned
        .signing_message(&state.settings.network.network_id)
        .map_err(|e| ApiError::Internal {
            reason: format!("tx hash: {e}"),
        })?;

    // 9) Co-signature : le MÊME message est signé par les DEUX parties ; chaque
    //    input porte l'unlock de SON propriétaire (vendeur pour l'item, acheteur
    //    pour le paiement/gas) — appariement positionnel `input[i] ↔ unlock[i]`.
    let seller_sig = seller_wallet.sign(&tx_hash).map_err(|e| ApiError::Internal {
        reason: format!("seller sign: {e:?}"),
    })?;
    let buyer_sig = buyer_wallet.sign(&tx_hash).map_err(|e| ApiError::Internal {
        reason: format!("buyer sign: {e:?}"),
    })?;
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

    let block_id = forge_persist_plain(&state, payload, "MarketSettle").await?;
    tx_helpers::accumulate_tx_fee(&state, gas).await;
    Ok((
        StatusCode::CREATED,
        Json(json!(SettleResponse {
            block_id,
            royalty: royalty.to_string(),
            royalty_beneficiary: beneficiary,
            net_to_seller: net_to_seller.to_string(),
            fee: gas.to_string(),
        })),
    ))
}

// ═══════════════════════════════════════════════════════════════════════════
// Royalty mutable post-mint — AUTORISÉE PAR LA CO-SIGNATURE DU BÉNÉFICIAIRE
// COURANT (protocole 2.7). Endpoints /v1 (API-key), PAS admin : une instance
// squelette custodiale (sans token admin) change la royalty avec la clé du
// bénéficiaire qu'elle détient. Le consensus rejette sans signature valide.
// ═══════════════════════════════════════════════════════════════════════════

/// Deltas sur la politique royalty courante (partagé par prepare + update).
#[derive(Debug, Deserialize)]
pub struct RoyaltyChangeRequest {
    pub asset_id: String,
    #[serde(default)]
    pub royalty_bps: Option<u32>,
    #[serde(default)]
    pub royalty_beneficiary: Option<String>,
    #[serde(default)]
    pub clear_beneficiary: bool,
    #[serde(default)]
    pub clear_royalty: bool,
}

/// Requête `POST /v1/royalty/update` : deltas + **autorisation** — soit la clé
/// privée custodiée du bénéficiaire courant (le serveur signe), soit une
/// signature pré-calculée `auth_pubkey_hex`+`auth_signature_b64` (le squelette
/// signe localement, le coordinateur ne voit jamais la clé).
#[derive(Debug, Deserialize)]
pub struct RoyaltyUpdateRequest {
    pub asset_id: String,
    #[serde(default)]
    pub royalty_bps: Option<u32>,
    #[serde(default)]
    pub royalty_beneficiary: Option<String>,
    #[serde(default)]
    pub clear_beneficiary: bool,
    #[serde(default)]
    pub clear_royalty: bool,
    /// Voie custodiale : clé privée base64 du bénéficiaire COURANT (le serveur
    /// vérifie qu'elle correspond au bénéficiaire courant, puis signe).
    #[serde(default)]
    pub authorizer_private_key_b64: Option<String>,
    /// Voie pré-signée : pubkey sec1 hex du bénéficiaire courant.
    #[serde(default)]
    pub auth_pubkey_hex: Option<String>,
    /// Voie pré-signée : signature base64 sur `royalty_update_signing_message`.
    #[serde(default)]
    pub auth_signature_b64: Option<String>,
}

/// Nouvelles valeurs absolues = politique courante + deltas.
fn resolve_new_policy(
    current: &TokenMetadata,
    royalty_bps: Option<u32>,
    royalty_beneficiary: &Option<String>,
    clear_royalty: bool,
    clear_beneficiary: bool,
) -> (Option<u32>, Option<String>) {
    let new_bps = if clear_royalty {
        None
    } else {
        royalty_bps.or(current.royalty_bps).filter(|b| *b > 0)
    };
    let new_beneficiary = if clear_beneficiary {
        None
    } else {
        royalty_beneficiary
            .clone()
            .or_else(|| current.royalty_beneficiary.clone())
    };
    (new_bps, new_beneficiary)
}

/// Adresse dont la clé DOIT signer un changement = bénéficiaire explicite courant,
/// à défaut le `creator`. (Miroir exact du check consensus dans persist.)
fn current_authorizer_addr(current: &TokenMetadata) -> String {
    current
        .royalty_beneficiary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(current.creator.as_str())
        .to_string()
}

/// `POST /v1/royalty/prepare` — renvoie le message canonique à SIGNER (par la clé
/// du bénéficiaire courant) pour autoriser le changement, sans rien produire.
/// Permet la voie non-custodiale (le squelette signe localement).
pub async fn royalty_prepare(
    State(state): State<AppState>,
    Json(req): Json<RoyaltyChangeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let current = resolve_meta(&state, &req.asset_id).ok_or_else(|| ApiError::NotFound {
        kind: "asset",
        id: req.asset_id.clone(),
    })?;
    let (new_bps, new_beneficiary) = resolve_new_policy(
        &current,
        req.royalty_bps,
        &req.royalty_beneficiary,
        req.clear_royalty,
        req.clear_beneficiary,
    );
    pms_types::validate_royalty_fields(new_bps, new_beneficiary.as_deref()).map_err(|e| {
        ApiError::InvalidField {
            field: "royalty",
            reason: e,
        }
    })?;
    let message_hex = pms_types::royalty_update_signing_message(
        &state.settings.network.network_id,
        &req.asset_id,
        new_bps,
        new_beneficiary.as_deref(),
        current.royalty_version,
    );
    Ok((
        StatusCode::OK,
        Json(json!({
            "asset_id": req.asset_id,
            "current_beneficiary": current_authorizer_addr(&current),
            "current_royalty_version": current.royalty_version,
            "new_royalty_bps": new_bps,
            "new_royalty_beneficiary": new_beneficiary,
            "message_hex": message_hex,
        })),
    ))
}

/// `POST /v1/royalty/update` — applique le changement de royalty, **autorisé par
/// la signature du bénéficiaire courant** (custodial ou pré-signée). Aucun token
/// admin requis. Le consensus RE-VÉRIFIE la signature (défense en profondeur).
pub async fn royalty_update(
    State(state): State<AppState>,
    Json(req): Json<RoyaltyUpdateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let current = resolve_meta(&state, &req.asset_id).ok_or_else(|| ApiError::NotFound {
        kind: "asset",
        id: req.asset_id.clone(),
    })?;
    let (new_bps, new_beneficiary) = resolve_new_policy(
        &current,
        req.royalty_bps,
        &req.royalty_beneficiary,
        req.clear_royalty,
        req.clear_beneficiary,
    );
    pms_types::validate_royalty_fields(new_bps, new_beneficiary.as_deref()).map_err(|e| {
        ApiError::InvalidField {
            field: "royalty",
            reason: e,
        }
    })?;
    let authorizer = current_authorizer_addr(&current);
    let msg = pms_types::royalty_update_signing_message(
        &state.settings.network.network_id,
        &req.asset_id,
        new_bps,
        new_beneficiary.as_deref(),
        current.royalty_version,
    );

    // Obtenir (auth_pubkey_hex, auth_signature_b64).
    let (auth_pubkey_hex, auth_signature_b64) = if let Some(pk_b64) = &req.authorizer_private_key_b64
    {
        // Voie custodiale : reconstruit le wallet, vérifie qu'il EST le
        // bénéficiaire courant (403 Forbidden sinon), puis signe.
        let wallet = wallet_from_b64(pk_b64).map_err(|e| ApiError::InvalidField {
            field: "authorizer_private_key_b64",
            reason: e.to_string(),
        })?;
        if !pms_core::validations::ownership::unlock_matches_address(
            &wallet.public_key_hex,
            &authorizer,
        ) {
            return Err(ApiError::Forbidden {
                reason: "provided key is not the current royalty beneficiary".into(),
            });
        }
        let sig = wallet.sign(&msg).map_err(|e| ApiError::Internal {
            reason: format!("authorizer sign failed: {e:?}"),
        })?;
        (wallet.public_key_hex.clone(), sig)
    } else if let (Some(pk), Some(sig)) = (&req.auth_pubkey_hex, &req.auth_signature_b64) {
        // Voie pré-signée : le coordinateur ne voit jamais la clé. Persist vérifie.
        (pk.clone(), sig.clone())
    } else {
        return Err(ApiError::InvalidField {
            field: "authorization",
            reason: "provide authorizer_private_key_b64 OR (auth_pubkey_hex + auth_signature_b64)"
                .into(),
        });
    };

    let payload = PlainPayload::RoyaltyUpdate {
        asset_id: req.asset_id.clone(),
        royalty_bps: new_bps,
        royalty_beneficiary: new_beneficiary.clone(),
        auth_pubkey_hex,
        auth_signature_b64,
    };
    let block_id = forge_persist_plain(&state, payload, "RoyaltyUpdate").await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "asset_id": req.asset_id,
            "royalty_bps": new_bps,
            "royalty_beneficiary": new_beneficiary,
            "block_id": block_id,
        })),
    ))
}
