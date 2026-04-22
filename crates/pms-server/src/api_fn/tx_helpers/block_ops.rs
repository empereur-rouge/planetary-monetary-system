use crate::api::AppState;
use pms_config::Settings;
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult};
use pms_types::{Block, OutputId, TxInput, TxOutput};
use pms_types_payload::PayloadEnvelope;
use pms_utils::{check_pow_leading_zero_bits, compute_block_id};
use pms_wallet::SignerBackend;
use pms_wallet::Wallet;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};
use std::sync::Arc;

/// Select parent blocks for a new block.
/// Uses top_tips, falls back to recent_ids(1), enforces single_writer.
pub async fn get_block_parents(
    store: &Arc<pms_storage::rocks_store::store::RocksStore>,
    settings: &Settings,
) -> Result<Vec<String>, String> {
    let mut parents = match store.top_tips(2).await {
        Ok(tips) if !tips.is_empty() => tips,
        // Fallback: pick the most recent block instead of loading ALL IDs
        // (all_block_ids on 13M+ blocks allocates ~1 GB).
        _ => match store.recent_ids(1).await {
            Ok(ids) if !ids.is_empty() => ids,
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
        Some(env) => {
            Some(serde_json::to_string(env).map_err(|e| format!("payload serialize: {e:#}"))?)
        }
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
pub async fn persist_and_broadcast(state: &AppState, wb: &WireBlock) -> Result<PutResult, String> {
    let adapter = state.srv.adapter_arc();
    let res = adapter
        .persist_block(wb)
        .await
        .map_err(|e| format!("persist error: {e:#}"))?;

    after_persist_bookkeeping(state, wb, &res).await;

    Ok(res)
}

/// Persist a block atomically with an externally-provided `UtxoDelta`, then
/// run the same metrics / broadcast bookkeeping as [`persist_and_broadcast`].
///
/// For encrypted payloads, the pipeline can't derive the UTXO delta from
/// the ciphertext, so the caller — which knows the plaintext — hands the
/// delta in here. The adapter applies it inside the same critical section
/// as the block insert, so a block is never visible to the DAG / persist
/// pipeline while its inputs still look spendable in the UTXO set
/// (audit finding H1).
pub async fn persist_and_broadcast_with_delta(
    state: &AppState,
    wb: &WireBlock,
    inputs: &[TxInput],
    outputs: &[TxOutput],
) -> Result<PutResult, String> {
    let adapter = state.srv.adapter_arc();
    let delta = adapter.build_encrypted_utxo_delta(&wb.id, inputs, outputs);
    let res = adapter
        .persist_block_with_delta(wb, delta)
        .await
        .map_err(|e| format!("persist error: {e:#}"))?;

    after_persist_bookkeeping(state, wb, &res).await;

    Ok(res)
}

/// Shared tail of both persist entry points: metrics + gossip + TPS counter.
async fn after_persist_bookkeeping(state: &AppState, wb: &WireBlock, res: &PutResult) {
    if matches!(res, PutResult::Inserted) {
        crate::metrics::BLOCKS_PERSISTED
            .with_label_values(&[&state.ledger_id])
            .inc();
        crate::metrics::PMS_BLOCKS_TOTAL
            .with_label_values(&[&state.ledger_id])
            .inc();
        let _ = state.srv.enqueue_broadcast(wb.id.clone()).await;
        // Record block for TPS tracker (dynamic fee calculation)
        state.tps_tracker.record_block();
    }
}

/// Apply UTXO delta: remove spent inputs and add new outputs.
///
/// **IMPORTANT**: Only use for **encrypted** payloads where `persist_block` cannot
/// see the transaction contents (delta = None). For **plain** payloads (Mint, Reward,
/// Seize, Reverse, etc.), `persist_block` already constructs the UtxoDelta and calls
/// `apply_diff()` -- calling this function would double-count supply.
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
