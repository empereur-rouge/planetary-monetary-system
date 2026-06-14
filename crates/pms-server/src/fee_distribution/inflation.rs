use crate::api::AppState;
use crate::emission::Voie;
use crate::emission_mint::{EmitOutcome, emit_native_gated};
use anyhow::Result;
use pms_types::TxOutput;
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;

use super::distribute::DistributeFeesResult;

/// Résultat « rien distribué » (mint sauté ou échoué). Factorise les retours
/// vides (non-coordinateur, budget consommé, échec de forge/persist).
fn empty_result(success: bool) -> DistributeFeesResult {
    DistributeFeesResult {
        success,
        reward_block_id: None,
        total_distributed: "0".to_string(),
        num_recipients: 0,
    }
}

/// Baseline mint d'émission : minte le **résidu** du budget de la période
/// (`budget − déjà-émis-par-les-voies`) via l'orchestrateur du budget partagé
/// (plan §3.1). Le budget est `supply × clamp(taux_cible, plancher, plafond) ×
/// frac_année` — le `clamp` au plafond est le couloir inviolable (plan §2.1).
///
/// Le montant autorisé par le gate est réparti `creator:treasury` (renormalisé
/// sur 100 %, **sans burn** : la cible EST le taux de croissance net ; le burn
/// déflationniste vit dans le chemin des fees, `burn_rate_bps`). Réserver ==
/// minter garde le compteur de budget exact.
pub async fn perform_daily_inflation_mint(state: &AppState) -> Result<DistributeFeesResult> {
    let settings = &state.settings;
    let node_wallet = &state.node_wallet;

    // Seul le Coordinator minte.
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true
    };
    if !is_coordinator {
        return Ok(empty_result(false));
    }

    // Destinataires du split (résolus en amont, capturés par la closure).
    let coordinator_address = node_wallet.get_address("8e");
    let treasury_addr = state
        .treasury_wallets
        .list
        .first()
        .cloned()
        .or_else(|| settings.fees.treasury_addresses.first().cloned())
        .unwrap_or_else(|| coordinator_address.clone());
    let creator_pct = Decimal::from(settings.fees.creator_reward_percent);
    let treasury_pct = Decimal::from(settings.fees.treasury_reward_percent);

    let description = format!(
        "Daily inflation (residual): target {}%/yr, ceiling {}%/yr",
        settings.fees.annual_inflation_percent, settings.fees.annual_ceiling_percent
    );

    // Voie baseline : montant = résidu (None). L'orchestrateur réserve (P1, ferme
    // le TOCTOU), publie les jauges, forge/persiste, et relâche en cas d'échec.
    let outcome = emit_native_gated(state, Voie::Baseline, None, &description, |amount| {
        let denom = creator_pct + treasury_pct;
        let coord = if denom > Decimal::ZERO {
            (amount * creator_pct / denom).round_dp(8)
        } else {
            amount
        };
        let treasury = amount - coord; // reste exact, pas de poussière
        let mut outs: Vec<TxOutput> = Vec::new();
        if coord > Decimal::ZERO {
            outs.push(TxOutput::new(
                coordinator_address.clone(),
                coord.normalize().to_string(),
                None,
            ));
        }
        if treasury > Decimal::ZERO {
            outs.push(TxOutput::new(
                treasury_addr.clone(),
                treasury.normalize().to_string(),
                None,
            ));
        }
        outs
    })
    .await;

    match outcome {
        Ok(EmitOutcome::Minted {
            amount,
            block_id,
            num_outputs,
        }) => {
            tracing::info!(
                "📊 Daily inflation minted (residual): {} PMS to {} recipients (block: {})",
                amount,
                num_outputs,
                &block_id[..block_id.len().min(16)]
            );
            Ok(DistributeFeesResult {
                success: true,
                reward_block_id: Some(block_id),
                total_distributed: amount.to_string(),
                num_recipients: num_outputs,
            })
        }
        Ok(EmitOutcome::Nothing) => {
            tracing::info!("📊 Inflation mint skipped: budget already consumed this epoch");
            Ok(empty_result(true))
        }
        Err(e) => {
            tracing::warn!("📊 Inflation mint failed: {}", e);
            Ok(empty_result(false))
        }
    }
}
