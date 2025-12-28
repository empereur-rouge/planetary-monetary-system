use pms_errors::ValidationError;
use pms_types::Transaction;
use crate::ValidatePolicy;

pub fn validate_fee_recipient_output(
    tx: &Transaction,
    policy: &ValidatePolicy,
) -> Result<(), ValidationError> {
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