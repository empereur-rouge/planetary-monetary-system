use crate::address_candidates;
use anyhow::{Result, bail};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct UtxoDec {
    pub txid: String,
    pub index: u32,
    pub amount: Decimal, // amount en décimal
}

pub async fn gather_wallet_utxos_dec(
    store: &RocksStore,
    wallet_pub_hex: &str,
    wallet_x25519_pub_hex: &str,
    wallet_x25519_sk_hex: &str,
    hrp: &str,
    scan_limit: usize,
) -> anyhow::Result<Vec<UtxoDec>> {
    let candidates = address_candidates(hrp, wallet_pub_hex, wallet_x25519_pub_hex);

    // Si scan_limit est grand (>500), on suppose une demande d'historique profond.
    // Comme `by_time` est taillé par `tip_limit` (ex: 256), `recent_ids` ne suffit pas.
    // On bascule sur un scan complet (coûteux mais exhaustif).
    let ids = if scan_limit > 500 {
        store.all_block_ids().await?
    } else {
        store.recent_ids(scan_limit).await?
    };
    let blocks = store.get_blocks_by_ids(&ids).await?;

    let mut earned: HashMap<(String, u32), Decimal> = HashMap::new();
    let mut spent: HashSet<(String, u32)> = HashSet::new();

    for b in blocks {
        let Some(s) = &b.payload_json else {
            continue;
        };
        let Ok(env) = serde_json::from_str::<PayloadEnvelope>(s) else {
            continue;
        };
        let p = match env {
            PayloadEnvelope::Encrypted(enc) => match enc.decrypt_as_payload(wallet_x25519_sk_hex) {
                Ok(pp) => pp,
                Err(_) => continue,
            },
            PayloadEnvelope::Plain(pp) => pp,
        };

        match p {
            PlainPayload::Mint { outputs } => {
                for (i, o) in outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        // parse Decimal sans panic
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            PlainPayload::TxUtxo(tx) => {
                // Track spent inputs
                for inp in &tx.inputs {
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
                // Track earned outputs matching wallet address
                for (i, o) in tx.outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            PlainPayload::BridgeLock { inputs, .. } => {
                // Track spent inputs (funds leaving this ledger)
                for inp in inputs {
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
            }
            PlainPayload::BridgeMint { outputs, .. } => {
                // Track earned outputs (funds arriving on this ledger)
                for (i, o) in outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            PlainPayload::Seize {
                inputs, outputs, ..
            }
            | PlainPayload::Reverse {
                inputs, outputs, ..
            } => {
                for inp in inputs {
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
                for (i, o) in outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            _other => {
                tracing::trace!("compute_utxos_for_address: skipped variant (no UTXO impact)");
            }
        }
    }

    // UTXO = earned - spent
    let mut utxos = Vec::new();
    for ((txid, index), amt) in earned {
        if !spent.contains(&(txid.clone(), index)) {
            utxos.push(UtxoDec {
                txid,
                index,
                amount: amt,
            });
        }
    }
    // tri du +gros au +petit (utile pour select_utxos_dec glouton)
    utxos.sort_by(|a, b| b.amount.cmp(&a.amount));
    Ok(utxos)
}

pub fn select_utxos_dec(mut utxos: Vec<UtxoDec>, need: Decimal) -> Result<(Vec<UtxoDec>, Decimal)> {
    // tri décroissant (gros -> petit)
    utxos.sort_by(|a, b| b.amount.cmp(&a.amount));

    let mut picked = Vec::new();
    let mut sum = Decimal::ZERO;

    for u in utxos {
        sum += u.amount;
        picked.push(u);
        if sum >= need {
            return Ok((picked, sum - need)); // change
        }
    }
    bail!("fonds insuffisants");
}

pub async fn gather_address_utxos_dec(
    store: &RocksStore,
    _hrp: &str,
    address: &str,
    scan_limit: usize,
) -> anyhow::Result<Vec<UtxoDec>> {
    // variantes d’adresse si tu en as (bech32m/hex/etc)
    let candidates = vec![address.to_string()];

    // Même logique deep scan
    let ids = if scan_limit > 500 {
        store.all_block_ids().await?
    } else {
        store.recent_ids(scan_limit).await?
    };
    let blocks = store.get_blocks_by_ids(&ids).await?;

    let mut earned: HashMap<(String, u32), Decimal> = HashMap::new();
    let mut spent: HashSet<(String, u32)> = HashSet::new();

    for b in blocks {
        let Some(s) = &b.payload_json else {
            continue;
        };
        let Ok(env) = serde_json::from_str::<PayloadEnvelope>(s) else {
            continue;
        };

        // IMPORTANT: address-only => on ignore l’encrypted
        let PayloadEnvelope::Plain(p) = env else {
            continue;
        };

        match p {
            PlainPayload::Mint { outputs } => {
                for (i, o) in outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            PlainPayload::TxUtxo(tx) => {
                for inp in &tx.inputs {
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
                for (i, o) in tx.outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            PlainPayload::BridgeLock { inputs, .. } => {
                // Track spent inputs (funds leaving this ledger)
                for inp in inputs {
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
            }
            PlainPayload::BridgeMint { outputs, .. } => {
                // Track earned outputs (funds arriving on this ledger)
                for (i, o) in outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            PlainPayload::Seize {
                inputs, outputs, ..
            }
            | PlainPayload::Reverse {
                inputs, outputs, ..
            } => {
                for inp in &inputs {
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
                for (i, o) in outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            _other => {
                tracing::trace!("compute_all_utxos: skipped variant (no UTXO impact)");
            }
        }
    }

    let mut utxos = Vec::new();
    for ((txid, index), amt) in earned {
        if !spent.contains(&(txid.clone(), index)) {
            utxos.push(UtxoDec {
                txid,
                index,
                amount: amt,
            });
        }
    }

    utxos.sort_by(|a, b| b.amount.cmp(&a.amount));
    Ok(utxos)
}
