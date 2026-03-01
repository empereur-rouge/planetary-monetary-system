use pms_config::FeeTier;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Amount;

// ═══════════════════════════════════════════════════════════════════════════════
// FeePolicy - Politique de calcul des frais de transaction
// ═══════════════════════════════════════════════════════════════════════════════
//
// Mode linéaire : fee = base_fee + (amount * ratio)
// Mode paliers  : fee = base_fee + Σ(tranche_i * ratio_i)  (marginal)
//
// Exemple linéaire :
//   - base_fee = "0.001", ratio = "0.03" (3%)
//   - Pour 10 PMS : fee = 0.001 + (10 * 0.03) = 0.301 PMS
//
// Exemple paliers :
//   - palier 1 : 0..100 → 3%, palier 2 : 100..10000 → 1.5%, palier 3 : 10000+ → 0.5%
//   - Pour 200 PMS : fee = (100 * 0.03) + (100 * 0.015) = 3.0 + 1.5 = 4.5 PMS
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Error)]
pub enum FeeError {
    #[error("montant invalide: {0}")]
    Amount(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeePolicy {
    /// Frais fixes (ex: "0.001")
    pub base_fee: String,
    /// Ratio * amount (ex: "0.03" = 3%) — utilisé en mode linéaire
    pub ratio: String,
    /// Barème par paliers. Si non vide, remplace `ratio` pour le calcul.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<FeeTier>,
}

impl FeePolicy {
    /// Crée une politique avec frais fixe uniquement (pas de ratio).
    pub fn fixed(base_fee: &str) -> Self {
        Self {
            base_fee: base_fee.to_string(),
            ratio: "0".into(),
            tiers: Vec::new(),
        }
    }

    /// Crée une nouvelle politique de frais linéaire.
    /// # Arguments
    /// * `base_fee` - Frais fixe minimum (ex: "0.0000001")
    /// * `ratio` - Pourcentage du montant (ex: "0.03" pour 3%)
    pub fn new(base_fee: &str, ratio: &str) -> Self {
        Self {
            base_fee: base_fee.into(),
            ratio: ratio.into(),
            tiers: Vec::new(),
        }
    }

    /// Crée une politique de frais par paliers marginaux.
    /// # Arguments
    /// * `base_fee` - Frais fixe minimum
    /// * `tiers` - Paliers ordonnés par `up_to` croissant, le dernier avec `up_to: None`
    pub fn tiered(base_fee: &str, tiers: Vec<FeeTier>) -> Self {
        Self {
            base_fee: base_fee.into(),
            ratio: "0".into(),
            tiers,
        }
    }

    /// Indique si la politique utilise des paliers.
    pub fn is_tiered(&self) -> bool {
        !self.tiers.is_empty()
    }

    /// Calcule les frais pour un montant donné.
    ///
    /// # Mode linéaire
    /// `fee = base_fee + (amount * ratio)`
    ///
    /// # Mode paliers (marginal)
    /// `fee = base_fee + Σ(min(remaining, tier_width) * tier.ratio)`
    ///
    /// # Garantie de précision
    /// Le résultat est automatiquement arrondi à 8 décimales grâce au type Amount.
    pub fn compute_fee(&self, amount: &str) -> Result<Amount, FeeError> {
        let base = Amount::parse_pms(&self.base_fee)
            .map_err(|e| FeeError::Amount(format!("base_fee: {}", e)))?;

        if self.tiers.is_empty() {
            // Mode linéaire (backward compat)
            let a = Amount::parse_pms(amount)
                .map_err(|e| FeeError::Amount(format!("amount: {}", e)))?;
            let r = Amount::parse_pms(&self.ratio)
                .map_err(|e| FeeError::Amount(format!("ratio: {}", e)))?;
            Ok(base + (a * r))
        } else {
            // Mode paliers marginaux
            self.compute_tiered_fee(amount, base)
        }
    }

    /// Calcul marginal par paliers.
    /// Chaque palier taxe uniquement la portion du montant dans sa tranche.
    fn compute_tiered_fee(&self, amount: &str, base: Amount) -> Result<Amount, FeeError> {
        let total_amount =
            Amount::parse_pms(amount).map_err(|e| FeeError::Amount(format!("amount: {}", e)))?;

        let mut fee = base;
        let mut remaining = total_amount.inner();
        let mut prev_boundary = Decimal::ZERO;

        for tier in &self.tiers {
            if remaining <= Decimal::ZERO {
                break;
            }

            let tier_ratio = Amount::parse_pms(&tier.ratio)
                .map_err(|e| FeeError::Amount(format!("tier ratio: {}", e)))?;

            let taxable_in_tier = match &tier.up_to {
                Some(upper_str) => {
                    let upper = Amount::parse_pms(upper_str)
                        .map_err(|e| FeeError::Amount(format!("tier up_to: {}", e)))?;
                    let tier_width = upper.inner() - prev_boundary;
                    let capped = if remaining < tier_width {
                        remaining
                    } else {
                        tier_width
                    };
                    prev_boundary = upper.inner();
                    capped
                }
                None => remaining, // Dernier palier : tout le restant
            };

            fee += Amount::from_decimal(taxable_in_tier) * tier_ratio;
            remaining -= taxable_in_tier;
        }

        Ok(fee)
    }

    /// Retourne le total (amount + fee) au format Amount.
    pub fn total(&self, amount: &str) -> Result<Amount, FeeError> {
        let a =
            Amount::parse_pms(amount).map_err(|e| FeeError::Amount(format!("amount: {}", e)))?;
        let fee = self.compute_fee(amount)?;
        Ok(a + fee)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests unitaires
// ═══════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;

    // --- Tests linéaires (backward compat) ---

    #[test]
    fn test_fixed_policy() {
        let policy = FeePolicy::fixed("0.001");
        let fee = policy.compute_fee("100").unwrap();
        assert_eq!(fee.to_string(), "0.001");
    }

    #[test]
    fn test_ratio_policy() {
        let policy = FeePolicy::new("0", "0.03"); // 3% sans frais fixe
        let fee = policy.compute_fee("10").unwrap();
        // 10 * 0.03 = 0.3
        assert_eq!(fee.to_string(), "0.3");
    }

    #[test]
    fn test_combined_policy() {
        let policy = FeePolicy::new("0.001", "0.03"); // 0.001 + 3%
        let fee = policy.compute_fee("10").unwrap();
        // 0.001 + (10 * 0.03) = 0.001 + 0.3 = 0.301
        assert_eq!(fee.to_string(), "0.301");
    }

    #[test]
    fn test_fee_precision_bug_fixed() {
        let policy = FeePolicy::new("0", "0.03");
        let fee = policy.compute_fee("0.03549294").unwrap();

        let fee_str = fee.to_string();
        if let Some(dot_pos) = fee_str.find('.') {
            let decimals = fee_str.len() - dot_pos - 1;
            assert!(
                decimals <= 8,
                "Fee '{}' has {} decimals, expected <= 8",
                fee_str,
                decimals
            );
        }
    }

    #[test]
    fn test_total() {
        let policy = FeePolicy::new("0.001", "0.01"); // 0.001 + 1%
        let total = policy.total("10").unwrap();
        // amount=10, fee=0.001+(10*0.01)=0.101, total=10.101
        assert_eq!(total.to_string(), "10.101");
    }

    // --- Tests paliers ---

    #[test]
    fn test_tiered_single_tier() {
        // Montant entièrement dans le 1er palier
        let policy = FeePolicy::tiered(
            "0",
            vec![
                FeeTier {
                    up_to: Some("100".into()),
                    ratio: "0.03".into(),
                },
                FeeTier {
                    up_to: None,
                    ratio: "0.005".into(),
                },
            ],
        );
        // 50 PMS : tout dans tier 1 → 50 * 0.03 = 1.5
        let fee = policy.compute_fee("50").unwrap();
        assert_eq!(fee.to_string(), "1.5");
    }

    #[test]
    fn test_tiered_across_two_tiers() {
        let policy = FeePolicy::tiered(
            "0",
            vec![
                FeeTier {
                    up_to: Some("100".into()),
                    ratio: "0.03".into(),
                },
                FeeTier {
                    up_to: None,
                    ratio: "0.005".into(),
                },
            ],
        );
        // 200 PMS : 100*0.03=3.0 + 100*0.005=0.5 → 3.5
        let fee = policy.compute_fee("200").unwrap();
        assert_eq!(fee.to_string(), "3.5");
    }

    #[test]
    fn test_tiered_three_tiers() {
        let policy = FeePolicy::tiered(
            "0",
            vec![
                FeeTier {
                    up_to: Some("100".into()),
                    ratio: "0.03".into(),
                },
                FeeTier {
                    up_to: Some("10000".into()),
                    ratio: "0.015".into(),
                },
                FeeTier {
                    up_to: None,
                    ratio: "0.005".into(),
                },
            ],
        );
        // 15000 PMS :
        //   tier 1: 100 * 0.03     = 3.0
        //   tier 2: 9900 * 0.015   = 148.5
        //   tier 3: 5000 * 0.005   = 25.0
        //   total = 176.5
        let fee = policy.compute_fee("15000").unwrap();
        assert_eq!(fee.to_string(), "176.5");
    }

    #[test]
    fn test_tiered_with_base_fee() {
        let policy = FeePolicy::tiered(
            "0.001",
            vec![
                FeeTier {
                    up_to: Some("100".into()),
                    ratio: "0.03".into(),
                },
                FeeTier {
                    up_to: None,
                    ratio: "0.005".into(),
                },
            ],
        );
        // 200 PMS : base=0.001 + 100*0.03 + 100*0.005 = 0.001 + 3.0 + 0.5 = 3.501
        let fee = policy.compute_fee("200").unwrap();
        assert_eq!(fee.to_string(), "3.501");
    }

    #[test]
    fn test_tiered_exact_boundary() {
        let policy = FeePolicy::tiered(
            "0",
            vec![
                FeeTier {
                    up_to: Some("100".into()),
                    ratio: "0.03".into(),
                },
                FeeTier {
                    up_to: None,
                    ratio: "0.01".into(),
                },
            ],
        );
        // Exactement 100 PMS : tout dans tier 1 → 100 * 0.03 = 3.0
        let fee = policy.compute_fee("100").unwrap();
        assert_eq!(fee.to_string(), "3");
    }

    #[test]
    fn test_tiered_zero_amount() {
        let policy = FeePolicy::tiered(
            "0.001",
            vec![
                FeeTier {
                    up_to: Some("100".into()),
                    ratio: "0.03".into(),
                },
                FeeTier {
                    up_to: None,
                    ratio: "0.005".into(),
                },
            ],
        );
        // 0 PMS : juste le base fee
        let fee = policy.compute_fee("0").unwrap();
        assert_eq!(fee.to_string(), "0.001");
    }

    #[test]
    fn test_is_tiered() {
        let linear = FeePolicy::new("0", "0.03");
        assert!(!linear.is_tiered());

        let tiered = FeePolicy::tiered(
            "0",
            vec![FeeTier {
                up_to: None,
                ratio: "0.01".into(),
            }],
        );
        assert!(tiered.is_tiered());
    }

    #[test]
    fn test_tiered_total() {
        let policy = FeePolicy::tiered(
            "0",
            vec![
                FeeTier {
                    up_to: Some("100".into()),
                    ratio: "0.03".into(),
                },
                FeeTier {
                    up_to: None,
                    ratio: "0.01".into(),
                },
            ],
        );
        // 150 PMS : fee = 100*0.03 + 50*0.01 = 3.0 + 0.5 = 3.5
        // total = 150 + 3.5 = 153.5
        let total = policy.total("150").unwrap();
        assert_eq!(total.to_string(), "153.5");
    }
}
