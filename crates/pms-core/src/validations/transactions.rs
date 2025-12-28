use std::collections::HashSet;
use rust_decimal::Decimal;
use pms_errors::ValidationError;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction};
use crate::{Dag};
use crate::validations::amount::amount_parse_pos_dec;

pub fn utxo_no_double_spend(dag: &Dag, tx: &Transaction) -> Result<(), ValidationError> {
    let mut seen = HashSet::new();
    for inp in &tx.inputs {
        let key = (inp.out.txid.clone(), inp.out.index);
        if !seen.insert(key.clone()) {
            return Err(ValidationError::DoubleSpend); // doublon dans la même tx
        }
        // selon ce que tu as sous la main : spent_in_ram(...) ou spent_outpoints.contains(...)
        if dag.spent_outpoints.contains(&key) {
            return Err(ValidationError::DoubleSpend);
        }
    }
    Ok(())
}

pub fn utxo_sufficient_funds(dag: &Dag, tx: &Transaction) -> Result<(), ValidationError> {
    let fee = amount_parse_pos_dec(&tx.fee)?;
    let mut out_sum = Decimal::ZERO;
    for o in &tx.outputs { out_sum += amount_parse_pos_dec(&o.amount)?; }
    let need = out_sum + fee;

    let mut in_sum = Decimal::ZERO;
    for inp in &tx.inputs {
        let Some(prev_block) = dag.blocks.get(&inp.out.txid) else {
            return Err(ValidationError::MissingInput);
        };
        let prev_amount = match &prev_block.payload {
            Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) => {
                outputs.get(inp.out.index as usize)
                    .ok_or(ValidationError::MissingOutput)?.amount.clone()
            }
            Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(txp))) => {
                txp.outputs.get(inp.out.index as usize)
                    .ok_or(ValidationError::MissingOutput)?.amount.clone()
            }
            _ => return Err(ValidationError::MissingOutput),
        };
        in_sum += amount_parse_pos_dec(&prev_amount)?;
    }

    if in_sum < need { return Err(ValidationError::InsufficientFunds); }
    Ok(())
}