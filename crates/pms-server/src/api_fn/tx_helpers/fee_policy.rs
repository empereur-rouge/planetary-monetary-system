use crate::api::AppState;
use pms_config::{
    FeeDistributionConfig, FeeTier, FeesSettings, LedgerFeesOverride, RuntimeConfig,
};
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::ConfigStorage;
use pms_token::FeePolicy;
use rust_decimal::Decimal;
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════
// Per-Ledger Effective Fees
// ═══════════════════════════════════════════════════════════════════════════

/// Resolved fee configuration for a specific ledger context.
/// Merges: global FeesSettings <- LedgerFeesOverride.
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
    // Economics
    pub burn_rate_bps: u32,
    pub gas_per_tx: Option<String>,
    pub gas_pool_min_balance: Option<String>,
    pub contract_deployment_fee: Option<String>,
    pub storage_fee_per_kb: Option<String>,
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
            burn_rate_bps: global.burn_rate_bps,
            gas_per_tx: global.gas_per_tx.clone(),
            gas_pool_min_balance: global.gas_pool_min_balance.clone(),
            contract_deployment_fee: global.contract_deployment_fee.clone(),
            storage_fee_per_kb: global.storage_fee_per_kb.clone(),
        },
        Some(ov) => EffectiveFees {
            ratio: ov.ratio.clone().unwrap_or_else(|| global.ratio.clone()),
            base_fee: ov
                .base_fee
                .clone()
                .unwrap_or_else(|| global.base_fee.clone()),
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
            mint_fee_base: ov
                .mint_fee_base
                .clone()
                .or_else(|| global.mint_fee_base.clone()),
            mint_fee_ratio: ov
                .mint_fee_ratio
                .clone()
                .or_else(|| global.mint_fee_ratio.clone()),
            token_creation_fee: ov
                .token_creation_fee
                .clone()
                .or_else(|| global.token_creation_fee.clone()),
            nft_mint_fee: ov
                .nft_mint_fee
                .clone()
                .or_else(|| global.nft_mint_fee.clone()),
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
            burn_rate_bps: ov.burn_rate_bps.unwrap_or(global.burn_rate_bps),
            gas_per_tx: ov
                .gas_per_tx
                .clone()
                .or_else(|| global.gas_per_tx.clone()),
            gas_pool_min_balance: ov
                .gas_pool_min_balance
                .clone()
                .or_else(|| global.gas_pool_min_balance.clone()),
            contract_deployment_fee: ov
                .contract_deployment_fee
                .clone()
                .or_else(|| global.contract_deployment_fee.clone()),
            storage_fee_per_kb: ov
                .storage_fee_per_kb
                .clone()
                .or_else(|| global.storage_fee_per_kb.clone()),
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

/// Compute the dynamic fee multiplier based on current TPS.
/// Returns 1.0 if dynamic fees are disabled.
/// Otherwise returns max(1.0, current_tps / target_tps) capped at max_multiplier.
pub fn dynamic_fee_multiplier(
    store: &Arc<RocksStore>,
    tps_tracker: &pms_economics::dynamic_fee::TpsTracker,
) -> Decimal {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());
    if !rc.dynamic_fee_enabled || rc.target_tps == 0 {
        return Decimal::ONE;
    }
    let mult = tps_tracker.fee_multiplier(rc.target_tps, rc.max_fee_multiplier);
    Decimal::from_f64_retain(mult).unwrap_or(Decimal::ONE)
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
pub fn load_nft_mint_fee(store: &Arc<RocksStore>, eff: Option<&EffectiveFees>) -> Option<Decimal> {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());
    rc.nft_mint_fee
        .as_ref()
        .or_else(|| eff.and_then(|e| e.nft_mint_fee.as_ref()))
        .and_then(|f| Decimal::from_str_exact(f).ok())
        .filter(|d| *d > Decimal::ZERO)
}

/// Load contract deployment fee.
/// Priority: RuntimeConfig > EffectiveFees (per-ledger).
/// Returns None if not configured.
pub fn load_contract_deployment_fee(
    store: &Arc<RocksStore>,
    eff: Option<&EffectiveFees>,
) -> Option<Decimal> {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());
    rc.contract_deployment_fee
        .as_ref()
        .or_else(|| eff.and_then(|e| e.contract_deployment_fee.as_ref()))
        .and_then(|f| Decimal::from_str_exact(f).ok())
        .filter(|d| *d > Decimal::ZERO)
}

/// Load storage fee per KB.
/// Priority: RuntimeConfig > EffectiveFees (per-ledger).
/// Returns None if not configured.
pub fn load_storage_fee_per_kb(
    store: &Arc<RocksStore>,
    eff: Option<&EffectiveFees>,
) -> Option<Decimal> {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());
    rc.storage_fee_per_kb
        .as_ref()
        .or_else(|| eff.and_then(|e| e.storage_fee_per_kb.as_ref()))
        .and_then(|f| Decimal::from_str_exact(f).ok())
        .filter(|d| *d > Decimal::ZERO)
}

/// Load burn rate (basis points).
/// Priority: RuntimeConfig > EffectiveFees (per-ledger) > global FeesSettings.
pub fn load_burn_rate_bps(store: &Arc<RocksStore>, eff: Option<&EffectiveFees>) -> u32 {
    let rc = store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());
    if rc.burn_rate_bps > 0 {
        return rc.burn_rate_bps;
    }
    eff.map(|e| e.burn_rate_bps).unwrap_or(0)
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

/// Consume gas from a ledger's gas pool.
/// Only applies to non-"main" ledgers when gas_per_tx is configured.
/// Returns Ok(()) if consumption succeeded or gas pool is not required.
/// Returns Err(message) with a user-friendly error when pool is depleted.
pub fn try_consume_gas(state: &AppState) -> Result<(), String> {
    // Main ledger never requires gas pool
    if state.ledger_id == "main" {
        return Ok(());
    }

    let eff = &state.effective_fees;
    let gas_per_tx: Decimal = eff
        .gas_per_tx
        .as_ref()
        .and_then(|s| Decimal::from_str_exact(s).ok())
        .unwrap_or(Decimal::ZERO);

    if gas_per_tx.is_zero() {
        return Ok(());
    }

    let min_balance: Decimal = eff
        .gas_pool_min_balance
        .as_ref()
        .and_then(|s| Decimal::from_str_exact(s).ok())
        .unwrap_or(Decimal::ZERO);

    use pms_storage::GasPoolStorage;
    state
        .store
        .consume_gas(&state.ledger_id, gas_per_tx, min_balance)
        .map(|_| ())
        .map_err(|e| format!("Gas pool depleted for ledger '{}': {e}", state.ledger_id))
}
