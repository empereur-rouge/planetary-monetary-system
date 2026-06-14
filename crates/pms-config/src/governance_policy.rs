//! Politique de gouvernance — palier minimum par paramètre + asymétrie
//! *tighten-now / loosen-later* (`pms-spec-governance-timelock.md` §2-§3).
//!
//! Deux règles structurent quel timelock s'applique à une proposition :
//!
//! 1. **Palier minimum par paramètre** ([`min_tier`]) — chaque `ConfigUpdate`
//!    exige un palier d'impact MINIMUM. On ne peut pas faire passer un changement
//!    du couloir d'émission (Constitution, 45 j) en « Operator 7 j ». Le palier
//!    est *déclaré* par le proposant mais *validé* `>= min_tier` au protocole.
//!
//! 2. **Asymétrie tighten/loosen** ([`direction`], [`required_timelock_ms`]) —
//!    **resserrer** (réduire un risque pour les détenteurs : couper le mint,
//!    baisser un plafond) peut être **instantané** ; **desserrer** (augmenter un
//!    risque : reprendre le mint, hausser un plafond) exige le timelock **plein**
//!    du palier. On n'attend pas 45 j pour stopper une fuite, mais on attend 45 j
//!    pour s'autoriser à émettre plus.
//!
//! La direction est calculée par le PROTOCOLE (persist) à partir de la config
//! courante, pas déclarée par le proposant — un proposant ne peut donc pas
//! réclamer un timelock instantané pour un desserrage. La validation re-dérive
//! l'`enact_after` attendu et rejette toute incohérence.

use crate::governance::GovernanceTier;
use crate::runtime::{ConfigUpdate, RuntimeConfig};

/// Sens d'un changement vis-à-vis du **risque pour les détenteurs**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Resserre (réduit un risque) — éligible au timelock instantané (urgence).
    Tighten,
    /// Desserre (augmente un risque) — exige le timelock plein du palier.
    Loosen,
}

/// Palier d'impact MINIMUM requis pour modifier ce paramètre (table §2).
///
/// Défaut sûr : tout ce qui touche le pouvoir de mint = au moins `Policy` ;
/// la distribution / le burn / les voies = `Policy` ; le calibrage de fees =
/// `Operator`. (Le couloir d'émission `SetEmissionCorridor` = `Constitution`
/// sera ajouté en P3 avec sa variante.)
pub fn min_tier(update: &ConfigUpdate) -> GovernanceTier {
    match update {
        // ── Politique : pouvoir de mint, distribution, burn, voies ──
        ConfigUpdate::SetMintEnabled { .. } => GovernanceTier::Policy,
        ConfigUpdate::SetMaxMint { .. } => GovernanceTier::Policy,
        ConfigUpdate::SetCoordinatorFee { .. } => GovernanceTier::Policy,
        ConfigUpdate::SetTreasuryFee { .. } => GovernanceTier::Policy,
        ConfigUpdate::SetFeeDistribution { .. } => GovernanceTier::Policy,
        ConfigUpdate::SetBurnRate { .. } => GovernanceTier::Policy,
        ConfigUpdate::SetMintFee { .. } => GovernanceTier::Policy,

        // ── Operator : calibrage opérationnel (fees, anti-spam) ──
        ConfigUpdate::SetFeeRate { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetBaseFee { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetMinPow { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetFeeTiers { .. } => GovernanceTier::Operator,
        ConfigUpdate::ClearFeeTiers => GovernanceTier::Operator,
        ConfigUpdate::SetTokenCreationFee { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetNftMintFee { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetNftFeeExemptTypes { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetContractDeploymentFee { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetStorageFeePerKb { .. } => GovernanceTier::Operator,
        ConfigUpdate::SetDynamicFee { .. } => GovernanceTier::Operator,

        // ── Batch : le palier le plus élevé de ses composants ──
        ConfigUpdate::BatchUpdate(updates) => updates
            .iter()
            .map(min_tier)
            .max()
            .unwrap_or(GovernanceTier::Operator),
    }
}

/// Sens du changement relativement à la config COURANTE (risque détenteurs).
///
/// Seuls les paramètres liés à l'émission ont un *tighten* éligible à l'instant
/// (couper/réduire le mint). Tout le reste est traité comme `Loosen` (timelock
/// plein) — défaut conservateur : un paramètre dont le sens n'est pas clairement
/// « réducteur de risque » ne mérite pas le passe-droit d'urgence.
pub fn direction(update: &ConfigUpdate, current: &RuntimeConfig) -> Direction {
    match update {
        // Couper le mint resserre ; le reprendre desserre.
        ConfigUpdate::SetMintEnabled { enabled } => {
            if *enabled {
                Direction::Loosen
            } else {
                Direction::Tighten
            }
        }
        // Baisser le plafond par bloc resserre ; le hausser desserre.
        ConfigUpdate::SetMaxMint { amount } => {
            if *amount < current.max_mint_per_block {
                Direction::Tighten
            } else {
                Direction::Loosen
            }
        }
        // Un batch est tighten SEULEMENT si TOUS ses composants le sont (sinon un
        // desserrage caché passerait en instantané).
        ConfigUpdate::BatchUpdate(updates) => {
            if !updates.is_empty()
                && updates
                    .iter()
                    .all(|u| direction(u, current) == Direction::Tighten)
            {
                Direction::Tighten
            } else {
                Direction::Loosen
            }
        }
        // Tout le reste : pas de passe-droit d'urgence.
        _ => Direction::Loosen,
    }
}

/// Timelock requis (ms) pour ce changement, vu la config courante + le palier
/// déclaré.
///
/// - `Tighten` ⇒ **0** (instantané) : `enact_after == announced_at`.
/// - `Loosen`  ⇒ durée pleine du palier déclaré (`tier.default_duration_ms()`).
///
/// C'est la fonction PARTAGÉE par le handler `propose` (qui en dérive
/// `enact_after`) et la validation persist (qui re-vérifie l'égalité) — un seul
/// point de vérité pour éviter toute divergence.
pub fn required_timelock_ms(
    update: &ConfigUpdate,
    current: &RuntimeConfig,
    declared_tier: GovernanceTier,
) -> u64 {
    match direction(update, current) {
        Direction::Tighten => 0,
        Direction::Loosen => declared_tier.default_duration_ms(),
    }
}

/// Valide que le palier déclaré respecte le minimum du paramètre (table §2).
///
/// Retourne `Err(message)` si `declared_tier < min_tier(update)` — le message
/// est non-sensible (noms de palier), remonté à l'opérateur (code `3071`).
pub fn validate_tier(update: &ConfigUpdate, declared_tier: GovernanceTier) -> Result<(), String> {
    let required = min_tier(update);
    if declared_tier < required {
        return Err(format!(
            "governance tier too low: {} requires at least tier {} (declared {})",
            update.description(),
            required.as_str(),
            declared_tier.as_str()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Config par défaut pour les tests de direction (max_mint_per_block = 1_000_000).
    fn cfg() -> RuntimeConfig {
        RuntimeConfig::default()
    }

    #[test]
    fn min_tier_table_golden() {
        // Golden hardcodés (anti-tautologie) : les paliers attendus par paramètre.
        assert_eq!(min_tier(&ConfigUpdate::SetFeeRate { bps: 100 }), GovernanceTier::Operator);
        assert_eq!(min_tier(&ConfigUpdate::SetBurnRate { bps: 100 }), GovernanceTier::Policy);
        assert_eq!(min_tier(&ConfigUpdate::SetMintEnabled { enabled: true }), GovernanceTier::Policy);
        assert_eq!(min_tier(&ConfigUpdate::SetMaxMint { amount: 5 }), GovernanceTier::Policy);
        // Batch = max des composants (Operator + Policy ⇒ Policy).
        let batch = ConfigUpdate::BatchUpdate(vec![
            ConfigUpdate::SetFeeRate { bps: 1 },
            ConfigUpdate::SetBurnRate { bps: 1 },
        ]);
        assert_eq!(min_tier(&batch), GovernanceTier::Policy);
        println!("min_tier: fee=Operator, burn=Policy, mint_enabled=Policy, batch(op,pol)=Policy ✓");
    }

    #[test]
    fn tier_ordering_is_increasing_impact() {
        assert!(GovernanceTier::Operator < GovernanceTier::Policy);
        assert!(GovernanceTier::Policy < GovernanceTier::Constitution);
        println!("tier order: Operator < Policy < Constitution ✓");
    }

    #[test]
    fn validate_tier_rejects_below_minimum() {
        // Proposer un SetBurnRate (min Policy) en Operator → rejeté (G4).
        let burn = ConfigUpdate::SetBurnRate { bps: 100 };
        let err = validate_tier(&burn, GovernanceTier::Operator).unwrap_err();
        println!("validate_tier(burn, Operator) → Err: {err}");
        assert!(err.contains("requires at least tier policy"), "must name the required tier: {err}");
        // Policy ou Constitution → OK.
        assert!(validate_tier(&burn, GovernanceTier::Policy).is_ok());
        assert!(validate_tier(&burn, GovernanceTier::Constitution).is_ok());
    }

    #[test]
    fn direction_tighten_loosen_golden() {
        let c = cfg();
        // Couper le mint = tighten ; le reprendre = loosen.
        assert_eq!(direction(&ConfigUpdate::SetMintEnabled { enabled: false }, &c), Direction::Tighten);
        assert_eq!(direction(&ConfigUpdate::SetMintEnabled { enabled: true }, &c), Direction::Loosen);
        // Baisser le plafond/bloc (< 1_000_000) = tighten ; le hausser = loosen.
        assert_eq!(direction(&ConfigUpdate::SetMaxMint { amount: 1 }, &c), Direction::Tighten);
        assert_eq!(direction(&ConfigUpdate::SetMaxMint { amount: 9_000_000 }, &c), Direction::Loosen);
        // Un fee change n'a pas de passe-droit → loosen.
        assert_eq!(direction(&ConfigUpdate::SetFeeRate { bps: 100 }, &c), Direction::Loosen);
        println!("direction: mint_off=Tighten, mint_on=Loosen, maxmint↓=Tighten, maxmint↑=Loosen, fee=Loosen ✓");
    }

    #[test]
    fn required_timelock_asymmetry_golden() {
        let c = cfg();
        const DAY_MS: u64 = 86_400_000;
        // Tighten (couper le mint) → 0 (instantané), quel que soit le palier.
        assert_eq!(
            required_timelock_ms(&ConfigUpdate::SetMintEnabled { enabled: false }, &c, GovernanceTier::Constitution),
            0
        );
        // Loosen (reprendre le mint) → durée pleine du palier déclaré (Policy = 15 j).
        assert_eq!(
            required_timelock_ms(&ConfigUpdate::SetMintEnabled { enabled: true }, &c, GovernanceTier::Policy),
            15 * DAY_MS
        );
        // Loosen fee en Operator → 7 j.
        assert_eq!(
            required_timelock_ms(&ConfigUpdate::SetFeeRate { bps: 100 }, &c, GovernanceTier::Operator),
            7 * DAY_MS
        );
        println!("timelock: mint_off=0 (instant), mint_on(Policy)=15d, fee(Operator)=7d ✓");
    }

    #[test]
    fn batch_tighten_only_if_all_tighten() {
        let c = cfg();
        // Tous tighten → tighten (instant).
        let all_tighten = ConfigUpdate::BatchUpdate(vec![
            ConfigUpdate::SetMintEnabled { enabled: false },
            ConfigUpdate::SetMaxMint { amount: 1 },
        ]);
        assert_eq!(direction(&all_tighten, &c), Direction::Tighten);
        // Un loosen caché → tout le batch devient loosen (pas de passe-droit).
        let mixed = ConfigUpdate::BatchUpdate(vec![
            ConfigUpdate::SetMintEnabled { enabled: false }, // tighten
            ConfigUpdate::SetFeeRate { bps: 100 },           // loosen
        ]);
        assert_eq!(direction(&mixed, &c), Direction::Loosen);
        // Batch VIDE → Loosen (jamais Tighten par vacuité de `all()`) : un batch
        // vide ne doit pas décrocher un timelock instantané.
        assert_eq!(direction(&ConfigUpdate::BatchUpdate(vec![]), &c), Direction::Loosen);
        println!("batch: all-tighten=Tighten, mixed=Loosen, empty=Loosen (no smuggled/vacuous loosen) ✓");
    }
}
