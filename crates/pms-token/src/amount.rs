use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AmountError {
    #[error("format de montant invalide")]
    Parse,
    #[error("montant négatif interdit")]
    Negative,
    #[error("trop de décimales (max {0})")]
    TooManyDecimals(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount(#[serde(with = "rust_decimal::serde::str")] pub Decimal);

impl Amount {
    /// Parse une chaîne en Amount, avec validation du nombre de décimales.
    /// Voir chapitre 10.2 du Rust Book sur les traits pour comprendre pourquoi
    /// on implémente Display plutôt que to_string() directement.
    pub fn parse(s: &str, max_decimals: u32) -> Result<Self, AmountError> {
        let d = s.parse::<Decimal>().map_err(|_| AmountError::Parse)?;
        if d.is_sign_negative() {
            return Err(AmountError::Negative);
        }
        if d.scale() > max_decimals {
            return Err(AmountError::TooManyDecimals(max_decimals));
        }
        Ok(Self(d))
    }
}

/// Implémentation du trait Display pour Amount.
/// Cela fournit automatiquement la méthode to_string() via le trait ToString
/// qui est implémenté pour tout type implémentant Display.
impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // normalize() supprime les zéros trailing (ex: "1.00" -> "1")
        write!(f, "{}", self.0.normalize())
    }
}
