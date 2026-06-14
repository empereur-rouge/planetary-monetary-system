//! Burn de token (plan §3.1, voie B) — primitive générique de destruction.
//!
//! L'utilisateur brûle `amount` de `asset_id` : ses UTXOs sont dépensés, le
//! montant est DÉTRUIT (la supply baisse), le change lui revient. Le payload est
//! **PLAIN** (transparent = proof-of-burn ; le hot path valide la
//! conservation-burn + l'ownership via `validate_token_burn_async`). Owner-signé
//! (signatures sur les inputs) ; le bloc est forgé par le Coordinator
//! (single-writer). Déclenche les contrats `OnTokenBurn{asset_id}` — le câblage
//! event→listener→mint (voie B complète) arrive en phase suivante.
//!
//! Route : `POST /v1/wallet/token/burn` (auth_write → gated read-only).

use crate::api::AppState;
use crate::api_error::ApiError;
use crate::api_fn::tx_helpers;
use crate::api_fn::wallet_factory::wallet_from_b64;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use pms_storage::PutResult;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput};
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;

/// Requête de burn : détruire `amount` de `asset_id` détenu par le owner.
#[derive(Debug, Deserialize)]
pub struct BurnTokenRequest {
    /// Clé privée (base64) du burner.
    pub private_key_b64: String,
    /// Asset à brûler (absent / `null` = PMS natif).
    #[serde(default)]
    pub asset_id: Option<String>,
    /// Montant à détruire (string décimale, > 0).
    pub amount: String,
}

/// `POST /v1/wallet/token/burn` — brûle un token (la supply baisse).
pub async fn wallet_burn_token(
    State(state): State<AppState>,
    Json(req): Json<BurnTokenRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // 1. Wallet du burner.
    let wallet = wallet_from_b64(&req.private_key_b64).map_err(|e| ApiError::InvalidField {
        field: "private_key_b64",
        reason: e,
    })?;
    let hrp = &state.settings.address.hrp;
    let owner = wallet.get_address(hrp);

    // 2. Montant.
    let amount_dec = match Decimal::from_str_exact(&req.amount) {
        Ok(d) if d > Decimal::ZERO => d,
        _ => {
            return Err(ApiError::InvalidAmount {
                reason: "amount must be a positive decimal".into(),
            });
        }
    };

    // 3. Coin selection sur l'asset à brûler.
    let adapter = state.srv.adapter_arc();
    let (selected, selected_sum) =
        tx_helpers::select_utxos(&adapter, &owner, amount_dec, &req.asset_id)
            .await
            .map_err(|_| ApiError::InsufficientBalance {
                addr: owner.clone(),
                asset_id: req.asset_id.clone(),
                requested: amount_dec.to_string(),
                available: "0".into(),
            })?;

    // 4. Inputs + change (le reste revient au burner — même asset).
    let inputs: Vec<TxInput> = selected
        .iter()
        .map(|(oid, _, _)| TxInput { out: oid.clone() })
        .collect();
    let change = selected_sum - amount_dec;
    let mut outputs: Vec<TxOutput> = Vec::new();
    if change > Decimal::ZERO {
        outputs.push(TxOutput::new(
            owner.clone(),
            change.to_string(),
            req.asset_id.clone(),
        ));
    }

    // 5. Transaction + signature (le owner signe les inputs ; fee 0 — un burn ne
    //    paie pas de frais, il détruit).
    let unsigned = Transaction {
        inputs,
        outputs,
        fee: "0".into(),
        unlocks: vec![],
    };
    let msg = unsigned
        .signing_message(&state.settings.network.network_id)
        .map_err(|e| ApiError::Internal {
            reason: format!("tx signing message: {e}"),
        })?;
    let sig = wallet.sign(&msg).map_err(|e| ApiError::Internal {
        reason: format!("sign: {e:?}"),
    })?;
    let signed = Transaction {
        unlocks: tx_helpers::replicate_unlocks(&wallet.public_key_hex, &sig, unsigned.inputs.len()),
        ..unsigned
    };

    // 6. Payload PLAIN TokenBurn (transparent — proof-of-burn ; conservation-burn
    //    + ownership validés par le hot path).
    let payload = PayloadEnvelope::Plain(PlainPayload::TokenBurn {
        tx: signed,
        asset_id: req.asset_id.clone(),
        amount: amount_dec.to_string(),
        owner: owner.clone(),
    });

    // 7. Forge (Coordinator) + persist.
    let parents = tx_helpers::get_block_parents(&state.store, &state.settings)
        .await
        .map_err(|e| ApiError::Internal {
            reason: format!("parents: {e}"),
        })?;
    let wb = tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("TokenBurn"),
    )
    .await
    .map_err(|e| ApiError::Internal {
        reason: format!("forge: {e}"),
    })?;

    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => {
            // Émet TokenBurnProcessed sur le bus contrat (main) pour déclencher
            // les contrats OnTokenBurn (voie B). Même bus que les burns NFT — le
            // ContractListener n'écoute que le bus main, y compris pour les
            // burns sur custom ledgers.
            if let Some(bus) = &state.contract_event_bus {
                bus.emit(pms_event::PmsEvent::token_burn_processed(
                    wb.id.clone(),
                    state.ledger_id.clone(),
                    owner.clone(),
                    req.asset_id.clone(),
                    amount_dec.to_string(),
                ));
            }
            Ok(Json(json!({
                "status": "ok",
                "block_id": wb.id,
                "burned": amount_dec.to_string(),
                "asset_id": req.asset_id,
                "owner": owner,
            })))
        }
        Ok(PutResult::AlreadyExists) => Err(ApiError::AlreadyExists {
            kind: "block",
            id: wb.id,
        }),
        Ok(PutResult::Rejected(r)) => Err(ApiError::Conflict(format!("token burn rejected: {r}"))),
        Err(e) => Err(ApiError::StorageError { reason: e }),
    }
}
