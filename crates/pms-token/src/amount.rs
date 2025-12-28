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
pub struct Amount(
    #[serde(with = "rust_decimal::serde::str")] pub Decimal
);

impl Amount {
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

    pub fn to_string(&self) -> String {
        self.0.normalize().to_string()
    }
}
