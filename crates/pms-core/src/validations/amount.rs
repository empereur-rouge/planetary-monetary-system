use crate::ValidatePolicy;
use pms_errors::ValidationError;
use pms_types::{Transaction, TxOutput};
use rust_decimal::Decimal;
use std::str::FromStr;

pub fn amount_parse_pos_dec(s: &str) -> Result<Decimal, ValidationError> {
    let d = Decimal::from_str(s).map_err(|_| ValidationError::InvalidAmount {
        reason: format!("amount not a valid decimal: '{}'", s),
    })?;
    if d <= Decimal::ZERO {
        return Err(ValidationError::InvalidAmount {
            reason: format!("amount must be positive: '{}'", s),
        });
    }
    Ok(d)
}

/// Parses a decimal amount that must be >= 0 (non-negative). Used for fees.
pub fn amount_parse_non_neg_dec(s: &str) -> Result<Decimal, ValidationError> {
    let d = Decimal::from_str(s).map_err(|_| ValidationError::InvalidAmount {
        reason: format!("amount not a valid decimal: '{}'", s),
    })?;
    if d < Decimal::ZERO {
        return Err(ValidationError::InvalidAmount {
            reason: format!("amount must be non-negative: '{}'", s),
        });
    }
    Ok(d)
}

/// Vérifie que tous les outputs ont des montants positifs (chaînes décimales > 0).
pub fn amounts_positive_outputs(outs: &[TxOutput]) -> Result<(), ValidationError> {
    for o in outs {
        let _ = amount_parse_pos_dec(&o.amount)?;
    }
    Ok(())
}

/// Vérifie montants/fees + quotas (taille/IO) d'une Tx (MVP).

pub fn tx_amounts_valid(tx: &Transaction, p: &ValidatePolicy) -> Result<(), ValidationError> {
    if tx.inputs.len() > p.max_inputs {
        return Err(ValidationError::TooManyInputs);
    }
    if tx.outputs.len() > p.max_outputs {
        return Err(ValidationError::TooManyOutputs);
    }

    let raw = serde_json::to_vec(tx).map_err(|_| ValidationError::Other("serde tx"))?;
    if raw.len() > p.max_tx_bytes {
        return Err(ValidationError::TxTooLarge);
    }

    for o in &tx.outputs {
        let _ = amount_parse_pos_dec(&o.amount)?;
    }
    let fee = amount_parse_non_neg_dec(&tx.fee)?; // Fee can be zero (implicit fee model)

    // Frais: min = 0, max = constante pour l’instant
    if fee < Decimal::ZERO {
        return Err(ValidationError::InvalidAmount {
            reason: "tx.fee is negative".into(),
        });
    }

    if fee > p.max_fee_per_tx {
        return Err(ValidationError::FeeTooHigh {
            fee: fee.to_string(),
            max: p.max_fee_per_tx.to_string(),
        });
    }
    Ok(())
}

/// Test de positivité « chaîne décimale » (strict minimal).
pub fn amount_is_positive_decimal(s: &str) -> bool {
    if s.starts_with('-') {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut dots = 0;
    for &c in bytes {
        if c == b'.' {
            dots += 1;
            if dots > 1 {
                return false;
            }
        } else if !(b'0'..=b'9').contains(&c) {
            return false;
        }
    }
    // interdit "0", "0.0", "000.000"
    s.trim_matches('0').trim_matches('.').len() > 0
}
