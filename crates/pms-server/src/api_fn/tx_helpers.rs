use crate::api::AppState;
use crate::fee_distribution::{FeeDistributionConfig, compute_fee_outputs};
use pms_config::{RuntimeConfig, Settings};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::{ConfigStorage, DagStorage, PutResult};
use pms_token::FeePolicy;
use pms_types::{Block, OutputId, TxInput, TxOutput};
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_utils::{check_pow_leading_zero_bits, compute_block_id};
use pms_wallet::SignerBackend;
use pms_wallet::Wallet;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};
use rust_decimal::Decimal;
use std::sync::Arc;

/// Load fee policy from runtime config in store.
/// Returns (fee_policy, fee_ratio_decimal).
pub fn load_fee_policy(store: &Arc<RocksStore>) -> (FeePolicy, Decimal) {
    let runtime_config = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    let ratio_dec = Decimal::from(runtime_config.fee_rate_bps) / Decimal::from(10000);
    let fee_policy = FeePolicy::new(&runtime_config.base_fee, &ratio_dec.to_string());
    (fee_policy, ratio_dec)
}

/// Select parent blocks for a new block.
/// Uses top_tips, falls back to all_block_ids, enforces single_writer.
pub async fn get_block_parents(
    store: &Arc<RocksStore>,
    settings: &Settings,
) -> Result<Vec<String>, String> {
    let mut parents = match store.top_tips(2).await {
        Ok(tips) if !tips.is_empty() => tips,
        _ => match store.all_block_ids().await {
            Ok(ids) if !ids.is_empty() => vec![ids[0].clone()],
            _ => return Err("no parents available (empty DAG)".into()),
        },
    };

    if settings.validation.enforce_single_writer {
        parents.truncate(1);
    } else if parents.len() < 2 {
        let genesis_id = Block::genesis(compute_block_id).id;
        if !parents.contains(&genesis_id) {
            parents.push(genesis_id);
        }
    }

    Ok(parents)
}

/// Build a block from a payload, mine PoW, serialize to WireBlock, and sign.
pub async fn forge_and_sign_block(
    payload: Option<PayloadEnvelope>,
    parents: Vec<String>,
    adapter: &Arc<dyn NetDagAdapter>,
    node_wallet: &Arc<Wallet>,
    settings: &Settings,
    description: Option<&str>,
) -> Result<WireBlock, String> {
    let meta = WireMeta::from(settings);

    let mut block_metadata = pms_types_block::BlockMetadata {
        signer_x25519_hex: Some(node_wallet.x25519_pub_hex().to_string()),
        ..Default::default()
    };
    if let Some(desc) = description {
        block_metadata.description = Some(desc.to_string());
    }

    let mut block = Block {
        id: String::new(),
        parents,
        payload,
        nonce: 0,
        metadata: Some(block_metadata),
        signer_pk: None,
        signature: None,
    };
    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

    // PoW mining
    let min_bits = adapter.min_pow_leading_zero_bits();
    if min_bits > 0 {
        loop {
            if check_pow_leading_zero_bits(&block.id, min_bits) {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
            if block.nonce == u64::MAX {
                return Err("mining failed".into());
            }
        }
    }

    // Serialize payload
    let payload_json = match &block.payload {
        None => None,
        Some(env) => Some(
            serde_json::to_string(env).map_err(|e| format!("payload serialize: {e:#}"))?,
        ),
    };

    // Create WireBlock + sign
    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json,
        nonce: block.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = node_wallet
        .sign(&msg)
        .map_err(|e| format!("block sign error: {e:?}"))?;

    Ok(wb)
}

/// Persist a block, increment metrics, and broadcast.
/// Returns the PutResult from the adapter.
pub async fn persist_and_broadcast(
    state: &AppState,
    wb: &WireBlock,
) -> Result<PutResult, String> {
    let adapter = state.srv.adapter_arc();
    let res = adapter
        .persist_block(wb)
        .await
        .map_err(|e| format!("persist error: {e:#}"))?;

    if matches!(res, PutResult::Inserted) {
        crate::metrics::BLOCKS_PERSISTED
            .with_label_values(&[&state.ledger_id])
            .inc();
        crate::metrics::PMS_BLOCKS_TOTAL
            .with_label_values(&[&state.ledger_id])
            .inc();
        let _ = state.srv.enqueue_broadcast(wb.id.clone()).await;
    }

    Ok(res)
}

/// Apply UTXO delta: remove spent inputs and add new outputs.
pub async fn apply_utxo_delta(
    adapter: &Arc<dyn NetDagAdapter>,
    block_id: &str,
    inputs: &[TxInput],
    outputs: &[TxOutput],
) {
    for input in inputs {
        let output_id = OutputId {
            txid: input.out.txid.clone(),
            index: input.out.index,
        };
        adapter.remove_utxo(&output_id).await;
    }
    for (idx, output) in outputs.iter().enumerate() {
        adapter
            .add_utxo(
                block_id.to_string(),
                idx as u32,
                output.address.clone(),
                output.amount.clone(),
                output.asset_id.clone(),
            )
            .await;
    }
}

/// Largest-first coin selection.
/// Returns (selected_utxos, total_selected_amount).
pub async fn select_utxos(
    adapter: &Arc<dyn NetDagAdapter>,
    address: &str,
    target: Decimal,
    asset_id: &Option<String>,
) -> Result<(Vec<(OutputId, TxOutput, Decimal)>, Decimal), String> {
    let all_utxos = adapter.utxos_by_address(address).await;

    let utxos: Vec<_> = all_utxos
        .into_iter()
        .filter(|(_, tx_output)| tx_output.asset_id == *asset_id)
        .collect();

    if utxos.is_empty() {
        return Err(format!("no UTXOs found for address {}", address));
    }

    let mut utxo_list: Vec<_> = utxos
        .into_iter()
        .filter_map(|(output_id, tx_output)| {
            Decimal::from_str_exact(&tx_output.amount)
                .ok()
                .map(|amt| (output_id, tx_output, amt))
        })
        .collect();
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

/// Create a reward block distributing fees to coordinator + treasury.
/// Returns the reward block ID if created, None if skipped.
pub async fn create_reward_block(
    state: &AppState,
    fee_dec: Decimal,
    parent_block_id: &str,
) -> Option<String> {
    let settings = &*state.settings;
    let node_wallet = &state.node_wallet;

    // Check if we're the coordinator
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true
    };

    if !is_coordinator || fee_dec <= Decimal::ZERO {
        return None;
    }

    let fee_config = FeeDistributionConfig::new(
        settings.fees.coordinator_fee_percent,
        settings.fees.treasury_fee_percent,
    );

    let coordinator_address = node_wallet.get_address("8e");

    let treasury_addrs: Vec<String> = if !state.treasury_wallets.is_empty() {
        state.treasury_wallets.list.clone()
    } else if !settings.fees.treasury_addresses.is_empty() {
        settings.fees.treasury_addresses.clone()
    } else {
        settings.admin.wallet_addresses.clone()
    };

    let fee_outputs_raw = compute_fee_outputs(fee_dec, &treasury_addrs, &coordinator_address, &fee_config);

    if fee_outputs_raw.is_empty() {
        return None;
    }

    let fee_txouts: Vec<TxOutput> = fee_outputs_raw
        .iter()
        .map(|fo| TxOutput {
            address: fo.address.clone(),
            amount: fo.amount.clone(),
            asset_id: None,
        })
        .collect();

    let reward_payload = PlainPayload::Reward {
        fee_outputs: fee_txouts,
        reward_outputs: vec![],
        burned: "0".to_string(),
        tx_block_id: parent_block_id.to_string(),
    };

    let adapter = state.srv.adapter_arc();
    let wb = forge_and_sign_block(
        Some(PayloadEnvelope::Plain(reward_payload)),
        vec![parent_block_id.to_string()],
        &adapter,
        node_wallet,
        settings,
        Some("Reward distribution"),
    )
    .await
    .ok()?;

    if let Ok(PutResult::Inserted) = persist_and_broadcast(state, &wb).await {
        Some(wb.id)
    } else {
        None
    }
}
