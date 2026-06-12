use crate::api::AppState;
use crate::fee_distribution::compute_fee_outputs;
use pms_config::FeeDistributionConfig;
use pms_storage::PutResult;
use pms_types::TxOutput;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;

use super::block_ops::{forge_and_sign_block, persist_and_broadcast};

/// Accumulates a transaction fee in the FeePool for periodic consolidated distribution.
///
/// This replaces per-TX `create_reward_block()` calls to prevent coordinator UTXO
/// proliferation. Instead of creating 1 Reward block per TX (= 1 UTXO per TX for
/// the coordinator), fees are pooled and distributed periodically as a single
/// consolidated Reward block by `spawn_fee_distributor_task`.
///
/// At 2000 TPS with 10s distribution interval, this reduces coordinator UTXO
/// creation from 7200/hour to ~360/hour (20x improvement).
///
/// For custom ledgers (ledger_id != "main"), fees are credited to the ledger owner
/// instead of the coordinator, so the owner receives the node rewards during
/// periodic distribution.
pub async fn accumulate_tx_fee(state: &AppState, fee: Decimal) {
    if fee <= Decimal::ZERO {
        return;
    }

    // For main ledger: credit fees to coordinator (who creates the blocks)
    // For custom ledgers: credit fees to ledger owner (who created/owns the ledger)
    let beneficiary_pk = if state.ledger_id == "main" {
        state.node_wallet.encoded_public_key()
    } else {
        // For custom ledgers, use the owner's public key
        if let Some(ref mgr) = state.ledger_mgr {
            if let Some(instance) = mgr.get(&state.ledger_id) {
                instance
                    .def
                    .owner_pubkey
                    .clone()
                    .unwrap_or_else(|| state.node_wallet.encoded_public_key())
            } else {
                // Ledger not found (shouldn't happen), fallback to coordinator
                state.node_wallet.encoded_public_key()
            }
        } else {
            // No ledger manager (shouldn't happen), fallback to coordinator
            state.node_wallet.encoded_public_key()
        }
    };

    {
        let mut pool = state.fee_pool.write().await;
        pool.add_fee(fee, &beneficiary_pk);
    }
    {
        let mut registry = state.node_registry.write().await;
        registry.increment_block_count(&beneficiary_pk);
    }
}

/// Create a reward block distributing fees to coordinator + treasury.
/// Returns the reward block ID if created, None if skipped.
///
/// **DEPRECATED**: Use `accumulate_tx_fee()` + periodic fee distribution instead.
/// Per-TX reward blocks cause coordinator UTXO proliferation at high TPS.
/// Kept for backward compatibility and edge cases where immediate distribution
/// is required.
#[allow(dead_code)]
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

    let eff = &state.effective_fees;
    let fee_config = eff.fee_distribution.clone().unwrap_or_else(|| {
        FeeDistributionConfig::new(
            eff.coordinator_fee_percent as u16 * 100,
            eff.treasury_fee_percent as u16 * 100,
        )
    });

    let coordinator_address = node_wallet.get_address("8e");

    let treasury_addrs: Vec<String> = if !state.treasury_wallets.is_empty() {
        state.treasury_wallets.list.clone()
    } else if !settings.fees.treasury_addresses.is_empty() {
        settings.fees.treasury_addresses.clone()
    } else {
        settings.admin.wallet_addresses.clone()
    };

    let fee_outputs_raw =
        compute_fee_outputs(fee_dec, &treasury_addrs, &coordinator_address, &fee_config);

    if fee_outputs_raw.is_empty() {
        return None;
    }

    let fee_txouts: Vec<TxOutput> = fee_outputs_raw
        .iter()
        .map(|fo| TxOutput::new(fo.address.clone(), fo.amount.clone(), None))
        .collect();

    let reward_payload = PlainPayload::Reward {
        fee_outputs: fee_txouts,
        reward_outputs: vec![],
        burned: "0".to_string(),
        tx_block_id: parent_block_id.to_string(),
    };

    let adapter = state.srv.adapter_arc();
    let wb = match forge_and_sign_block(
        Some(PayloadEnvelope::Plain(reward_payload)),
        vec![parent_block_id.to_string()],
        &adapter,
        node_wallet,
        settings,
        Some("Reward distribution"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            tracing::warn!(
                ledger = %state.ledger_id,
                fee = %fee_dec,
                "Reward block forge failed: {e}"
            );
            return None;
        }
    };

    match persist_and_broadcast(state, &wb).await {
        Ok(PutResult::Inserted) => {
            // NOTE: No add_utxo here — PlainPayload::Reward is a plain payload,
            // so persist_block() already constructs the UtxoDelta and applies it
            // via apply_diff(). Calling add_utxo again would double-count supply
            // AND destroy the address index (LRU re-insert evicts existing entry).
            Some(wb.id)
        }
        Ok(other) => {
            tracing::warn!(
                ledger = %state.ledger_id,
                block_id = %wb.id,
                result = ?other,
                "Reward block not inserted"
            );
            None
        }
        Err(e) => {
            tracing::error!(
                ledger = %state.ledger_id,
                block_id = %wb.id,
                "Reward block persist error: {e}"
            );
            None
        }
    }
}
