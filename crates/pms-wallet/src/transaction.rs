use std::str::FromStr;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use pms_storage::{DagStorage, RedisStore};
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_types_transaction::{OutputId, Transaction, TxInput, TxOutput};

#[derive(Clone, Debug)]
struct Utxo { txid: String, vout: u32, amount: String }

const SCALE: u128 = 100_000_000;

fn amt_parse(s: &str) -> anyhow::Result<Decimal> {
    Decimal::from_str(s).map_err(|_| anyhow::anyhow!("amount invalide: {s}"))
}
fn amt_to_u128(dec: Decimal) -> anyhow::Result<u128> {
    (dec * Decimal::from(SCALE))
        .to_u128()
        .ok_or_else(|| anyhow::anyhow!("overflow amount"))
}
fn u128_to_amt(u: u128) -> Decimal {
    Decimal::from(u) / Decimal::from(SCALE)
}

pub async fn build_local_utxos_for_wallet(
    store: &RedisStore,
    sk_hex: &str,
    addr_candidates: &[String],
    limit_scan: usize,
) -> anyhow::Result<Vec<Utxo>> {
    // 1) scan déchiffré récent (tu peux paginer si besoin)
    let ids = store.recent_ids(limit_scan).await?;
    let blocks = store.get_blocks_by_ids(&ids).await?;

    // 2) collecte outputs gagnés (rewards + tx) et marque ceux dépensés par nos inputs
    use std::collections::{HashMap, HashSet};
    // key = (txid, index)
    let mut earned: HashMap<(String,u32), String> = HashMap::new();
    let mut spent:  HashSet<(String,u32)> = HashSet::new();


    for b in blocks {
        let Some(s) = &b.payload_json else { continue; };
        let Ok(PayloadEnvelope::Encrypted(enc)) = serde_json::from_str::<PayloadEnvelope>(s) else { continue; };
        let Ok(plain) = enc.decrypt_as_payload(sk_hex) else { continue; };

        match plain {
            PlainPayload::Reward { outputs } => {
                for (i, o) in outputs.iter().enumerate() {
                    if addr_candidates.contains(&o.address) {
                        // amount est déjà String -> on clone tel quel
                        earned.insert((b.id.clone(), i as u32), o.amount.clone());
                    }
                }
            }
            PlainPayload::TxUtxo(tx) => {
                // inputs dépensés
                for inp in &tx.inputs {
                    // nouveau schéma: inp.out.{txid,index}
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
                // outputs gagnés
                for (i, o) in tx.outputs.iter().enumerate() {
                    if addr_candidates.contains(&o.address) {
                        earned.insert((b.id.clone(), i as u32), o.amount.clone());
                    }
                }
            }
            _ => {}
        }
    }

    // 3) UTXOs = earned - spent
    let mut utxos = Vec::new();
    for ((txid, vout), amount) in earned {
        if !spent.contains(&(txid.clone(), vout)) {
            utxos.push(Utxo { txid, vout, amount });
        }
    }
    // tri du plus récent au plus ancien si tu veux (optionnel)
    Ok(utxos)
}

pub fn select_utxos(mut utxos: Vec<Utxo>, need: u128) -> anyhow::Result<(Vec<Utxo>, u128)> {
    // Trie décroissant (gros -> petit)
    utxos.sort_by(|a, b| {
        let da = Decimal::from_str(&a.amount).unwrap_or(Decimal::ZERO);
        let db = Decimal::from_str(&b.amount).unwrap_or(Decimal::ZERO);
        db.cmp(&da)
    });

    let mut picked = Vec::new();
    let mut sum = 0u128;

    for u in utxos {
        // Parse en Decimal
        let dec = Decimal::from_str(&u.amount)
            .map_err(|_| anyhow::anyhow!("invalid amount: {}", u.amount))?;

        // Échelle 10^8 -> u128
        let scaled = (dec * Decimal::from(100_000_000u128))
            .to_u128()
            .ok_or_else(|| anyhow::anyhow!("overflow amount: {}", u.amount))?;

        sum = sum.checked_add(scaled)
            .ok_or_else(|| anyhow::anyhow!("overflow in sum"))?;

        picked.push(u.clone());

        if sum >= need {
            return Ok((picked, sum - need)); // change = somme - besoin
        }
    }

    anyhow::bail!("fonds insuffisants");
}

pub fn build_tx(
    inputs: Vec<Utxo>,
    to_addr: String,
    amount_str: String,
    fee_str: String,
    change_addr: String,
) -> anyhow::Result<Transaction> {
    let want = amt_to_u128(amt_parse(&amount_str)?)?;
    let fee  = amt_to_u128(amt_parse(&fee_str)?)?;
    let need = want.checked_add(fee).ok_or_else(|| anyhow::anyhow!("overflow"))?;

    let (picked, change_u) = select_utxos(inputs, need)?;
    let mut tx_inputs = Vec::new();
    for u in &picked {
        tx_inputs.push(TxInput { out: OutputId { txid: u.txid.clone(), index: u.vout } });
    }

    let mut tx_outputs = vec![ TxOutput { address: to_addr, amount: amount_str } ];
    if change_u > 0 {
        tx_outputs.push(TxOutput {
            address: change_addr,
            amount: u128_to_amt(change_u).normalize().to_string(),
        });
    }

    Ok(Transaction {
        inputs: tx_inputs,
        outputs: tx_outputs,
        fee: fee_str,
        unlocks: Vec::new(), // tu signes après
    })
}