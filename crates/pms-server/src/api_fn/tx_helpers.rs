use crate::api::AppState;
use crate::fee_distribution::compute_fee_outputs;
use pms_config::{
    FeeDistributionConfig, FeeTier, FeesSettings, LedgerFeesOverride, RuntimeConfig, Settings,
};
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

// ═══════════════════════════════════════════════════════════════════════════
// Per-Ledger Effective Fees
// ═══════════════════════════════════════════════════════════════════════════

/// Resolved fee configuration for a specific ledger context.
/// Merges: global FeesSettings ← LedgerFeesOverride.
/// RuntimeConfig (hot-swap) takes priority at read-time in load_* functions.
#[derive(Debug, Clone)]
pub struct EffectiveFees {
    pub ratio: String,
    pub base_fee: String,
    pub platform_fee_ratio: String,
    pub block_reward: String,
    pub fee_tiers: Vec<FeeTier>,
    pub mint_fee_base: Option<String>,
    pub mint_fee_ratio: Option<String>,
    pub token_creation_fee: Option<String>,
    pub nft_mint_fee: Option<String>,
    pub nft_fee_exempt_types: Vec<String>,
    pub fee_distribution: Option<FeeDistributionConfig>,
    pub treasury_fee_percent: u8,
    pub coordinator_fee_percent: u8,
}

/// Resolve effective fees by merging global FeesSettings with optional per-ledger overrides.
/// Per-ledger values take priority over global when present.
pub fn resolve_effective_fees(
    global: &FeesSettings,
    ledger_override: Option<&LedgerFeesOverride>,
) -> EffectiveFees {
    match ledger_override {
        None => EffectiveFees {
            ratio: global.ratio.clone(),
            base_fee: global.base_fee.clone(),
            platform_fee_ratio: global.platform_fee_ratio.clone(),
            block_reward: global.block_reward.clone(),
            fee_tiers: global.fee_tiers.clone(),
            mint_fee_base: global.mint_fee_base.clone(),
            mint_fee_ratio: global.mint_fee_ratio.clone(),
            token_creation_fee: global.token_creation_fee.clone(),
            nft_mint_fee: global.nft_mint_fee.clone(),
            nft_fee_exempt_types: global.nft_fee_exempt_types.clone(),
            fee_distribution: global.fee_distribution.clone(),
            treasury_fee_percent: global.treasury_fee_percent,
            coordinator_fee_percent: global.coordinator_fee_percent,
        },
        Some(ov) => EffectiveFees {
            ratio: ov.ratio.clone().unwrap_or_else(|| global.ratio.clone()),
            base_fee: ov.base_fee.clone().unwrap_or_else(|| global.base_fee.clone()),
            platform_fee_ratio: ov
                .platform_fee_ratio
                .clone()
                .unwrap_or_else(|| global.platform_fee_ratio.clone()),
            block_reward: ov
                .block_reward
                .clone()
                .unwrap_or_else(|| global.block_reward.clone()),
            fee_tiers: if ov.fee_tiers.is_empty() {
                global.fee_tiers.clone()
            } else {
                ov.fee_tiers.clone()
            },
            mint_fee_base: ov.mint_fee_base.clone().or_else(|| global.mint_fee_base.clone()),
            mint_fee_ratio: ov.mint_fee_ratio.clone().or_else(|| global.mint_fee_ratio.clone()),
            token_creation_fee: ov
                .token_creation_fee
                .clone()
                .or_else(|| global.token_creation_fee.clone()),
            nft_mint_fee: ov.nft_mint_fee.clone().or_else(|| global.nft_mint_fee.clone()),
            nft_fee_exempt_types: if ov.nft_fee_exempt_types.is_empty() {
                global.nft_fee_exempt_types.clone()
            } else {
                ov.nft_fee_exempt_types.clone()
            },
            fee_distribution: ov
                .fee_distribution
                .clone()
                .or_else(|| global.fee_distribution.clone()),
            treasury_fee_percent: ov
                .treasury_fee_percent
                .unwrap_or(global.treasury_fee_percent),
            coordinator_fee_percent: ov
                .coordinator_fee_percent
                .unwrap_or(global.coordinator_fee_percent),
        },
    }
}

/// Load fee policy from runtime config in store.
/// Returns (fee_policy, fee_ratio_decimal).
pub fn load_fee_policy(store: &Arc<RocksStore>) -> (FeePolicy, Decimal) {
    let runtime_config = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    let ratio_dec = Decimal::from(runtime_config.fee_rate_bps) / Decimal::from(10000);
    let fee_policy = if runtime_config.fee_tiers.is_empty() {
        FeePolicy::new(&runtime_config.base_fee, &ratio_dec.to_string())
    } else {
        FeePolicy::tiered(&runtime_config.base_fee, runtime_config.fee_tiers.clone())
    };
    (fee_policy, ratio_dec)
}

/// Load mint fee policy.
/// Priority: RuntimeConfig > EffectiveFees (per-ledger).
/// Returns None if no mint fee is configured.
pub fn load_mint_fee_policy(
    store: &Arc<RocksStore>,
    eff: Option<&EffectiveFees>,
) -> Option<FeePolicy> {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    let base = rc
        .mint_fee_base
        .or_else(|| eff.and_then(|e| e.mint_fee_base.clone()))
        .unwrap_or_default();
    let ratio = rc
        .mint_fee_ratio
        .or_else(|| eff.and_then(|e| e.mint_fee_ratio.clone()))
        .unwrap_or_default();

    if base.is_empty() && ratio.is_empty() {
        return None;
    }

    Some(FeePolicy::new(
        if base.is_empty() { "0" } else { &base },
        if ratio.is_empty() { "0" } else { &ratio },
    ))
}

/// Load token creation fee.
/// Priority: RuntimeConfig > EffectiveFees (per-ledger).
/// Returns None if not configured.
pub fn load_token_creation_fee(
    store: &Arc<RocksStore>,
    eff: Option<&EffectiveFees>,
) -> Option<Decimal> {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());
    rc.token_creation_fee
        .as_ref()
        .or_else(|| eff.and_then(|e| e.token_creation_fee.as_ref()))
        .and_then(|f| Decimal::from_str_exact(f).ok())
        .filter(|d| *d > Decimal::ZERO)
}

/// Load NFT mint fee.
/// Priority: RuntimeConfig > EffectiveFees (per-ledger).
/// Returns None if not configured.
pub fn load_nft_mint_fee(
    store: &Arc<RocksStore>,
    eff: Option<&EffectiveFees>,
) -> Option<Decimal> {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());
    rc.nft_mint_fee
        .as_ref()
        .or_else(|| eff.and_then(|e| e.nft_mint_fee.as_ref()))
        .and_then(|f| Decimal::from_str_exact(f).ok())
        .filter(|d| *d > Decimal::ZERO)
}

/// Check if an NFT type is exempt from mint fees.
/// Checks RuntimeConfig first, falls back to EffectiveFees.
pub fn is_nft_type_fee_exempt(
    store: &Arc<RocksStore>,
    nft_type: Option<&str>,
    eff: Option<&EffectiveFees>,
) -> bool {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    let exempt_types = if !rc.nft_fee_exempt_types.is_empty() {
        &rc.nft_fee_exempt_types
    } else if let Some(e) = eff {
        &e.nft_fee_exempt_types
    } else {
        return false;
    };

    match nft_type {
        Some(t) => exempt_types
            .iter()
            .any(|exempt| exempt.eq_ignore_ascii_case(t)),
        None => false,
    }
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

    let eff = &state.effective_fees;
    let fee_config = eff
        .fee_distribution
        .clone()
        .unwrap_or_else(|| {
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
        // Register fee UTXOs so recipients can spend them
        let adapter = state.srv.adapter_arc();
        for (idx, fo) in fee_outputs_raw.iter().enumerate() {
            adapter
                .add_utxo(
                    wb.id.clone(),
                    idx as u32,
                    fo.address.clone(),
                    fo.amount.clone(),
                    None, // fees always in PMS native
                )
                .await;
        }
        Some(wb.id)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pms_config::{FeePickMode, LedgerFeesOverride};

    /// Helper: minimal FeesSettings for tests.
    fn test_fees_settings() -> FeesSettings {
        FeesSettings {
            epsilon: "0.001".into(),
            ratio: "0.035".into(),
            base_fee: "0.0".into(),
            mode: FeePickMode::Uniform,
            seed: None,
            platform_address: None,
            platform_address_signature: None,
            platform_fee_ratio: "0.45".into(),
            fee_tiers: vec![],
            treasury_addresses: vec![],
            treasury_fee_percent: 35,
            coordinator_fee_percent: 65,
            fee_distribution: None,
            mint_fee_base: Some("1.0".into()),
            mint_fee_ratio: Some("0.02".into()),
            token_creation_fee: Some("100".into()),
            nft_mint_fee: Some("0.5".into()),
            nft_fee_exempt_types: vec!["reward".into()],
            block_reward: "0.1".into(),
            annual_inflation_percent: 2.0,
            creator_reward_percent: 70,
            treasury_reward_percent: 20,
            burn_percent: 10,
            distribution_interval_sec: 600,
            daily_inflation_enabled: false,
            daily_inflation_interval_sec: 86400,
        }
    }

    #[test]
    fn resolve_no_override_uses_global() {
        let global = test_fees_settings();
        let eff = resolve_effective_fees(&global, None);

        assert_eq!(eff.ratio, "0.035");
        assert_eq!(eff.base_fee, "0.0");
        assert_eq!(eff.mint_fee_base, Some("1.0".into()));
        assert_eq!(eff.mint_fee_ratio, Some("0.02".into()));
        assert_eq!(eff.token_creation_fee, Some("100".into()));
        assert_eq!(eff.nft_mint_fee, Some("0.5".into()));
        assert_eq!(eff.nft_fee_exempt_types, vec!["reward".to_string()]);
        assert_eq!(eff.treasury_fee_percent, 35);
        assert_eq!(eff.coordinator_fee_percent, 65);
        assert!(eff.fee_distribution.is_none());
    }

    #[test]
    fn resolve_full_override() {
        let global = test_fees_settings();
        let ov = LedgerFeesOverride {
            ratio: Some("0.01".into()),
            base_fee: Some("0.5".into()),
            platform_fee_ratio: Some("0.10".into()),
            block_reward: Some("0.2".into()),
            fee_tiers: vec![FeeTier { up_to: Some("50".into()), ratio: "0.05".into() }],
            mint_fee_base: Some("2.0".into()),
            mint_fee_ratio: Some("0.03".into()),
            token_creation_fee: Some("200".into()),
            nft_mint_fee: Some("1.0".into()),
            nft_fee_exempt_types: vec!["cube".into()],
            fee_distribution: Some(FeeDistributionConfig::new(8000, 2000)),
            treasury_fee_percent: Some(20),
            coordinator_fee_percent: Some(80),
        };

        let eff = resolve_effective_fees(&global, Some(&ov));

        assert_eq!(eff.ratio, "0.01");
        assert_eq!(eff.base_fee, "0.5");
        assert_eq!(eff.platform_fee_ratio, "0.10");
        assert_eq!(eff.block_reward, "0.2");
        assert_eq!(eff.fee_tiers.len(), 1);
        assert_eq!(eff.fee_tiers[0].ratio, "0.05");
        assert_eq!(eff.mint_fee_base, Some("2.0".into()));
        assert_eq!(eff.mint_fee_ratio, Some("0.03".into()));
        assert_eq!(eff.token_creation_fee, Some("200".into()));
        assert_eq!(eff.nft_mint_fee, Some("1.0".into()));
        assert_eq!(eff.nft_fee_exempt_types, vec!["cube".to_string()]);
        assert!(eff.fee_distribution.is_some());
        assert_eq!(eff.treasury_fee_percent, 20);
        assert_eq!(eff.coordinator_fee_percent, 80);
    }

    #[test]
    fn resolve_partial_override_merges() {
        let global = test_fees_settings();
        let ov = LedgerFeesOverride {
            ratio: Some("0.01".into()),
            nft_mint_fee: Some("2.0".into()),
            ..Default::default()
        };

        let eff = resolve_effective_fees(&global, Some(&ov));

        // Overridden
        assert_eq!(eff.ratio, "0.01");
        assert_eq!(eff.nft_mint_fee, Some("2.0".into()));
        // Inherited from global
        assert_eq!(eff.base_fee, "0.0");
        assert_eq!(eff.mint_fee_base, Some("1.0".into()));
        assert_eq!(eff.token_creation_fee, Some("100".into()));
        assert_eq!(eff.nft_fee_exempt_types, vec!["reward".to_string()]);
        assert_eq!(eff.treasury_fee_percent, 35);
    }

    #[test]
    fn resolve_vec_fields_empty_override_inherits_global() {
        let global = test_fees_settings();
        // fee_tiers and nft_fee_exempt_types empty in override → inherit global
        let ov = LedgerFeesOverride {
            fee_tiers: vec![],
            nft_fee_exempt_types: vec![],
            ..Default::default()
        };

        let eff = resolve_effective_fees(&global, Some(&ov));

        // Empty override → inherits global nft_fee_exempt_types
        assert_eq!(eff.nft_fee_exempt_types, vec!["reward".to_string()]);
        // Global has empty fee_tiers too
        assert!(eff.fee_tiers.is_empty());
    }
}
