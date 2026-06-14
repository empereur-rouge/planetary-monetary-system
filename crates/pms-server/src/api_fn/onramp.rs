//! On-ramp fiat→PMS (voie A du plan §3.2) — mint de PMS natif à travers le
//! budget d'émission partagé (plan §3.1).
//!
//! L'opérateur encaisse le fiat **hors-DAG** (plan §6 : on-ramp fiat→PMS hors
//! DAG ; §9 : AML/KYC côté rampe), puis appelle cet endpoint avec une **preuve
//! de paiement attestée** (`payment_ref` : id de charge Stripe, référence
//! bancaire…) que l'on inscrit dans les métadonnées du bloc pour la traçabilité.
//! Le mint passe par le gate : il est **plafonné par le budget de la période**
//! (P1) — si le budget est épuisé, l'émission est refusée (code `5030`), jamais
//! clampée en silence. C'est la 2e voie qui valide que le budget est bien
//! *partagé* (baseline + on-ramp puisent dans le même couloir).
//!
//! Route : `POST /admin/onramp` (admin, `admin_writable` → gated read-only).

use crate::api::AppState;
use crate::api_error::ApiError;
use crate::emission::{EmissionError, Voie};
use crate::emission_mint::{EmitOutcome, emit_native_gated};
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use pms_types::TxOutput;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;

/// Requête d'on-ramp : créditer `to` de `amount` PMS contre une preuve de
/// paiement fiat encaissé hors-DAG.
#[derive(Debug, Deserialize)]
pub struct OnRampRequest {
    /// Adresse destinataire (bech32).
    pub to: String,
    /// Montant de PMS à émettre (string décimale).
    pub amount: String,
    /// Référence du paiement fiat attestée par l'opérateur (encaissé hors-DAG).
    /// Inscrite dans les métadonnées du bloc pour l'audit. La vérification fiat
    /// réelle (AML/KYC, anti-fraude) se fait hors-DAG (plan §6, §9).
    pub payment_ref: String,
}

/// `POST /admin/onramp` — émet du PMS natif via la voie A, sous budget partagé.
pub async fn admin_onramp(
    State(state): State<AppState>,
    Json(req): Json<OnRampRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // Validation requête (2xxx — spécifique OK, pas d'info leak).
    let amount = match Decimal::from_str_exact(&req.amount) {
        Ok(d) if d > Decimal::ZERO => d,
        _ => {
            return Err(ApiError::InvalidAmount {
                reason: "amount must be a positive decimal".into(),
            });
        }
    };
    if req.to.trim().is_empty() {
        return Err(ApiError::InvalidAddress {
            addr: req.to.clone(),
        });
    }
    if req.payment_ref.trim().is_empty() {
        return Err(ApiError::InvalidField {
            field: "payment_ref",
            reason: "required (off-DAG fiat payment attestation)".into(),
        });
    }

    let to = req.to.clone();
    let description = format!("On-ramp mint (voie A) — payment_ref={}", req.payment_ref);

    // Voie A : montant EXPLICITE → réservé tel quel, refusé si > budget restant.
    let outcome = emit_native_gated(
        &state,
        Voie::OnRamp,
        Some(amount),
        &description,
        |amt| vec![TxOutput::new(to.clone(), amt.normalize().to_string(), None)],
    )
    .await;

    match outcome {
        Ok(EmitOutcome::Minted {
            amount, block_id, ..
        }) => Ok(Json(json!({
            "status": "ok",
            "block_id": block_id,
            "minted": amount.to_string(),
            "to": req.to,
            "payment_ref": req.payment_ref,
        }))),
        // Montant explicite > 0 dans le budget ⇒ toujours Minted ; Nothing est
        // un cas défensif (ne devrait pas arriver pour une voie à montant).
        Ok(EmitOutcome::Nothing) => Err(ApiError::Internal {
            reason: "on-ramp produced no mint (unexpected for explicit amount)".into(),
        }),
        Err(EmissionError::BudgetExhausted {
            voie,
            requested,
            remaining,
            budget,
        }) => Err(ApiError::EmissionBudgetExhausted {
            reason: format!(
                "voie={} requested={} remaining={} budget={}",
                voie.as_str(),
                requested,
                remaining,
                budget
            ),
        }),
        Err(EmissionError::Persist(e)) => Err(ApiError::StorageError {
            reason: e.to_string(),
        }),
        Err(EmissionError::MintDisabled) => Err(ApiError::MintDisabled),
    }
}
