//! Burn de token + conversion voie B (plan §3.1) — destruction + mint PMS.
//!
//! L'utilisateur brûle `amount` de `asset_id` : ses UTXOs sont dépensés, le
//! montant est DÉTRUIT (la supply baisse), le change lui revient. Le payload est
//! **PLAIN** (transparent = proof-of-burn ; le hot path valide la
//! conservation-burn + l'ownership via `validate_token_burn_async`). Owner-signé
//! (signatures sur les inputs) ; le bloc est forgé par le Coordinator
//! (single-writer).
//!
//! ## Conversion voie B (SYNCHRONE, dans ce handler)
//!
//! Si un contrat `OnTokenBurn{asset_id}` existe, ce handler évalue le contrat
//! (`evaluate_token_burn` → taux R), **réserve le PMS sur le budget d'émission
//! AVANT de brûler** (atomicité / sûreté des fonds : budget épuisé ⇒ rejet
//! complet, aucun burn), brûle, puis minte le PMS au burner. Le mint réel passe
//! par l'`EmissionGate` (le moteur garde la primitive native ; le contrat porte
//! la politique). Choix SYNCHRONE (pas un listener async) pour l'atomicité +
//! le gating read-only par la route + le retour immédiat `{converted_pms}`.
//!
//! L'event `TokenBurnProcessed` reste émis pour **l'observabilité** (webhooks,
//! activité, consommateurs futurs) — **aucun listener ne re-déclenche la
//! conversion** (le `ContractListener` n'écoute que `NftBurnProcessed`), donc
//! pas de double-mint.
//!
//! Route : `POST /v1/wallet/token/burn` (auth_write → gated read-only).

use crate::api::AppState;
use crate::api_error::ApiError;
use crate::api_fn::tx_helpers;
use crate::api_fn::wallet_factory::wallet_from_b64;
use crate::emission::{EmissionError, Voie};
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use pms_storage::PutResult;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput};
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
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

    // 6. CONVERSION (voie B) — RÉSERVE le budget AVANT de brûler.
    //
    // Si un contrat `OnTokenBurn{asset}` existe (le moteur de contrats détient la
    // politique : taux R), on calcule le PMS à minter et on le réserve sur le
    // budget d'émission AVANT le moindre bloc. Atomicité / sûreté des fonds : un
    // budget épuisé rejette TOUTE l'opération (aucun burn → aucune perte). Un burn
    // de PMS natif (asset None) ne se convertit pas. Le mint réel passe par le
    // budget partagé (EmissionGate) — le moteur garde la primitive native.
    let conversions: Vec<pms_contracts::MintNativeResult> = match &req.asset_id {
        Some(asset) => pms_contracts::evaluate_token_burn(
            state.contract_store.as_ref(),
            &state.ledger_id,
            &owner,
            asset,
            amount_dec,
        ),
        None => Vec::new(),
    };
    let pms_to_mint: Decimal = conversions.iter().map(|r| r.amount).sum();

    let mut reserved = Decimal::ZERO;
    if pms_to_mint > Decimal::ZERO {
        let params = crate::emission_mint::params_from_settings(&state.settings);
        let (supply, _) = adapter.circulating_supply().await;
        match state
            .emission_gate
            .reserve(
                &state.store,
                pms_utils::ts_ms(),
                supply,
                params,
                Voie::TokenConversion,
                Some(pms_to_mint),
            )
            .await
        {
            Ok(r) => reserved = r.amount,
            Err(EmissionError::BudgetExhausted {
                remaining, budget, ..
            }) => {
                // Aucun burn n'a eu lieu — rien à relâcher. L'utilisateur garde son token.
                return Err(ApiError::EmissionBudgetExhausted {
                    reason: format!(
                        "voie=token_conversion asset={:?} burn={} pms_requested={} remaining={} budget={}",
                        req.asset_id, amount_dec, pms_to_mint, remaining, budget
                    ),
                });
            }
            Err(EmissionError::Persist(e)) => {
                return Err(ApiError::StorageError {
                    reason: e.to_string(),
                });
            }
            // Kill-switch armé : aucun burn n'a eu lieu (réserve-avant-burn) —
            // l'utilisateur garde son token, rien à relâcher.
            Err(EmissionError::MintDisabled) => {
                return Err(ApiError::MintDisabled);
            }
        }
    }

    // (`EmissionGate::release` est un no-op si le montant est 0 — on peut
    // l'appeler inconditionnellement sur les chemins d'échec ci-dessous.)

    // 7. Payload PLAIN TokenBurn + forge + persist (le burn lui-même).
    let payload = PayloadEnvelope::Plain(PlainPayload::TokenBurn {
        tx: signed,
        asset_id: req.asset_id.clone(),
        amount: amount_dec.to_string(),
        owner: owner.clone(),
    });
    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => {
            state.emission_gate.release(&state.store, reserved).await;
            return Err(ApiError::Internal {
                reason: format!("parents: {e}"),
            });
        }
    };
    let wb = match tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("TokenBurn"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            state.emission_gate.release(&state.store, reserved).await;
            return Err(ApiError::Internal {
                reason: format!("forge: {e}"),
            });
        }
    };
    let burn_block_id = match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => wb.id.clone(),
        Ok(PutResult::AlreadyExists) => {
            state.emission_gate.release(&state.store, reserved).await;
            return Err(ApiError::AlreadyExists {
                kind: "block",
                id: wb.id,
            });
        }
        Ok(PutResult::Rejected(r)) => {
            state.emission_gate.release(&state.store, reserved).await;
            return Err(ApiError::Conflict(format!("token burn rejected: {r}")));
        }
        Err(e) => {
            state.emission_gate.release(&state.store, reserved).await;
            return Err(ApiError::StorageError { reason: e });
        }
    };

    // 8. Burn persisté → event (observabilité) + mint de la conversion (si réservé).
    if let Some(bus) = &state.contract_event_bus {
        bus.emit(pms_event::PmsEvent::token_burn_processed(
            burn_block_id.clone(),
            state.ledger_id.clone(),
            owner.clone(),
            req.asset_id.clone(),
            amount_dec.to_string(),
        ));
    }

    let mut mint_block_id: Option<String> = None;
    if reserved > Decimal::ZERO {
        // Budget DÉJÀ réservé → on forge le Mint directement (pas via
        // emit_native_gated qui re-réserverait). Les MintNativeResult ciblent le
        // burner.
        let mint_payload = PayloadEnvelope::Plain(PlainPayload::Mint {
            outputs: vec![TxOutput::new(
                owner.clone(),
                reserved.normalize().to_string(),
                None,
            )],
        });
        let desc = format!(
            "TokenConversion mint (voie B): {} {} → {} PMS",
            amount_dec,
            req.asset_id.as_deref().unwrap_or("?"),
            reserved
        );
        let mint_res: Result<String, String> = async {
            let parents = tx_helpers::get_block_parents(&state.store, &state.settings)
                .await
                .map_err(|e| format!("parents: {e}"))?;
            let mwb = tx_helpers::forge_and_sign_block(
                Some(mint_payload),
                parents,
                &adapter,
                &state.node_wallet,
                &state.settings,
                Some(&desc),
            )
            .await
            .map_err(|e| format!("forge: {e}"))?;
            match tx_helpers::persist_and_broadcast(&state, &mwb).await {
                Ok(PutResult::Inserted) => Ok(mwb.id),
                other => Err(format!("persist not inserted: {other:?}")),
            }
        }
        .await;

        match mint_res {
            Ok(id) => {
                crate::metrics::EMISSION_MINTED
                    .with_label_values(&[state.ledger_id.as_str(), Voie::TokenConversion.as_str()])
                    .inc_by(reserved.to_f64().unwrap_or(0.0));
                crate::emission_mint::publish_emission_gauges(
                    &state,
                    crate::emission_mint::params_from_settings(&state.settings),
                )
                .await;
                mint_block_id = Some(id);
            }
            Err(e) => {
                // Burn persisté mais mint échoué → ORPHELIN (rare). On relâche le
                // budget et on LOG CRITIQUE : l'utilisateur a brûlé sans recevoir
                // son PMS, réconciliation opérateur requise.
                state.emission_gate.release(&state.store, reserved).await;
                crate::metrics::EMISSION_CONVERSION_ORPHANED.inc();
                tracing::error!(
                    target: "emission",
                    "CRITICAL token-conversion orphan: burn {} persisted ({} {}), PMS mint FAILED ({}). Burner {} owed {} PMS — manual reconciliation required.",
                    burn_block_id,
                    amount_dec,
                    req.asset_id.as_deref().unwrap_or("?"),
                    e,
                    owner,
                    reserved
                );
                return Err(ApiError::Internal {
                    reason: format!("token burned but PMS conversion mint failed: {e}"),
                });
            }
        }
    }

    Ok(Json(json!({
        "status": "ok",
        "block_id": burn_block_id,
        "burned": amount_dec.to_string(),
        "asset_id": req.asset_id,
        "owner": owner,
        "converted_pms": reserved.to_string(),
        "mint_block_id": mint_block_id,
    })))
}
