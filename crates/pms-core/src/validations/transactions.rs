use crate::Dag;
use crate::validations::amount::{amount_parse_non_neg_dec, amount_parse_pos_dec};
use pms_errors::ValidationError;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

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
/// 3. Conservation par asset : sum(inputs[asset]) == sum(outputs[asset]) pour chaque asset.
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

    // 2. Grouper les inputs par asset_id
    let mut inputs_by_asset: HashMap<Option<String>, Decimal> = HashMap::new();
    for inp in &tx.inputs {
        let output_opt = utxos.get(&inp.out).await;
        match output_opt {
            Some(out) => {
                let amount = amount_parse_pos_dec(&out.amount)?;
                *inputs_by_asset.entry(out.asset_id.clone()).or_insert(Decimal::ZERO) += amount;
            }
            None => {
                tracing::warn!("Input missing: {:?}", inp.out);
                return Err(ValidationError::MissingInput);
            }
        }
    }

    // 3. Grouper les outputs par asset_id
    let mut outputs_by_asset: HashMap<Option<String>, Decimal> = HashMap::new();
    for o in &tx.outputs {
        let amount = amount_parse_pos_dec(&o.amount)?;
        *outputs_by_asset.entry(o.asset_id.clone()).or_insert(Decimal::ZERO) += amount;
    }

    // 4. Vérifier la conservation par asset
    for (asset_id, in_sum) in &inputs_by_asset {
        let out_sum = outputs_by_asset.get(asset_id).copied().unwrap_or(Decimal::ZERO);
        if *in_sum != out_sum {
            tracing::warn!(
                "Asset balance mismatch: asset={:?}, inputs={}, outputs={}",
                asset_id, in_sum, out_sum
            );
            return Err(ValidationError::AssetBalanceMismatch {
                asset_id: asset_id.clone(),
                inputs: in_sum.to_string(),
                outputs: out_sum.to_string(),
            });
        }
    }

    // 5. Vérifier qu'aucun output ne crée un asset sans input correspondant
    for (asset_id, _) in &outputs_by_asset {
        if !inputs_by_asset.contains_key(asset_id) {
            tracing::warn!("Output creates asset without input: {:?}", asset_id);
            return Err(ValidationError::AssetBalanceMismatch {
                asset_id: asset_id.clone(),
                inputs: "0".to_string(),
                outputs: outputs_by_asset[asset_id].to_string(),
            });
        }
    }

    Ok(())
}
