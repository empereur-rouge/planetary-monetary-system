use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Amount, PLANETARY_MONETARY_SYSTEM as PMS};

#[derive(Debug, Error)]
pub enum FeeError {
    #[error("montant invalide")]
    Amount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeePolicy {
    /// Frais fixes (ex: "0.001")
    pub base_fee: String,
    /// Ratio *amount (ex: "0.001" = 0.1%)
    pub ratio: String,
    /// Nombre max de décimales autorisées par le protocole (souvent = token.decimals)
    pub precision: u32,
}

impl FeePolicy {
    pub fn fixed(base_fee: &str) -> Self {
        Self {
            base_fee: base_fee.to_string(),
            ratio: "0".into(),
            precision: PMS.decimals,
        }
    }

    pub fn new(base_fee: &str, ratio: &str, precision: u32) -> Self {
        Self {
            base_fee: base_fee.into(),
            ratio: ratio.into(),
            precision,
        }
    }

    /// Calcule la fee pour un `amount` (string) et renvoie une string (stable côté API).
    pub fn compute_fee(&self, amount: &str) -> Result<String, FeeError> {
        let a = Amount::parse(amount, self.precision)
            .map_err(|_| FeeError::Amount)?
            .0;
        let base = Amount::parse(&self.base_fee, self.precision)
            .map_err(|_| FeeError::Amount)?
            .0;
        let r = Amount::parse(&self.ratio, self.precision)
            .map_err(|_| FeeError::Amount)?
            .0;

        let fee = base + (a * r);
        Ok(fee.normalize().to_string())
    }

    /// Retourne `total = amount + fee` au format string.
    pub fn total(&self, amount: &str) -> Result<String, FeeError> {
        let a = Amount::parse(amount, self.precision)
            .map_err(|_| FeeError::Amount)?
            .0;
        let f = Decimal::from_str_exact(&self.compute_fee(amount)?).unwrap();
        Ok((a + f).normalize().to_string())
    }
}
