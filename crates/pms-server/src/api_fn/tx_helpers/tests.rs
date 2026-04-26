use super::*;
use pms_config::{FeePickMode, LedgerFeesOverride};

/// Helper: minimal FeesSettings for tests.
fn test_fees_settings() -> pms_config::FeesSettings {
    pms_config::FeesSettings {
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
            coord_shard_count: 0,
        burn_rate_bps: 0,
        gas_per_tx: None,
        gas_pool_min_balance: None,
        contract_deployment_fee: None,
        storage_fee_per_kb: None,
        dynamic_fee_enabled: false,
        target_tps: 100,
        max_fee_multiplier: 5.0,
        cross_ledger_fee_multiplier: 2.0,
        ledger_annual_fee_pms: None,
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
        fee_tiers: vec![pms_config::FeeTier {
            up_to: Some("50".into()),
            ratio: "0.05".into(),
        }],
        mint_fee_base: Some("2.0".into()),
        mint_fee_ratio: Some("0.03".into()),
        token_creation_fee: Some("200".into()),
        nft_mint_fee: Some("1.0".into()),
        nft_fee_exempt_types: vec!["cube".into()],
        fee_distribution: Some(pms_config::FeeDistributionConfig::new(8000, 2000)),
        treasury_fee_percent: Some(20),
        coordinator_fee_percent: Some(80),
        burn_rate_bps: Some(3000),
        gas_per_tx: Some("0.002".into()),
        gas_pool_min_balance: Some("20.0".into()),
        contract_deployment_fee: Some("50.0".into()),
        storage_fee_per_kb: Some("0.05".into()),
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
    // Economics overrides
    assert_eq!(eff.burn_rate_bps, 3000);
    assert_eq!(eff.gas_per_tx, Some("0.002".into()));
    assert_eq!(eff.gas_pool_min_balance, Some("20.0".into()));
    assert_eq!(eff.contract_deployment_fee, Some("50.0".into()));
    assert_eq!(eff.storage_fee_per_kb, Some("0.05".into()));
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
    // fee_tiers and nft_fee_exempt_types empty in override -> inherit global
    let ov = LedgerFeesOverride {
        fee_tiers: vec![],
        nft_fee_exempt_types: vec![],
        ..Default::default()
    };

    let eff = resolve_effective_fees(&global, Some(&ov));

    // Empty override -> inherits global nft_fee_exempt_types
    assert_eq!(eff.nft_fee_exempt_types, vec!["reward".to_string()]);
    // Global has empty fee_tiers too
    assert!(eff.fee_tiers.is_empty());
}
