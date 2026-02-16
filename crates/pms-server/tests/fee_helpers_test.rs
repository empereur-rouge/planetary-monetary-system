// Integration tests for fee helper functions (load_*, is_nft_type_fee_exempt)
// with EffectiveFees fallback when RuntimeConfig has no overrides.

use std::sync::Arc;
use tempfile::tempdir;

use pms_config::{FeePickMode, FeesSettings, FeeTier};
use pms_server::api_fn::tx_helpers::{
    is_nft_type_fee_exempt, load_mint_fee_policy, load_nft_mint_fee, load_token_creation_fee,
    resolve_effective_fees, EffectiveFees,
};
use pms_storage::rocks_store::store::RocksStore;
use rust_decimal::Decimal;

/// Create a temporary RocksStore with default RuntimeConfig.
async fn temp_store() -> Arc<RocksStore> {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("rocks-fee-test");
    // Leak the tempdir so it doesn't get cleaned up during test
    std::mem::forget(dir);
    Arc::new(
        RocksStore::new(db_path.to_str().unwrap(), 256, "test:fee", None)
            .await
            .expect("RocksStore::new failed"),
    )
}

/// Helper: build EffectiveFees with specific values set.
fn eff_with_mint_fee() -> EffectiveFees {
    let global = base_fees_settings();
    let mut eff = resolve_effective_fees(&global, None);
    eff.mint_fee_base = Some("1.0".into());
    eff.mint_fee_ratio = Some("0.02".into());
    eff
}

fn eff_with_token_creation_fee(fee: &str) -> EffectiveFees {
    let global = base_fees_settings();
    let mut eff = resolve_effective_fees(&global, None);
    eff.token_creation_fee = Some(fee.to_string());
    eff
}

fn eff_with_nft_mint_fee(fee: &str) -> EffectiveFees {
    let global = base_fees_settings();
    let mut eff = resolve_effective_fees(&global, None);
    eff.nft_mint_fee = Some(fee.to_string());
    eff
}

fn eff_with_exempt_types(types: Vec<String>) -> EffectiveFees {
    let global = base_fees_settings();
    let mut eff = resolve_effective_fees(&global, None);
    eff.nft_fee_exempt_types = types;
    eff
}

/// Minimal FeesSettings with everything unconfigured.
fn base_fees_settings() -> FeesSettings {
    FeesSettings {
        epsilon: "0.001".into(),
        ratio: "0.035".into(),
        base_fee: "0.0".into(),
        mode: FeePickMode::Uniform,
        seed: None,
        platform_address: None,
        platform_address_signature: None,
        platform_fee_ratio: "0.0".into(),
        fee_tiers: vec![],
        treasury_addresses: vec![],
        treasury_fee_percent: 35,
        coordinator_fee_percent: 65,
        fee_distribution: None,
        mint_fee_base: None,
        mint_fee_ratio: None,
        token_creation_fee: None,
        nft_mint_fee: None,
        nft_fee_exempt_types: vec![],
        block_reward: "0.0".into(),
        annual_inflation_percent: 0.0,
        creator_reward_percent: 0,
        treasury_reward_percent: 0,
        burn_percent: 0,
        distribution_interval_sec: 600,
        daily_inflation_enabled: false,
        daily_inflation_interval_sec: 86400,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// load_mint_fee_policy
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn load_mint_fee_policy_returns_none_when_unconfigured() {
    let store = temp_store().await;
    // No RuntimeConfig, no EffectiveFees → None
    assert!(load_mint_fee_policy(&store, None).is_none());
}

#[tokio::test]
async fn load_mint_fee_policy_falls_back_to_effective_fees() {
    let store = temp_store().await;
    let eff = eff_with_mint_fee();
    let policy = load_mint_fee_policy(&store, Some(&eff));
    assert!(policy.is_some(), "should fall back to EffectiveFees");
}

// ═══════════════════════════════════════════════════════════════════════════
// load_token_creation_fee
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn load_token_creation_fee_returns_none_when_unconfigured() {
    let store = temp_store().await;
    assert!(load_token_creation_fee(&store, None).is_none());
}

#[tokio::test]
async fn load_token_creation_fee_falls_back_to_effective_fees() {
    let store = temp_store().await;
    let eff = eff_with_token_creation_fee("100");
    let fee = load_token_creation_fee(&store, Some(&eff));
    assert_eq!(fee, Some(Decimal::from(100)));
}

#[tokio::test]
async fn load_token_creation_fee_filters_zero() {
    let store = temp_store().await;
    let eff = eff_with_token_creation_fee("0");
    let fee = load_token_creation_fee(&store, Some(&eff));
    assert!(fee.is_none(), "zero fee should be filtered out");
}

// ═══════════════════════════════════════════════════════════════════════════
// load_nft_mint_fee
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn load_nft_mint_fee_falls_back_to_effective_fees() {
    let store = temp_store().await;
    let eff = eff_with_nft_mint_fee("0.5");
    let fee = load_nft_mint_fee(&store, Some(&eff));
    assert_eq!(fee, Some(Decimal::from_str_exact("0.5").unwrap()));
}

// ═══════════════════════════════════════════════════════════════════════════
// is_nft_type_fee_exempt
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn nft_type_exempt_from_effective_fees() {
    let store = temp_store().await;
    let eff = eff_with_exempt_types(vec!["cube".into(), "reward".into()]);

    assert!(is_nft_type_fee_exempt(&store, Some("cube"), Some(&eff)));
    assert!(is_nft_type_fee_exempt(&store, Some("reward"), Some(&eff)));
}

#[tokio::test]
async fn nft_type_exempt_is_case_insensitive() {
    let store = temp_store().await;
    let eff = eff_with_exempt_types(vec!["cube".into()]);

    assert!(is_nft_type_fee_exempt(&store, Some("CUBE"), Some(&eff)));
    assert!(is_nft_type_fee_exempt(&store, Some("Cube"), Some(&eff)));
}

#[tokio::test]
async fn nft_type_not_exempt() {
    let store = temp_store().await;
    let eff = eff_with_exempt_types(vec!["cube".into()]);

    assert!(!is_nft_type_fee_exempt(&store, Some("art"), Some(&eff)));
}

#[tokio::test]
async fn nft_type_none_is_not_exempt() {
    let store = temp_store().await;
    let eff = eff_with_exempt_types(vec!["cube".into()]);

    assert!(!is_nft_type_fee_exempt(&store, None, Some(&eff)));
}

use rust_decimal::prelude::FromStr;

#[tokio::test]
async fn nft_type_exempt_returns_false_without_effective_fees() {
    let store = temp_store().await;
    // No RuntimeConfig exempt types, no EffectiveFees
    assert!(!is_nft_type_fee_exempt(&store, Some("cube"), None));
}
