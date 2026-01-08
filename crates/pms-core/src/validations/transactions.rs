use crate::Dag;
use crate::validations::amount::{amount_parse_non_neg_dec, amount_parse_pos_dec};
use pms_errors::ValidationError;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction};
use rust_decimal::Decimal;
use std::collections::HashSet;

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
    let fee = amount_parse_non_neg_dec(&tx.fee)?; // Fee can be zero
    let mut out_sum = Decimal::ZERO;
    for o in &tx.outputs {
        out_sum += amount_parse_pos_dec(&o.amount)?;
    }
    let need = out_sum + fee;

    let mut in_sum = Decimal::ZERO;
    for inp in &tx.inputs {
        let Some(prev_block) = dag.blocks.get(&inp.out.txid) else {
            return Err(ValidationError::MissingInput);
        };
        let prev_amount = match &prev_block.payload {
            Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) => outputs
                .get(inp.out.index as usize)
                .ok_or(ValidationError::MissingOutput)?
                .amount
                .clone(),
            Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(txp))) => txp
                .outputs
                .get(inp.out.index as usize)
                .ok_or(ValidationError::MissingOutput)?
                .amount
                .clone(),
            _ => return Err(ValidationError::MissingOutput),
        };
        in_sum += amount_parse_pos_dec(&prev_amount)?;
    }

    if in_sum < need {
        return Err(ValidationError::InsufficientFunds);
    }
    Ok(())
}

/// Validation ASYNC sans lock global DAG, utilisant le ShardedUtxoSet.
/// Vérifie:
/// 1. Pas de doublons internes (inputs).
/// 2. Existence des inputs dans l'UTXO set (anti-double-spend + input exists).
/// 3. Solvabilité (Inputs >= Outputs + Fee).
pub async fn validate_transaction_async(
    utxos: &crate::utxo::ShardedUtxoSet,
    tx: &Transaction,
) -> Result<(), ValidationError> {
    // 1. Doublons internes
    let mut seen = HashSet::new();
    for inp in &tx.inputs {
        let key = (inp.out.txid.clone(), inp.out.index);
        if !seen.insert(key) {
            return Err(ValidationError::DoubleSpend);
        }
    }

    // 2. Récupération des inputs (lecture parallèle par shard)
    let fee = amount_parse_non_neg_dec(&tx.fee)?;
    let mut out_sum = Decimal::ZERO;
    for o in &tx.outputs {
        out_sum += amount_parse_pos_dec(&o.amount)?;
    }
    let need = out_sum + fee;

    let mut in_sum = Decimal::ZERO;

    for inp in &tx.inputs {
        // Lecture async sans bloquer tout le monde
        let output_opt = utxos.get(&inp.out).await;
        match output_opt {
            Some(out) => {
                in_sum += amount_parse_pos_dec(&out.amount)?;
            }
            None => {
                // Si pas dans l'UTXO set => soit n'existe pas, soit déjà dépensé.
                // Dans les deux cas : invalide.
                tracing::warn!("Input missing: {:?}", inp.out);
                return Err(ValidationError::MissingInput);
            }
        }
    }

    if in_sum < need {
        tracing::warn!(
            "Insufficient funds: in_sum={} need={} (out={} + fee={})",
            in_sum,
            need,
            out_sum,
            fee
        );
        return Err(ValidationError::InsufficientFunds);
    }

    Ok(())
}
