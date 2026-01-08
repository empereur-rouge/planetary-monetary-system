use crate::ValidatePolicy;
use pms_errors::ValidationError;
use pms_types::Transaction;
use rust_decimal::Decimal;
use std::str::FromStr;

pub fn validate_fee_recipient_output(
    tx: &Transaction,
    policy: &ValidatePolicy,
) -> Result<(), ValidationError> {
    // 1) Vérification du Split Fee Platform (si configuré)
    // 1) Vérification du Split Fee Platform (si configuré)
    if let Some(platform_addr) = &policy.platform_address {
        if policy.platform_fee_ratio > Decimal::ZERO {
            // Calculer combien a été envoyé à la plateforme via les outputs
            let mut platform_sent = Decimal::ZERO;
            for out in &tx.outputs {
                if out.address == *platform_addr {
                    if let Ok(amt) = Decimal::from_str(&out.amount) {
                        platform_sent += amt;
                    }
                }
            }

            // tx.fee est considéré comme la "Miner Fee" (implicite/restante)
            let miner_fee = Decimal::from_str(&tx.fee).unwrap_or(Decimal::ZERO);

            // Total Fee générée = Miner Fee + Platform Fee
            let total_fee = miner_fee + platform_sent;

            if total_fee > Decimal::ZERO {
                let expected_platform_fee = total_fee * policy.platform_fee_ratio;

                // On vérifie si ce qu'on a envoyé couvre le ratio requis
                if platform_sent < expected_platform_fee {
                    return Err(ValidationError::InvalidAmount {
                        reason: format!(
                            "Transaction must include a platform fee output of at least {} to {} (sent: {}, total_fee: {})",
                            expected_platform_fee, platform_addr, platform_sent, total_fee
                        ),
                    });
                }
            }
        }
    }

    if !policy.enforce_fee_recipient {
        return Ok(());
    }

    if policy.allowed_fee_addresses.is_empty() {
        return Ok(()); // pas de liste => pas de règle applicable
    }

    // Convention : dernier output = fee output (quand fee>0)
    if tx.outputs.len() < 2 {
        return Ok(());
    }

    let fee_out = tx.outputs.last().unwrap();
    let ok = policy
        .allowed_fee_addresses
        .iter()
        .any(|a| a.eq_ignore_ascii_case(&fee_out.address));

    if !ok {
        return Err(ValidationError::InvalidFeeRecipient {
            address: fee_out.address.clone(),
        });
    }

    Ok(())
}
