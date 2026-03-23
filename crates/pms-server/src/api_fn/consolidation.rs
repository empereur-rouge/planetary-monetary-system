//! Admin endpoint for UTXO consolidation.
//!
//! Consolidates many small UTXOs for the coordinator into a single large UTXO
//! via a self-transfer. Combats coordinator UTXO proliferation from fee
//! distribution (each distribution cycle creates new UTXOs).
//!
//! Protected by `require_local_or_admin` middleware (same as all `/admin/*` routes).

use crate::api::AppState;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::StatusCode;
use pms_storage::PutResult;
use pms_types::{Transaction, TxInput, TxOutput, Unlock};
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn default_max_inputs() -> usize {
    64
}

/// Request body for `POST /admin/consolidate-utxos`.
#[derive(Debug, Deserialize)]
pub struct ConsolidateRequest {
    /// Asset ID to consolidate. `null` = PMS native.
    #[serde(default)]
    pub asset_id: Option<String>,
    /// Maximum number of UTXO inputs per consolidation (default: 64, max: 256).
    #[serde(default = "default_max_inputs")]
    pub max_inputs: usize,
}

/// Response for `POST /admin/consolidate-utxos`.
#[derive(Debug, Serialize)]
pub struct ConsolidateResponse {
    pub block_id: String,
    pub consolidated_inputs: usize,
    pub new_utxo_amount: String,
    pub fee: String,
}

/// POST /admin/consolidate-utxos
///
/// Consolidates multiple UTXOs for the coordinator address into a single output.
/// Creates a self-transfer signed by the coordinator node wallet.
///
/// Only the coordinator's own address can be consolidated (enforced in handler).
/// Protected by `require_local_or_admin` middleware layer.
pub async fn admin_consolidate_utxos(
    State(state): State<AppState>,
    Json(req): Json<ConsolidateRequest>,
) -> impl IntoResponse {
    // ════════════════════════════════════════════════════════════════════
    // 1) Validate max_inputs
    // ════════════════════════════════════════════════════════════════════
    let max_inputs = req.max_inputs.clamp(2, 256);

    // ════════════════════════════════════════════════════════════════════
    // 2) Get coordinator address (only coordinator can consolidate)
    // ════════════════════════════════════════════════════════════════════
    let hrp = &state.settings.address.hrp;
    let address = state.node_wallet.get_address(hrp);

    // ════════════════════════════════════════════════════════════════════
    // 3) Select UTXOs — grab as many as possible up to max_inputs
    // ════════════════════════════════════════════════════════════════════
    let adapter = state.srv.adapter_arc();

    let (mut selected, selected_sum) = match tx_helpers::select_utxos(
        &adapter,
        &address,
        Decimal::MAX,
        &req.asset_id,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": format!("coin selection failed: {e}") })),
            );
        }
    };

    // Cap to max_inputs
    selected.truncate(max_inputs);

    if selected.len() <= 1 {
        return (
            StatusCode::OK,
            Json(json!({
                "status": "noop",
                "message": "nothing to consolidate (0 or 1 UTXOs found)",
                "utxo_count": selected.len()
            })),
        );
    }

    // Recalculate actual sum after truncation
    let actual_sum: Decimal = selected.iter().map(|(_, _, amt)| amt).sum();

    // ════════════════════════════════════════════════════════════════════
    // 4) Compute fee
    // ════════════════════════════════════════════════════════════════════
    let (fee_policy, _ratio) = tx_helpers::load_fee_policy(&state.store);
    let fee_dec = fee_policy
        .compute_fee(&actual_sum.to_string())
        .map(|a| a.inner())
        .unwrap_or(Decimal::ZERO);

    if actual_sum <= fee_dec {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "total UTXO value is less than or equal to the fee",
                "total": actual_sum.to_string(),
                "fee": fee_dec.to_string()
            })),
        );
    }

    let consolidation_amount = actual_sum - fee_dec;

    // ════════════════════════════════════════════════════════════════════
    // 5) Build inputs
    // ════════════════════════════════════════════════════════════════════
    let tx_inputs: Vec<TxInput> = selected
        .iter()
        .map(|(output_id, _, _)| TxInput {
            out: output_id.clone(),
        })
        .collect();

    let input_count = tx_inputs.len();

    // ════════════════════════════════════════════════════════════════════
    // 6) Build outputs — self-transfer + fee
    // ════════════════════════════════════════════════════════════════════
    let mut tx_outputs: Vec<TxOutput> = Vec::new();

    // Consolidated output back to coordinator
    tx_outputs.push(TxOutput {
        address: address.clone(),
        amount: consolidation_amount.to_string(),
        asset_id: req.asset_id.clone(),
    });

    // Fee output to treasury/admin
    if fee_dec > Decimal::ZERO {
        let admin_addr = state
            .settings
            .admin
            .wallet_addresses
            .first()
            .cloned()
            .or_else(|| state.settings.fees.treasury_addresses.first().cloned());

        let admin_addr = match admin_addr {
            Some(addr) => addr,
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "no admin wallet configured for fees" })),
                );
            }
        };

        tx_outputs.push(TxOutput {
            address: admin_addr,
            amount: fee_dec.to_string(),
            asset_id: req.asset_id.clone(), // Must match input asset for conservation
        });
    }

    // ════════════════════════════════════════════════════════════════════
    // 7) Build transaction + sign with coordinator wallet
    // ════════════════════════════════════════════════════════════════════
    let unsigned_tx = Transaction {
        inputs: tx_inputs,
        outputs: tx_outputs,
        fee: fee_dec.to_string(),
        unlocks: vec![],
    };

    let tx_hash = match unsigned_tx.signing_message() {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("tx hash failed: {e}") })),
            );
        }
    };

    let signature_b64 = match state.node_wallet.sign(&tx_hash) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("signing failed: {e:?}") })),
            );
        }
    };

    let signed_tx = Transaction {
        unlocks: vec![Unlock {
            pubkey_hex: state.node_wallet.public_key_hex.clone(),
            signature_b64,
        }],
        ..unsigned_tx
    };

    // ════════════════════════════════════════════════════════════════════
    // 8) Build plain payload (no encryption needed — coordinator self-transfer)
    // ════════════════════════════════════════════════════════════════════
    let plain = PlainPayload::TxUtxo(signed_tx.clone());
    let payload = Some(PayloadEnvelope::Plain(plain.clone()));

    // ════════════════════════════════════════════════════════════════════
    // 9) Get parents + forge block + PoW + sign
    // ════════════════════════════════════════════════════════════════════
    let settings = &*state.settings;

    let parents = match tx_helpers::get_block_parents(&state.store, settings).await {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e })));
        }
    };

    let wb = match tx_helpers::forge_and_sign_block(
        payload,
        parents,
        &adapter,
        &state.node_wallet,
        settings,
        None,
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("block forge failed: {e}") })),
            );
        }
    };

    // ════════════════════════════════════════════════════════════════════
    // 10) Persist + broadcast + fee accumulation
    // ════════════════════════════════════════════════════════════════════
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => {
            // Plain payload — UTXO delta already applied in persist_block.
            // Do NOT call apply_utxo_delta for plain payloads (v0.6.4 critical pattern).

            // Accumulate fee in pool for periodic consolidated distribution
            tx_helpers::accumulate_tx_fee(&state, fee_dec).await;

            tracing::info!(
                ledger = %state.ledger_id,
                block_id = %wb.id,
                inputs = input_count,
                amount = %consolidation_amount,
                fee = %fee_dec,
                "UTXO consolidation complete"
            );

            (
                StatusCode::CREATED,
                Json(json!(ConsolidateResponse {
                    block_id: wb.id,
                    consolidated_inputs: input_count,
                    new_utxo_amount: consolidation_amount.to_string(),
                    fee: fee_dec.to_string(),
                })),
            )
        }
        Ok(PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "block already exists" })),
        ),
        Ok(PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("rejected: {reason}") })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("persist failed: {e}") })),
        ),
    }
}
