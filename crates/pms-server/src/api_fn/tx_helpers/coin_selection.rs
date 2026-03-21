use pms_interface::NetDagAdapter;
use pms_types::{OutputId, TxOutput};
use rust_decimal::Decimal;
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

    let mut utxo_list: Vec<_> = all_utxos
        .into_iter()
        .filter(|(_, tx_output)| tx_output.asset_id == *asset_id)
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
