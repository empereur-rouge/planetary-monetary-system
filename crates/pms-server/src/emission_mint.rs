//! Orchestrateur d'émission de PMS natif gatée (plan §3.1).
//!
//! Couche au-dessus du gate pur ([`crate::emission`]) : enchaîne
//! **réserve → forge → persist → (release si échec)** + publication des jauges,
//! pour les voies qui émettent un **`Mint` simple sur le ledger main**
//! (baseline taux cible, on-ramp fiat, faucet). Sans cet orchestrateur, chaque
//! voie ré-écrirait le « dance » reserve/release et oublierait tôt ou tard un
//! chemin de rollback → fuite ou double-comptage de budget (audit altitude).
//!
//! NOTE : la **voie B** (conversion token custom→PMS, déclenchée par un
//! `OnTokenBurn`) NE passe PAS par `emit_native_gated` : elle doit réserver le
//! budget AVANT de brûler (atomicité), donc elle enveloppe son propre forge avec
//! `EmissionGate::reserve`/`release` directement dans `api_fn::token_burn`. Un
//! refactor possible : extraire un `forge_reserved_mint` partagé (réutilisé par
//! `emit_native_gated` ET la voie B) pour dédupliquer le tail forge+métriques.
//!
//! Le gate (`reserve`/`release`) reste découplé de `AppState` ; cet
//! orchestrateur, lui, vit dans la couche serveur.

use crate::api::AppState;
use crate::api_fn::tx_helpers;
use crate::emission::{EmissionError, EmissionParams, Voie};
use pms_storage::PutResult;
use pms_types::TxOutput;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

/// Résultat d'une émission gatée.
#[derive(Debug)]
pub enum EmitOutcome {
    /// Un bloc de mint a été forgé et persisté.
    Minted {
        amount: Decimal,
        block_id: String,
        num_outputs: usize,
    },
    /// Rien à émettre (budget de la période déjà consommé, ou résidu nul, ou
    /// sorties vides après construction) — aucun bloc forgé.
    Nothing,
}

/// Construit les `EmissionParams` du couloir depuis la config de fees.
pub(crate) fn params_from_settings(settings: &pms_config::Settings) -> EmissionParams {
    EmissionParams {
        target_pct: settings.fees.annual_inflation_percent,
        ceiling_pct: settings.fees.annual_ceiling_percent,
        floor_pct: settings.fees.annual_floor_percent,
        epoch_duration_sec: settings.fees.emission_epoch_duration_sec,
    }
}

/// Rafraîchit les jauges du budget depuis l'état courant du gate (appelé après
/// chaque réservation pour que le dashboard reste à jour même hors mint).
/// `pms_emission_effective_rate` est le **canari** du couloir : une alerte
/// `effective_rate > plafond` signalerait un bug du `clamp`.
pub(crate) async fn publish_emission_gauges(state: &AppState, params: EmissionParams) {
    let snap = state.emission_gate.snapshot().await;
    let eff = crate::emission::effective_rate_pct(
        params.target_pct,
        params.ceiling_pct,
        params.floor_pct,
    );
    let lid = state.ledger_id.as_str();
    crate::metrics::EMISSION_BUDGET_TOTAL
        .with_label_values(&[lid])
        .set(snap.budget.to_f64().unwrap_or(0.0));
    crate::metrics::EMISSION_BUDGET_CONSUMED
        .with_label_values(&[lid])
        .set(snap.emitted.to_f64().unwrap_or(0.0));
    crate::metrics::EMISSION_BUDGET_REMAINING
        .with_label_values(&[lid])
        .set(snap.remaining().to_f64().unwrap_or(0.0));
    crate::metrics::EMISSION_EFFECTIVE_RATE
        .with_label_values(&[lid])
        .set(eff);
}

/// Émet du PMS natif sur le ledger main à travers le budget partagé.
///
/// - `voie` : la voie (label métrique + sémantique baseline/explicite).
/// - `requested` : `None` (ou `Voie::Baseline`) ⇒ résidu du budget ; `Some(x)`
///   ⇒ montant explicite, rejeté si `x > restant` (P1).
/// - `build_outputs` : construit les sorties du bloc `Mint` à partir du montant
///   **réellement autorisé** par le gate (la baseline le splitte
///   creator:treasury ; l'on-ramp en fait une sortie unique). Sorties vides ⇒
///   la réservation est relâchée et `Nothing` est renvoyé.
///
/// Garanties : la réservation (P1, sous mutex) précède le forge ; tout échec
/// après réservation (parents, forge, persist non-`Inserted`) **relâche** le
/// budget (pas de fuite). Le compteur `EMISSION_MINTED` n'est incrémenté qu'au
/// `Inserted`.
pub async fn emit_native_gated<F>(
    state: &AppState,
    voie: Voie,
    requested: Option<Decimal>,
    description: &str,
    build_outputs: F,
) -> Result<EmitOutcome, EmissionError>
where
    F: FnOnce(Decimal) -> Vec<TxOutput>,
{
    let params = params_from_settings(&state.settings);

    // 1. Réservation atomique (P1, ferme le TOCTOU) — supply lue pour le rollover.
    let (supply, _) = state.srv.adapter_arc().circulating_supply().await;
    let reservation = state
        .emission_gate
        .reserve(
            &state.store,
            pms_utils::ts_ms(),
            supply,
            params,
            voie,
            requested,
        )
        .await?;

    // 2. Jauges fraîches à chaque entrée (même si rien n'est minté).
    publish_emission_gauges(state, params).await;

    let amount = reservation.amount;
    if amount <= Decimal::ZERO {
        return Ok(EmitOutcome::Nothing);
    }

    // 3. Sorties du bloc à partir du montant autorisé.
    let outputs = build_outputs(amount);
    if outputs.is_empty() {
        state.emission_gate.release(&state.store, amount).await;
        return Ok(EmitOutcome::Nothing);
    }

    // 4. Forge + persist (réutilise les helpers partagés). Tout échec relâche.
    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => {
            state.emission_gate.release(&state.store, amount).await;
            return Err(EmissionError::Persist(anyhow::anyhow!(
                "parent resolution: {e}"
            )));
        }
    };

    let num_outputs = outputs.len();
    let payload = PayloadEnvelope::Plain(PlainPayload::Mint { outputs });
    let wb = match tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &state.srv.adapter_arc(),
        &state.node_wallet,
        &state.settings,
        Some(description),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            state.emission_gate.release(&state.store, amount).await;
            return Err(EmissionError::Persist(anyhow::anyhow!("forge: {e}")));
        }
    };

    match tx_helpers::persist_and_broadcast(state, &wb).await {
        Ok(PutResult::Inserted) => {
            // NOTE: pas d'`add_utxo` ici — `PlainPayload::Mint` est un payload
            // plain, `persist_block` construit et applique déjà le UtxoDelta.
            crate::metrics::EMISSION_MINTED
                .with_label_values(&[state.ledger_id.as_str(), voie.as_str()])
                .inc_by(amount.to_f64().unwrap_or(0.0));
            Ok(EmitOutcome::Minted {
                amount,
                block_id: wb.id,
                num_outputs,
            })
        }
        other => {
            // AlreadyExists / Rejected / Err : rien de neuf émis → relâche.
            state.emission_gate.release(&state.store, amount).await;
            Err(EmissionError::Persist(anyhow::anyhow!(
                "persist not inserted: {other:?}"
            )))
        }
    }
}
