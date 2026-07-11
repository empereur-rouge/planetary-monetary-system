use crate::Dag;
use crate::validations::amount::{amount_parse_non_neg_dec, amount_parse_pos_dec};
use crate::validations::check::ValidatePolicy;
use crate::validations::conditions::{check_spend_authorization, validate_output_conditions};
use crate::validations::signature::verify_tx_signatures;
use pms_errors::ValidationError;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

/// Addition `Decimal` VÉRIFIÉE — rejette proprement un overflow au lieu de
/// paniquer.
///
/// Audit S2 (v0.9.9) : la somme des montants (outputs surtout, contrôlés par
/// l'émetteur) utilisait `+`/`+=` bruts, qui PANIQUENT quand le total dépasse
/// `Decimal::MAX` (≈ 7.92e28). Un attaquant pouvait soumettre une tx avec des
/// outputs proches du max dont la somme déborde → panic dans la validation =
/// DoS. On somme désormais via `checked_add`.
#[inline]
fn checked_sum(acc: Decimal, x: Decimal) -> Result<Decimal, ValidationError> {
    acc.checked_add(x).ok_or(ValidationError::InvalidAmount {
        reason: "amount sum overflow".to_string(),
    })
}

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
        out_sum = checked_sum(out_sum, amount_parse_pos_dec(&o.amount)?)?;
    }
    let need = checked_sum(out_sum, fee)?;

    // Appariement input[i] ↔ unlock[i] — déjà garanti dans le flux
    // `validate_block` (verify_tx_signatures), re-vérifié ici pour que la
    // fonction reste sûre appelée seule (sinon les conditions seraient
    // silencieusement sautées sur les inputs sans unlock).
    if tx.inputs.len() != tx.unlocks.len() {
        return Err(ValidationError::InvalidSignature(format!(
            "inputs/unlocks count mismatch: {} inputs, {} unlocks",
            tx.inputs.len(),
            tx.unlocks.len()
        )));
    }

    // Résout les outputs dépensés depuis les blocs du DAG RAM, puis applique
    // LES MÊMES helpers que le hot path (check_input_time_locks /
    // check_spend_authorization) — la règle ne peut pas diverger.
    let mut prev_outs: Vec<TxOutput> = Vec::with_capacity(tx.inputs.len());
    let mut in_sum = Decimal::ZERO;
    for inp in &tx.inputs {
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
        in_sum = checked_sum(in_sum, amount_parse_pos_dec(&prev_out.amount)?)?;
        prev_outs.push(prev_out.clone());
    }

    check_input_time_locks(&prev_outs, crate::utxo::current_time_ms())?;
    check_spend_authorization(&tx.unlocks, &prev_outs)?;

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
    check_asset_conservation_with_demurrage(tx, input_outputs, &HashMap::new(), 0)
}

/// Conservation par asset, variante demurrage-aware (protocole 2.5).
///
/// `demurrage_rates` = `asset_id -> bps_per_day` pour les assets opt-in
/// (résolu par l'appelant depuis le token registry). Pour ces assets, la
/// valeur d'input prise en compte est la valeur **effective** après décote
/// ([`crate::validations::demurrage::effective_value`], horloge `now_ms`) et
/// la règle devient `sum(outputs) <= sum(effective_inputs)` — l'écart est la
/// décote brûlée implicitement. Les assets hors map gardent la conservation
/// STRICTE (audit M-7).
pub fn check_asset_conservation_with_demurrage(
    tx: &Transaction,
    input_outputs: &[TxOutput],
    demurrage_rates: &HashMap<String, u32>,
    now_ms: u64,
) -> Result<(), ValidationError> {
    // Grouper les inputs par asset_id — valeur effective pour les assets à
    // demurrage, nominale sinon.
    let mut inputs_by_asset: HashMap<Option<String>, Decimal> = HashMap::new();
    for out in input_outputs {
        let nominal = amount_parse_pos_dec(&out.amount)?;
        let rate = out
            .asset_id
            .as_ref()
            .and_then(|a| demurrage_rates.get(a))
            .copied()
            .unwrap_or(0);
        let value = if rate > 0 {
            crate::validations::demurrage::effective_value(nominal, out.created_at, now_ms, rate)
        } else {
            nominal
        };
        let e = inputs_by_asset
            .entry(out.asset_id.clone())
            .or_insert(Decimal::ZERO);
        *e = checked_sum(*e, value)?;
    }

    // Grouper les outputs par asset_id
    let mut outputs_by_asset: HashMap<Option<String>, Decimal> = HashMap::new();
    for o in &tx.outputs {
        let amount = amount_parse_pos_dec(&o.amount)?;
        let e = outputs_by_asset
            .entry(o.asset_id.clone())
            .or_insert(Decimal::ZERO);
        *e = checked_sum(*e, amount)?;
    }

    // Conservation par asset
    for (asset_id, in_sum) in &inputs_by_asset {
        let out_sum = outputs_by_asset
            .get(asset_id)
            .copied()
            .unwrap_or(Decimal::ZERO);
        let has_demurrage = asset_id
            .as_ref()
            .and_then(|a| demurrage_rates.get(a))
            .copied()
            .unwrap_or(0)
            > 0;
        // Conservation par asset :
        //  - PMS natif (`asset_id == None`) : le frais de gas est BRÛLÉ à la
        //    source → `out ≤ in` ; la différence `in − out` est le frais détruit
        //    (finance les récompenses gatées ; invariant supply = genesis +
        //    Σémis − Σbrûlé). C'est le modèle UTXO standard (fee = in − out).
        //  - asset à demurrage : `out ≤ in` (la décote est brûlée).
        //  - token custom sans demurrage : égalité stricte (M-7) — le gas se
        //    paie en PMS, jamais dans le token.
        //  Dans TOUS les cas `out > in` reste interdit (création d'asset).
        let violated = if has_demurrage || asset_id.is_none() {
            out_sum > *in_sum
        } else {
            *in_sum != out_sum
        };
        if violated {
            tracing::warn!(
                "Asset balance mismatch: asset={:?}, inputs(effective)={}, outputs={}, demurrage={}",
                asset_id,
                in_sum,
                out_sum,
                has_demurrage
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
/// `demurrage_rates` = `asset_id -> bps_per_day` des assets opt-in présents
/// dans la tx (résolu par l'appelant depuis le token registry ; map vide =
/// conservation stricte pour tout, protocole 2.5).
///
/// Retourne les `TxOutput` des inputs (dans l'ordre) pour que l'appelant
/// puisse réutiliser les adresses sans re-fetch (ex: compliance freeze check).
pub async fn validate_transaction_full(
    utxos: &crate::utxo::ShardedUtxoSet,
    tx: &Transaction,
    policy: &ValidatePolicy,
    now_ms: u64,
    demurrage_rates: &HashMap<String, u32>,
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

    check_asset_conservation_with_demurrage(tx, &input_outputs, demurrage_rates, now_ms)?;

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
                in_sum = checked_sum(in_sum, amount_parse_pos_dec(&out.amount)?)?;
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

/// Validation COMPLÈTE d'un `TokenBurn` pour le hot path (plan §3.1, voie B).
///
/// Comme [`validate_transaction_full`] (signatures C-2, ownership C-1, time-lock
/// 2.1, anti-double-spend) MAIS avec une **conservation-burn** au lieu de la
/// conservation stricte M-7 : la valeur est intentionnellement détruite, donc
/// `Σ(inputs[asset]) = Σ(change[asset]) + amount`, le `amount` étant brûlé (la
/// supply baisse). Invariants supplémentaires :
/// - tous les inputs ET le change portent le même `asset_id` (le token brûlé) ;
/// - tous les inputs appartiennent à `owner`, et le change revient à `owner`
///   (un burn ne peut pas déplacer des fonds vers un tiers) ;
/// - `tx.fee == 0` (un burn ne paie pas de frais — il détruit) ;
/// - `amount > 0` et `amount == inputs − change` (cohérence du montant déclaré).
///
/// Retourne les `TxOutput` des inputs (pour le freeze-check de l'appelant, comme
/// `validate_transaction_full`).
pub async fn validate_token_burn_async(
    utxos: &crate::utxo::ShardedUtxoSet,
    tx: &Transaction,
    asset_id: &Option<String>,
    amount: &str,
    owner: &str,
    policy: &ValidatePolicy,
    now_ms: u64,
) -> Result<Vec<TxOutput>, ValidationError> {
    // 1. Appariement input[i] ↔ unlock[i] + au moins un input.
    if tx.inputs.is_empty() {
        return Err(ValidationError::MissingInput);
    }
    if tx.inputs.len() != tx.unlocks.len() {
        return Err(ValidationError::InvalidSignature(format!(
            "inputs/unlocks count mismatch: {} inputs, {} unlocks",
            tx.inputs.len(),
            tx.unlocks.len()
        )));
    }

    // 2. Pas de fee : un burn détruit, il ne paie pas.
    let fee = amount_parse_non_neg_dec(&tx.fee)?;
    if fee != Decimal::ZERO {
        return Err(ValidationError::InvalidAmount {
            reason: "TokenBurn must carry no fee (fee must be 0)".to_string(),
        });
    }

    // 3. Signatures (C-2) + 4. structure des conditions du change (2.2).
    verify_tx_signatures(tx, &policy.network_id)?;
    validate_output_conditions(&tx.outputs)?;

    // 5. Existence des inputs, autorisation de dépense (C-1), time-lock (2.1).
    let input_outputs = fetch_input_outputs(utxos, tx).await?;
    check_spend_authorization(&tx.unlocks, &input_outputs)?;
    check_input_time_locks(&input_outputs, now_ms)?;

    // 6. Conservation-burn : owner possède tout, même asset, inputs = change + amount.
    //    Attribution par IDENTITÉ d'adresse (pas string brute) : un token minté
    //    vers la pubkey hex « forme SDK » est sélectionné par la coin-selection
    //    multi-forme (`select_utxos_multi`) mais l'`owner` déclaré est en bech32m
    //    — sans le collapse hex↔bech32m, le burn serait rejeté à tort (parité
    //    avec `validate_settlement`, cf. Dual-Layer Consistency).
    let owner_id = crate::validations::ownership::address_identity(owner);
    let mut in_sum = Decimal::ZERO;
    for out in &input_outputs {
        // Fast path : la forme canonique (cas commun — le change est minté
        // canonique par le nœud) est byte-identique à `owner` → on évite le
        // hex-decode/SHA256 de `address_identity`. Le collapse hex↔bech32m ne
        // sert que pour un input détenu sous une AUTRE forme.
        if out.address != owner
            && crate::validations::ownership::address_identity(&out.address) != owner_id
        {
            return Err(ValidationError::InvalidSignature(format!(
                "TokenBurn input not owned by burner: input addr={}, owner={}",
                out.address, owner
            )));
        }
        if &out.asset_id != asset_id {
            return Err(ValidationError::AssetBalanceMismatch {
                asset_id: asset_id.clone(),
                inputs: format!("{:?}", out.asset_id),
                outputs: format!("{:?}", asset_id),
            });
        }
        in_sum = checked_sum(in_sum, amount_parse_pos_dec(&out.amount)?)?;
    }
    let mut change_sum = Decimal::ZERO;
    for o in &tx.outputs {
        if &o.asset_id != asset_id {
            return Err(ValidationError::AssetBalanceMismatch {
                asset_id: o.asset_id.clone(),
                inputs: format!("{:?}", asset_id),
                outputs: format!("{:?}", o.asset_id),
            });
        }
        if crate::validations::ownership::address_identity(&o.address) != owner_id {
            return Err(ValidationError::InvalidSignature(format!(
                "TokenBurn change must return to burner: output addr={}, owner={}",
                o.address, owner
            )));
        }
        change_sum = checked_sum(change_sum, amount_parse_pos_dec(&o.amount)?)?;
    }

    // amount détruit = inputs − change ; doit égaler le montant déclaré et > 0.
    let declared = amount_parse_pos_dec(amount)?;
    let burned = in_sum
        .checked_sub(change_sum)
        .ok_or(ValidationError::InvalidAmount {
            reason: "TokenBurn change exceeds inputs".to_string(),
        })?;
    if burned != declared {
        return Err(ValidationError::AssetBalanceMismatch {
            asset_id: asset_id.clone(),
            inputs: in_sum.to_string(),
            outputs: format!("change={change_sum} + declared_burn={declared}"),
        });
    }

    Ok(input_outputs)
}
