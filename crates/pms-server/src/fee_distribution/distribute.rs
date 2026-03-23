use crate::api::AppState;
use anyhow::Result;
use pms_economics::fee_burn::calculate_fee_burn;
use pms_storage::PutResult;
use pms_types::TxOutput;
use pms_types_block::Block;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_utils::check_pow::check_pow_leading_zero_bits;
use pms_utils::compute_block_id;
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireBlock;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributeFeesResult {
    pub success: bool,
    pub reward_block_id: Option<String>,
    pub total_distributed: String,
    pub num_recipients: usize,
}

/// Executes the distribution of accumulated fees and refunds from the pool.
/// Creates a Mint block containing UTXOs for the recipients.
pub async fn perform_fee_distribution(
    state: &AppState,
    parent_id: Option<String>,
) -> Result<DistributeFeesResult> {
    let settings = &state.settings;
    let node_wallet = &state.node_wallet;

    // Only Coordinator can distribute
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true // Dev mode
    };

    if !is_coordinator {
        // Not authorized, but we return success=false instead of logging error essentially
        return Ok(DistributeFeesResult {
            success: false,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        });
    }

    // 0. RESOLVE PARENT
    let parent_id = match parent_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            // Fetch current tip from DAG
            match state.srv.adapter_arc().top_tips(1).await {
                Ok(tips) if !tips.is_empty() => tips[0].clone(),
                Ok(_empty) => {
                    let pool_total = state.fee_pool.read().await.total_fees;
                    tracing::error!(
                        pool_total = %pool_total,
                        "Fee distribution BLOCKED: top_tips() returned empty. \
                         DAG may have been over-pruned. Fees are accumulating."
                    );
                    return Ok(DistributeFeesResult {
                        success: false,
                        reward_block_id: None,
                        total_distributed: "0".to_string(),
                        num_recipients: 0,
                    });
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Fee distribution BLOCKED: top_tips() returned error"
                    );
                    return Ok(DistributeFeesResult {
                        success: false,
                        reward_block_id: None,
                        total_distributed: "0".to_string(),
                        num_recipients: 0,
                    });
                }
            }
        }
    };

    // 1. ATOMIC SWAP: replace pool with fresh instance, work on the snapshot.
    //    This eliminates the race condition where fees accumulated between
    //    snapshot and reset were permanently lost. Single write lock (~1us).
    let snapshot = {
        let mut pool = state.fee_pool.write().await;
        if !pool.has_fees() {
            return Ok(DistributeFeesResult {
                success: true, // "Success" because nothing to do
                reward_block_id: None,
                total_distributed: "0".to_string(),
                num_recipients: 0,
            });
        }
        std::mem::replace(&mut *pool, crate::fee_pool::FeePool::new())
    };
    // Pool is now fresh -- new fees from concurrent TX go into the new pool.
    // We work exclusively on the snapshot below.
    let total_node_fees = snapshot.total_fees;
    let shares = snapshot.calculate_shares();
    let burn_refunds = snapshot.get_burn_refunds();
    let _total_burn_refunds = snapshot.total_burn_refunds();

    // 1b. FEE BURN -- remove burned portion from distributable fees
    let burn_rate_bps =
        crate::api_fn::tx_helpers::load_burn_rate_bps(&state.store, Some(&state.effective_fees));
    let burn_result = calculate_fee_burn(total_node_fees, burn_rate_bps);
    let fee_burned = burn_result.burned;
    let total_node_fees = burn_result.distributable; // shadow with distributable amount

    if fee_burned > Decimal::ZERO {
        tracing::info!(
            "🔥 Fee burn: {} PMS burned ({}bps), {} PMS distributable",
            fee_burned,
            burn_rate_bps,
            total_node_fees
        );
        // Track cumulative burn in RocksDB
        if let Err(e) = state.store.increment_total_burned(fee_burned) {
            tracing::error!("Failed to persist total_burned: {}", e);
        }
    }

    // 2. BUILD OUTPUTS
    let coordinator_x25519 = state.node_wallet.x25519_pub_hex().to_string();
    let mut all_outputs: Vec<TxOutput> = Vec::new();
    let mut total_distributed = Decimal::ZERO;

    // 2a. BURN REFUNDS (multi-asset: PMS native + custom tokens like Edenite)
    for (wallet_address, asset_id, amount) in &burn_refunds {
        if *amount <= Decimal::ZERO {
            continue;
        }
        all_outputs.push(TxOutput {
            address: wallet_address.clone(),
            amount: amount.to_string(),
            asset_id: asset_id.clone(),
        });
        // Only count PMS-native refunds towards total_distributed (for stats)
        if asset_id.is_none() {
            total_distributed += *amount;
        }
        let asset_label = asset_id.as_deref().unwrap_or("PMS");
        tracing::info!(
            "💰 Burn refund output: {} -> {} {}",
            &wallet_address[..20.min(wallet_address.len())],
            amount,
            asset_label,
        );
    }

    // 2b. TREASURY TAX (First cut)
    let treasury_percent = Decimal::from(settings.fees.treasury_fee_percent);
    let mut node_pool_amount = total_node_fees;

    if treasury_percent > Decimal::ZERO && total_node_fees > Decimal::ZERO {
        let treasury_cut = (total_node_fees * treasury_percent / Decimal::from(100)).round_dp(8);
        if treasury_cut > Decimal::ZERO {
            // Get treasury wallets: prefer loaded file, fallback to config
            let treasury_wallets = if !state.treasury_wallets.is_empty() {
                &state.treasury_wallets.list
            } else {
                &settings.fees.treasury_addresses
            };

            if let Some(target) = treasury_wallets.first() {
                all_outputs.push(TxOutput {
                    address: target.clone(),
                    amount: treasury_cut.to_string(),
                    asset_id: None,
                });
                total_distributed += treasury_cut;
                node_pool_amount -= treasury_cut;
                tracing::info!(
                    "🏛️ Treasury Tax ({}%): {} PMS -> {}",
                    settings.fees.treasury_fee_percent,
                    treasury_cut,
                    &target[..20.min(target.len())]
                );
            } else {
                // [FALLBACK SECURITE] Pas de treasury wallet -> On laisse les fonds dans le pool pour les Noeuds/Createur
                // On ne deduit PAS `treasury_cut` de `node_pool_amount`.
                tracing::warn!(
                    "⚠️ Treasury tax enabled but no treasury addresses configured! Keeping {} PMS in node pool distribution (fallback to nodes).",
                    treasury_cut
                );
            }
        }
    }

    // 2c. NODE FEES (Remaining amount distributed by share)
    if node_pool_amount > Decimal::ZERO {
        let registry = state.node_registry.read().await;
        let nodes = registry.get_active_nodes();

        for (node_pk, share_pct, _original_share_amount) in &shares {
            // Recalculate share amount based on remaining pool
            let share_amount = (node_pool_amount * *share_pct).round_dp(8);

            if share_amount <= Decimal::ZERO {
                continue;
            }

            // Find node info to get wallet address
            let node_info = nodes.iter().find(|n| &n.node_pk == node_pk);
            let mut target_address = None;

            if let Some(node) = node_info {
                if let Some(addr) = &node.wallet_address {
                    target_address = Some(addr.clone());
                } else {
                    tracing::warn!(
                        "⚠️ Node {} has no registered wallet address!",
                        &node_pk[..10]
                    );
                }
            } else {
                tracing::warn!(
                    "⚠️ Node {} disappeared from registry during distribution!",
                    &node_pk[..10]
                );
            }

            // If no target address found (node missing or no wallet), fallback to Treasury
            if target_address.is_none() {
                // Determine fallback treasury address (same logic as tax)
                let fallback = if !state.treasury_wallets.is_empty() {
                    state.treasury_wallets.list.first().cloned()
                } else {
                    settings.fees.treasury_addresses.first().cloned()
                };

                if let Some(addr) = fallback {
                    tracing::warn!(
                        "⚠️ Redirecting {} PMS for node {} to Treasury (fallback)",
                        share_amount,
                        &node_pk[..10]
                    );
                    target_address = Some(addr);
                }
            }

            if let Some(addr) = target_address {
                all_outputs.push(TxOutput {
                    address: addr.clone(),
                    amount: share_amount.to_string(),
                    asset_id: None,
                });
                total_distributed += share_amount;
                tracing::info!(
                    "👷 Node Reward: {} PMS -> {} (Node: {})",
                    share_amount,
                    &addr[..20.min(addr.len())],
                    &node_pk[..10]
                );
            } else {
                tracing::error!(
                    "❌ FAILED to distribute {} PMS for Node {}: No wallet & No Treasury fallback!",
                    share_amount,
                    &node_pk[..10]
                );
            }
        }
    }

    if all_outputs.is_empty() {
        return Ok(DistributeFeesResult {
            success: true,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        });
    }

    let num_recipients = all_outputs.len();

    // 3. CREATE MINT BLOCK
    let mint_payload = PlainPayload::Mint {
        outputs: all_outputs.clone(),
    };

    let mut reward_block = Block {
        id: String::new(),
        parents: vec![parent_id.clone()],
        payload: Some(PayloadEnvelope::Plain(mint_payload)),
        nonce: 0,
        metadata: Some(pms_types_block::BlockMetadata {
            signer_x25519_hex: Some(coordinator_x25519.clone()),
            description: Some(format!(
                "Fees/Refunds: {} PMS to {} wallets",
                total_distributed, num_recipients
            )),
            ..Default::default()
        }),
        signer_pk: None,
        signature: None,
    };
    reward_block.id = compute_block_id(
        &reward_block.parents,
        &reward_block.payload,
        reward_block.nonce,
    );

    // PoW
    let min_bits = state.srv.adapter_arc().min_pow_leading_zero_bits();
    if min_bits > 0 {
        while !check_pow_leading_zero_bits(&reward_block.id, min_bits) {
            reward_block.nonce += 1;
            reward_block.id = compute_block_id(
                &reward_block.parents,
                &reward_block.payload,
                reward_block.nonce,
            );
        }
    }

    // Build WireBlock
    let reward_payload_json = serde_json::to_string(&reward_block.payload)?;
    let mut reward_wb = WireBlock {
        id: reward_block.id.clone(),
        parents: reward_block.parents.clone(),
        payload_json: Some(reward_payload_json),
        nonce: reward_block.nonce,
        network_id: state.settings.network.network_id.clone(),
        protocol_version: state.settings.network.protocol_version as u16,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: reward_block.metadata.clone(),
    };

    // Sign
    let reward_msg = canonical_wireblock_message(&reward_wb);
    reward_wb.signature_hex = node_wallet.sign(&reward_msg)?;

    // 4. PERSIST
    match state.srv.adapter_arc().persist_block(&reward_wb).await {
        Ok(PutResult::Inserted) => {
            crate::metrics::BLOCKS_PERSISTED
                .with_label_values(&[&state.ledger_id])
                .inc();
            let _ = state.srv.enqueue_broadcast(reward_wb.id.clone()).await;

            // NOTE: No add_utxo here — PlainPayload::Mint is a plain payload,
            // so persist_block() already constructs the UtxoDelta and applies it
            // via apply_diff(). Calling add_utxo again would double-count supply
            // AND destroy the address index (LRU re-insert evicts existing entry).

            // Pool was already swapped atomically in step 1 -- no reset needed.

            tracing::info!(
                "📦 Automated fees distributed: {} PMS to {} wallets (block: {})",
                total_distributed,
                num_recipients,
                &reward_wb.id[..16]
            );

            Ok(DistributeFeesResult {
                success: true,
                reward_block_id: Some(reward_wb.id),
                total_distributed: total_distributed.to_string(),
                num_recipients,
            })
        }
        Ok(PutResult::AlreadyExists) => {
            // Restore fees -- persist didn't happen, fees would be lost
            {
                let mut pool = state.fee_pool.write().await;
                pool.merge_from(&snapshot);
            }
            anyhow::bail!("Reward block already exists")
        }
        Ok(PutResult::Rejected(r)) => {
            // Restore fees -- persist was rejected, fees would be lost
            {
                let mut pool = state.fee_pool.write().await;
                pool.merge_from(&snapshot);
            }
            anyhow::bail!("Reward block rejected: {}", r)
        }
        Err(e) => {
            // Restore fees -- persist errored, fees would be lost
            {
                let mut pool = state.fee_pool.write().await;
                pool.merge_from(&snapshot);
            }
            anyhow::bail!("Storage error: {}", e)
        }
    }
}
