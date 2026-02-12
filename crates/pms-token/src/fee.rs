use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Amount;

// ═══════════════════════════════════════════════════════════════════════════════
// FeePolicy - Politique de calcul des frais de transaction
// ═══════════════════════════════════════════════════════════════════════════════
//
// Cette structure définit comment les frais sont calculés pour une transaction.
// La formule est : fee = base_fee + (amount * ratio)
//
// Exemple :
//   - base_fee = "0.001" (frais fixe minimum)
//   - ratio = "0.03" (3% du montant)
//   - Pour un transfert de 10 PMS : fee = 0.001 + (10 * 0.03) = 0.301 PMS
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
    /// Ratio * amount (ex: "0.03" = 3%)
    pub ratio: String,
}

impl FeePolicy {
    /// Crée une politique avec frais fixe uniquement (pas de ratio).
    pub fn fixed(base_fee: &str) -> Self {
        Self {
            base_fee: base_fee.to_string(),
            ratio: "0".into(),
        }
    }

    /// Crée une nouvelle politique de frais.
    ///   - base_fee = "0.0000001" (frais fixe minimum)
    /// # Arguments
    /// * `base_fee` - Frais fixe minimum (ex: "0.0000001")
    /// * `ratio` - Pourcentage du montant (ex: "0.03" pour 3%)
    pub fn new(base_fee: &str, ratio: &str) -> Self {
        Self {
            base_fee: base_fee.into(),
            ratio: ratio.into(),
        }
    }

    /// Calcule les frais pour un montant donné.
    ///
    /// # Formule
    /// `fee = base_fee + (amount * ratio)`
    ///
    /// # Garantie de précision
    /// Le résultat est automatiquement arrondi à 8 décimales grâce au type Amount.
    /// Plus de problème de fees à 11 décimales comme 0.00106478824 !
    ///
    /// # Exemple
    /// ```ignore
    /// let policy = FeePolicy::new("0.001", "0.03");  // 0.1% fixe + 3%
    /// let fee = policy.compute_fee("10.0")?;         // = 0.301 PMS
    /// ```
    pub fn compute_fee(&self, amount: &str) -> Result<Amount, FeeError> {
        // Parse les 3 valeurs en Amount (validation + arrondi automatique)
        let a =
            Amount::parse_pms(amount).map_err(|e| FeeError::Amount(format!("amount: {}", e)))?;
        let base = Amount::parse_pms(&self.base_fee)
            .map_err(|e| FeeError::Amount(format!("base_fee: {}", e)))?;
        let r = Amount::parse_pms(&self.ratio)
            .map_err(|e| FeeError::Amount(format!("ratio: {}", e)))?;

        // Grâce aux opérateurs impl sur Amount, le résultat est
        // automatiquement arrondi à 8 décimales !
        // Avant: let fee = base.0 + (a.0 * r.0); // pouvait avoir 11+ décimales
        // Maintenant: le * et + arrondissent chacun à 8 décimales
        let fee = base + (a * r);

        Ok(fee)
    }

    /// Retourne le total (amount + fee) au format string.
    ///
    /// Utile pour calculer combien l'utilisateur doit avoir au minimum
    /// pour effectuer un transfert.
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
        // Ce test vérifie que le bug des 11 décimales est corrigé
        // Avant: 0.03549294 * 0.03 = 0.00106478820 (11 décimales)
        // Maintenant: arrondi à 8 décimales maximum
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
}
