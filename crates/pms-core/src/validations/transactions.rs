use crate::Dag;
use crate::validations::amount::{amount_parse_non_neg_dec, amount_parse_pos_dec};
use crate::validations::check::ValidatePolicy;
use crate::validations::conditions::{check_spend_authorization, validate_output_conditions};
use crate::validations::signature::verify_tx_signatures;
use pms_errors::ValidationError;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput};
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

    // Time-lock 2.1 : même règle que le hot path (check_input_time_locks),
    // appliquée ici input par input pour éviter une seconde résolution.
    let now_ms = crate::utxo::current_time_ms();

    let mut in_sum = Decimal::ZERO;
    for (i, inp) in tx.inputs.iter().enumerate() {
        let Some(prev_block) = dag.blocks.get(&inp.out.txid) else {
            return Err(ValidationError::MissingInput);
        };
        let prev_out = match &prev_block.payload {
            Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) => outputs
                .get(inp.out.index as usize)
                .ok_or(ValidationError::MissingOutput)?,
            Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(txp))) => txp
                .outputs
                .get(inp.out.index as usize)
                .ok_or(ValidationError::MissingOutput)?,
            _ => return Err(ValidationError::MissingOutput),
        };
        if let Some(until) = prev_out.locked_until {
            if now_ms < until {
                return Err(ValidationError::OutputTimeLocked {
                    input_index: i,
                    until,
                    now: now_ms,
                });
            }
        }
        // Spend conditions 2.2 (chemin legacy) — même règle que le hot path.
        // `verify_tx_signatures` (appelé en amont par validate_block) garantit
        // déjà l'appariement inputs/unlocks et la validité crypto.
        if let Some(unlock) = tx.unlocks.get(i) {
            crate::validations::conditions::check_input_spend_condition(i, prev_out, unlock)?;
        }
        in_sum += amount_parse_pos_dec(&prev_out.amount)?;
    }

    if in_sum < need {
        return Err(ValidationError::InsufficientFunds);
    }
    Ok(())
}

/// Récupère le `TxOutput` de chaque input depuis le ShardedUtxoSet, dans
/// l'ordre des inputs, en rejetant :
/// - les doublons internes (même outpoint référencé deux fois) ;
/// - les inputs absents de l'UTXO set (déjà dépensés ou inexistants).
async fn fetch_input_outputs(
    utxos: &crate::utxo::ShardedUtxoSet,
    tx: &Transaction,
) -> Result<Vec<TxOutput>, ValidationError> {
    let mut seen = HashSet::new();
    let mut fetched = Vec::with_capacity(tx.inputs.len());
    for inp in &tx.inputs {
        let key = (inp.out.txid.clone(), inp.out.index);
        if !seen.insert(key) {
            return Err(ValidationError::DoubleSpend);
        }
        match utxos.get(&inp.out).await {
            Some(out) => fetched.push(out),
            None => {
                tracing::warn!("Input missing: {:?}", inp.out);
                return Err(ValidationError::MissingInput);
            }
        }
    }
    Ok(fetched)
}

/// Time-lock natif (protocole 2.1) : rejette la dépense d'un input dont le
/// `locked_until` (timestamp UNIX ms, porté par l'output on-DAG) est encore
/// dans le futur par rapport à `now_ms`.
///
/// `input_outputs` = les `TxOutput` dépensés, dans l'ordre des inputs.
/// Partagé entre le hot path (`validate_transaction_full`) et le chemin
/// legacy (`utxo_sufficient_funds` via `validate_block`) pour que la règle
/// ne puisse pas diverger entre les deux.
pub fn check_input_time_locks(
    input_outputs: &[TxOutput],
    now_ms: u64,
) -> Result<(), ValidationError> {
    for (i, out) in input_outputs.iter().enumerate() {
        if let Some(until) = out.locked_until {
            if now_ms < until {
                tracing::warn!(
                    "🚫 Time-locked input {} spent too early: locked until {}, now {}",
                    i,
                    until,
                    now_ms
                );
                return Err(ValidationError::OutputTimeLocked {
                    input_index: i,
                    until,
                    now: now_ms,
                });
            }
        }
    }
    Ok(())
}

/// Conservation stricte par asset : `sum(inputs[asset]) == sum(outputs[asset])`
/// pour chaque asset, et aucun output ne crée un asset sans input correspondant.
///
/// C'est la règle de conservation CANONIQUE du protocole (audit M-7) : le fee
/// déclaré dans `tx.fee` est purement informatif — la valeur des frais doit
/// être portée par un output explicite vers une adresse de frais, sinon la
/// conservation échoue. Aucun surplus n'est jamais brûlé implicitement.
///
/// `input_outputs` = les `TxOutput` dépensés, dans l'ordre des inputs
/// (typiquement fetchés depuis l'UTXO set par l'appelant). Public : réutilisé
/// par les handlers qui valident une tx AVANT chiffrement du payload
/// (`wallet_send_tx`), là où le hot path ne voit que le ciphertext.
pub fn check_asset_conservation(
    tx: &Transaction,
    input_outputs: &[TxOutput],
) -> Result<(), ValidationError> {
    // Grouper les inputs par asset_id
    let mut inputs_by_asset: HashMap<Option<String>, Decimal> = HashMap::new();
    for out in input_outputs {
        let amount = amount_parse_pos_dec(&out.amount)?;
        *inputs_by_asset
            .entry(out.asset_id.clone())
            .or_insert(Decimal::ZERO) += amount;
    }

    // Grouper les outputs par asset_id
    let mut outputs_by_asset: HashMap<Option<String>, Decimal> = HashMap::new();
    for o in &tx.outputs {
        let amount = amount_parse_pos_dec(&o.amount)?;
        *outputs_by_asset
            .entry(o.asset_id.clone())
            .or_insert(Decimal::ZERO) += amount;
    }

    // Conservation par asset
    for (asset_id, in_sum) in &inputs_by_asset {
        let out_sum = outputs_by_asset
            .get(asset_id)
            .copied()
            .unwrap_or(Decimal::ZERO);
        if *in_sum != out_sum {
            tracing::warn!(
                "Asset balance mismatch: asset={:?}, inputs={}, outputs={}",
                asset_id,
                in_sum,
                out_sum
            );
            return Err(ValidationError::AssetBalanceMismatch {
                asset_id: asset_id.clone(),
                inputs: in_sum.to_string(),
                outputs: out_sum.to_string(),
            });
        }
    }

    // Aucun output ne crée un asset sans input correspondant
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

/// Validation ASYNC sans lock global DAG, utilisant le ShardedUtxoSet.
/// Vérifie:
/// 1. Pas de doublons internes (inputs).
/// 2. Existence des inputs dans l'UTXO set (anti-double-spend + input exists).
/// 3. Conservation par asset : sum(inputs[asset]) == sum(outputs[asset]) pour chaque asset.
///
/// ⚠️ Ne vérifie NI les signatures NI l'ownership des inputs — pour le hot
/// path de production, utiliser [`validate_transaction_full`] qui couvre
/// l'autorisation complète (audit C-1/C-2).
pub async fn validate_transaction_async(
    utxos: &crate::utxo::ShardedUtxoSet,
    tx: &Transaction,
) -> Result<(), ValidationError> {
    let input_outputs = fetch_input_outputs(utxos, tx).await?;
    check_asset_conservation(tx, &input_outputs)
}

/// Validation COMPLÈTE d'une `TxUtxo` pour le hot path de production
/// (audit C-1 + C-2 + M-7). Ordre des checks, du moins cher au plus cher :
///
/// 1. **Appariement** : `inputs.len() == unlocks.len()` — chaque input[i] est
///    autorisé par unlock[i] (correspondance positionnelle).
/// 2. **Fee sanity (M-7)** : `tx.fee` parse en décimal non-négatif et
///    `<= policy.max_fee_per_tx`. Le fee est déclaratif — la conservation
///    stricte (étape 5) garantit qu'il correspond à un output explicite.
/// 3. **Signatures (C-2)** : chaque unlock porte une signature ECDSA valide
///    du message canonique `{network_id, inputs, outputs, fee}` (anti-replay
///    cross-chain inclus).
/// 4. **Spend conditions des outputs créés (protocole 2.2)** : structure des
///    conditions (`MultiSig` bien formée + adresse canonique, `HashLock`
///    SHA-256) via [`validate_output_conditions`].
/// 5. **Autorisation de dépense (C-1 généralisé)** : pour chaque input, la
///    condition de l'UTXO STOCKÉ est satisfaite par l'unlock apparié —
///    binding pubkey↔adresse (`PubKey`/None), quorum M-of-N (`MultiSig`),
///    préimage (`HashLock`) — via [`check_spend_authorization`].
/// 6. **Time-lock (protocole 2.1)** : aucun input `locked_until` dans le
///    futur ([`check_input_time_locks`], horloge = `now_ms`).
/// 7. **Existence + double-spend + conservation par asset** (règle canonique).
///
/// `now_ms` = horloge du validateur (timestamp UNIX ms) — paramètre explicite
/// pour la testabilité ; en production, le hot path passe le `now_ms` du
/// persist (même source que `now_ms_for_signers`).
///
/// Retourne les `TxOutput` des inputs (dans l'ordre) pour que l'appelant
/// puisse réutiliser les adresses sans re-fetch (ex: compliance freeze check).
pub async fn validate_transaction_full(
    utxos: &crate::utxo::ShardedUtxoSet,
    tx: &Transaction,
    policy: &ValidatePolicy,
    now_ms: u64,
) -> Result<Vec<TxOutput>, ValidationError> {
    // 1. Appariement input[i] ↔ unlock[i]
    if tx.inputs.len() != tx.unlocks.len() {
        return Err(ValidationError::InvalidSignature(format!(
            "inputs/unlocks count mismatch: {} inputs, {} unlocks",
            tx.inputs.len(),
            tx.unlocks.len()
        )));
    }

    // 2. Fee sanity (M-7) — non-négatif et borné
    let fee = amount_parse_non_neg_dec(&tx.fee)?;
    if fee > policy.max_fee_per_tx {
        return Err(ValidationError::FeeTooHigh {
            fee: fee.to_string(),
            max: policy.max_fee_per_tx.to_string(),
        });
    }

    // 3. Signatures de transaction (C-2) — principale + cosignatures MultiSig
    verify_tx_signatures(tx, &policy.network_id)?;

    // 4. Structure des conditions portées par les NOUVEAUX outputs (2.2)
    validate_output_conditions(&tx.outputs)?;

    // 5+6+7. Existence des inputs, puis autorisation (C-1 généralisé :
    // PubKey/MultiSig/HashLock), time-lock et conservation.
    let input_outputs = fetch_input_outputs(utxos, tx).await?;

    check_spend_authorization(&tx.unlocks, &input_outputs)?;

    check_input_time_locks(&input_outputs, now_ms)?;

    check_asset_conservation(tx, &input_outputs)?;

    Ok(input_outputs)
}

/// Validation ASYNC des inputs d'un BridgeLock.
/// Vérifie :
/// 1. Pas de doublons internes.
/// 2. Tous les inputs existent dans l'UTXO set.
/// 3. sum(inputs) >= amount demandé (pour le même asset_id).
pub async fn validate_bridge_lock_async(
    utxos: &crate::utxo::ShardedUtxoSet,
    inputs: &[TxInput],
    amount: &str,
    asset_id: &Option<String>,
) -> Result<(), ValidationError> {
    use crate::validations::amount::amount_parse_pos_dec;

    // 1. Doublons internes
    let mut seen = HashSet::new();
    for inp in inputs {
        let key = (inp.out.txid.clone(), inp.out.index);
        if !seen.insert(key) {
            return Err(ValidationError::DoubleSpend);
        }
    }

    // 2. Somme des inputs (par asset)
    let mut in_sum = Decimal::ZERO;
    for inp in inputs {
        let output_opt = utxos.get(&inp.out).await;
        match output_opt {
            Some(out) => {
                // Vérifier que l'asset_id correspond
                if &out.asset_id != asset_id {
                    return Err(ValidationError::AssetBalanceMismatch {
                        asset_id: asset_id.clone(),
                        inputs: format!("{:?}", out.asset_id),
                        outputs: format!("{:?}", asset_id),
                    });
                }
                in_sum += amount_parse_pos_dec(&out.amount)?;
            }
            None => {
                tracing::warn!("BridgeLock input missing: {:?}", inp.out);
                return Err(ValidationError::MissingInput);
            }
        }
    }

    // 3. sum(inputs) >= amount
    let required = amount_parse_pos_dec(amount)?;
    if in_sum < required {
        return Err(ValidationError::InsufficientFunds);
    }

    Ok(())
}
