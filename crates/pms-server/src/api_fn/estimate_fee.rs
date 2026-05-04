//! `POST /v1/estimate-fee` — calcule le fee total qui s'appliquerait à un
//! transfert, sans construire de transaction. Pure compute (pas de side
//! effects, pas de UTXO selection) — beaucoup moins cher que `prepare_tx`.
//!
//! Le SaaS payment rail l'appelle pour informer l'utilisateur du fee final
//! AVANT de lui demander de signer. Combine le `FeePolicy` natif (linéaire
//! ou par paliers) et les frais de smart contract `OnTransfer` (cf.
//! [[smart-contracts]]).

use crate::api::AppState;
use axum::extract::State;
use axum::{Json, http::StatusCode};
use pms_contracts::evaluate_transfer;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;

use super::tx_helpers;

#[derive(Deserialize, Debug)]
pub struct EstimateFeeRequest {
    /// Montant du transfert (string décimal pour préserver la précision).
    pub amount: String,
    /// Asset transféré. `None` ou `null` = PMS natif. Sinon l'asset_id du token.
    #[serde(default)]
    pub asset_id: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct EstimateFeeResponse {
    /// Fee de base PMS (FeePolicy natif). String décimal 8 décimales.
    pub fee: String,
    /// Fee additionnel issu des smart contracts `OnTransfer` (souvent 0).
    pub transfer_fee: String,
    /// Total à débiter du sender = `amount + fee + transfer_fee`.
    pub total: String,
    /// Liste des bénéficiaires des transfer fees (transparency pour la UI).
    pub fee_breakdown: Vec<FeeBreakdownItem>,
}

#[derive(Serialize, Debug)]
pub struct FeeBreakdownItem {
    pub contract_id: String,
    pub contract_name: String,
    pub beneficiary: String,
    pub amount: String,
}

pub async fn estimate_fee(
    State(state): State<AppState>,
    Json(req): Json<EstimateFeeRequest>,
) -> Result<Json<EstimateFeeResponse>, (StatusCode, Json<serde_json::Value>)> {
    let amount_dec = Decimal::from_str(&req.amount).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "code": 2020, "message": format!("invalid amount: {e}") })),
        )
    })?;

    if amount_dec < Decimal::ZERO {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "code": 2020, "message": "amount must be non-negative" })),
        ));
    }

    let (fee_policy, _ratio_dec) = tx_helpers::load_fee_policy(&state.store);
    let fee = fee_policy.compute_fee(&req.amount).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "code": 2020, "message": format!("fee computation failed: {e:?}") })),
        )
    })?;

    let transfer_results = evaluate_transfer(
        state.contract_store.as_ref(),
        &state.ledger_id,
        req.asset_id.as_deref(),
        amount_dec,
    );

    let transfer_fee_total: Decimal = transfer_results.iter().map(|r| r.fee_amount).sum();
    let fee_dec = fee.inner();
    let total = amount_dec + fee_dec + transfer_fee_total;

    let fee_breakdown = transfer_results
        .into_iter()
        .map(|r| FeeBreakdownItem {
            contract_id: r.contract_id,
            contract_name: r.contract_name,
            beneficiary: r.beneficiary_address,
            amount: r.fee_amount.to_string(),
        })
        .collect();

    Ok(Json(EstimateFeeResponse {
        fee: fee_dec.to_string(),
        transfer_fee: transfer_fee_total.to_string(),
        total: total.to_string(),
        fee_breakdown,
    }))
}
