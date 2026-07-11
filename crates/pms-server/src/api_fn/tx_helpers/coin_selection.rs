use pms_core::utxo::current_time_ms;
use pms_interface::NetDagAdapter;
use pms_types::{OutputId, TxOutput};
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::Arc;

/// Coin selection with O(1)-ish fast path for large UTXO sets.
///
/// **Fast path**: Collects at most 256 UTXOs via `utxos_for_selection()` which
/// exits early from the address index. For coordinator addresses with millions
/// of fee reward UTXOs, this avoids cloning/sorting the entire set.
///
/// **Slow fallback**: If 256 UTXOs don't cover `target`, falls back to full
/// scan with largest-first sort (original O(N log N) algorithm).
///
/// Returns (selected_utxos, total_selected_amount).
pub async fn select_utxos(
    adapter: &Arc<dyn NetDagAdapter>,
    address: &str,
    target: Decimal,
    asset_id: &Option<String>,
) -> Result<(Vec<(OutputId, TxOutput, Decimal)>, Decimal), String> {
    // Fast path: collect at most 256 UTXOs with early exit.
    // For coordinator fee UTXOs (~0.065 PMS each), 256 * 0.065 = 16.64 PMS.
    // For normal wallets with few large UTXOs, 256 is more than enough.
    const FAST_LIMIT: usize = 256;

    let (fast_result, fast_total) = adapter
        .utxos_for_selection(address, asset_id, target, FAST_LIMIT)
        .await;

    if fast_result.is_empty() {
        return Err(format!("no UTXOs found for address {}", address));
    }

    // Fast path succeeded -- trim to what we actually need
    if fast_total >= target {
        let mut selected = Vec::new();
        let mut sum = Decimal::ZERO;
        for item in fast_result {
            if sum >= target {
                break;
            }
            sum += item.2;
            selected.push(item);
        }
        return Ok((selected, sum));
    }

    // Slow fallback: 256 UTXOs weren't enough (rare -- very small UTXOs or
    // very large target). Full scan with largest-first sort.
    let all_utxos = adapter.utxos_by_address(address).await;

    // Exclut les UTXOs time-lockés encore verrouillés — parité avec le fast path
    // (`utxos_by_address_for_selection`) et `select_utxos_multi` : sans ce filtre,
    // un UTXO verrouillé pouvait être sélectionné ici puis rejeté au consensus
    // (`OutputTimeLocked`), un échec de liveness évitable.
    let now_ms = current_time_ms();
    let mut utxo_list: Vec<_> = all_utxos
        .into_iter()
        .filter(|(_, tx_output)| tx_output.asset_id == *asset_id)
        .filter(|(_, tx_output)| tx_output.locked_until.is_none_or(|l| l <= now_ms))
        .filter_map(|(output_id, tx_output)| {
            Decimal::from_str_exact(&tx_output.amount)
                .ok()
                .map(|amt| (output_id, tx_output, amt))
        })
        .collect();

    if utxo_list.is_empty() {
        return Err(format!("no UTXOs found for address {}", address));
    }

    utxo_list.sort_by(|a, b| b.2.cmp(&a.2));

    let mut selected: Vec<(OutputId, TxOutput, Decimal)> = Vec::new();
    let mut selected_sum = Decimal::ZERO;
    for (output_id, tx_output, amt) in utxo_list {
        if selected_sum >= target {
            break;
        }
        selected_sum += amt;
        selected.push((output_id, tx_output, amt));
    }

    if selected_sum < target {
        return Err(format!(
            "insufficient balance: available={}, required={}",
            selected_sum, target
        ));
    }

    Ok((selected, selected_sum))
}

/// Coin selection unifiée sur **plusieurs formes d'adresse** d'un même
/// propriétaire.
///
/// Un wallet à clé unique possède plusieurs encodages d'adresse équivalents qui
/// peuvent chacun indexer des UTXOs (cf. [`pms_wallet::spend_address_forms`]) :
/// la forme **bech32m** canonique (dérivée par le nœud dans les chemins de
/// dépense) et la **pubkey secp hex brute** (l'`address` historique du SDK).
/// Comme l'index d'adresse est keyé par le string-propriétaire exact écrit au
/// mint, des fonds mintés vers une forme sont invisibles à un chemin qui dérive
/// l'autre → fonds « piégés ». Ce helper unit les formes pour qu'un wallet
/// dépense toujours ce qu'il possède.
///
/// **Fast-path** : la 1re forme candidate (bech32m canonique) est essayée seule
/// via [`select_utxos`], préservant l'early-exit O(1)-ish du cas commun. Le
/// balayage-union (plus lent, full-scan largest-first) sur les formes restantes
/// ne tourne QUE si la forme canonique ne couvre pas `target` — un chemin de
/// récupération rare. Les UTXOs time-lockés encore verrouillés sont exclus
/// (le validateur hot-path les rejetterait — `OutputTimeLocked`).
///
/// `addresses` doit être ordonné canonique-d'abord ; passer une seule forme
/// équivaut à [`select_utxos`]. Renvoie `(utxos_sélectionnés, somme)`.
pub async fn select_utxos_multi(
    adapter: &Arc<dyn NetDagAdapter>,
    addresses: &[String],
    target: Decimal,
    asset_id: &Option<String>,
) -> Result<(Vec<(OutputId, TxOutput, Decimal)>, Decimal), String> {
    let Some(primary) = addresses.first() else {
        return Err("no candidate addresses provided".to_string());
    };

    // Fast-path : la forme canonique couvre la cible à elle seule (cas commun,
    // perf et comportement strictement identiques à select_utxos).
    if let Ok(res) = select_utxos(adapter, primary, target, asset_id).await {
        return Ok(res);
    }

    // Chemin de récupération (rare) : union des UTXOs sur toutes les formes.
    // Une seule forme n'ayant pas couvert la cible, on agrège largest-first.
    let now_ms = current_time_ms();
    let mut seen: HashSet<OutputId> = HashSet::new();
    let mut pool: Vec<(OutputId, TxOutput, Decimal)> = Vec::new();

    for addr in addresses {
        for (oid, txo) in adapter.utxos_by_address(addr).await {
            // Filtre asset (None==None pour PMS natif) puis time-lock.
            if txo.asset_id != *asset_id {
                continue;
            }
            if txo.locked_until.is_some_and(|l| l > now_ms) {
                continue;
            }
            if let Ok(amt) = Decimal::from_str_exact(&txo.amount) {
                // Dédup par OutputId : une même forme ne peut pas ré-indexer,
                // mais on se protège d'un chevauchement inattendu entre formes.
                if seen.insert(oid.clone()) {
                    pool.push((oid, txo, amt));
                }
            }
        }
    }

    if pool.is_empty() {
        return Err(format!(
            "no UTXOs found across {} address form(s)",
            addresses.len()
        ));
    }

    // Largest-first : minimise le nombre d'inputs (frais / taille de bloc).
    pool.sort_by(|a, b| b.2.cmp(&a.2));

    let mut selected: Vec<(OutputId, TxOutput, Decimal)> = Vec::new();
    let mut selected_sum = Decimal::ZERO;
    for item in pool {
        if selected_sum >= target {
            break;
        }
        selected_sum += item.2;
        selected.push(item);
    }

    if selected_sum < target {
        return Err(format!(
            "insufficient balance across address forms: available={}, required={}",
            selected_sum, target
        ));
    }

    Ok((selected, selected_sum))
}
