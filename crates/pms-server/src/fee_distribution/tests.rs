use super::*;
use rust_decimal::Decimal;

// ═══════════════════════════════════════════════════════════════════════
// Tests pour FeeDistributionConfig (N-way, depuis pms_config)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fee_distribution_config_default_sums_to_100() {
    let config = pms_config::FeeDistributionConfig::default();
    assert_eq!(config.beneficiaries.len(), 2);
    assert_eq!(config.beneficiaries[0].role, "coordinator");
    assert_eq!(config.beneficiaries[0].percent_bps, 6500);
    assert_eq!(config.beneficiaries[1].role, "treasury");
    assert_eq!(config.beneficiaries[1].percent_bps, 3500);
    assert!(config.validate().is_ok());
}

#[test]
fn fee_distribution_config_validation_rejects_invalid_sum() {
    let config = pms_config::FeeDistributionConfig::new(6000, 5000); // = 11000
    let result = config.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must sum to 10000"));
}

#[test]
fn fee_distribution_config_accepts_valid_custom() {
    let config = pms_config::FeeDistributionConfig::new(7000, 3000);
    assert!(config.validate().is_ok());
}

#[test]
fn fee_distribution_n_way_split() {
    use pms_config::FeeBeneficiary;
    let config = pms_config::FeeDistributionConfig {
        beneficiaries: vec![
            FeeBeneficiary {
                role: "coordinator".into(),
                percent_bps: 5000,
                address: None,
            },
            FeeBeneficiary {
                role: "client".into(),
                percent_bps: 3000,
                address: Some("client_addr".into()),
            },
            FeeBeneficiary {
                role: "treasury".into(),
                percent_bps: 2000,
                address: None,
            },
        ],
    };
    assert!(config.validate().is_ok());

    let outputs = compute_fee_outputs(
        "100".parse().unwrap(),
        &["treasury_addr".into()],
        "coordinator_addr",
        &config,
    );

    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].address, "coordinator_addr");
    assert_eq!(outputs[0].amount, "50");
    assert_eq!(outputs[1].address, "client_addr");
    assert_eq!(outputs[1].amount, "30");
    assert_eq!(outputs[2].address, "treasury_addr");
    assert_eq!(outputs[2].amount, "20");
}

// ═══════════════════════════════════════════════════════════════════════
// Tests pour BlockRewardConfig
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn block_reward_config_default() {
    let config = BlockRewardConfig::default();
    assert_eq!(config.reward_per_block, "0.1");
    assert_eq!(config.creator_percent, 70);
    assert_eq!(config.treasury_percent, 20);
    assert_eq!(config.burn_percent, 10);
}

// ═══════════════════════════════════════════════════════════════════════
// Tests pour compute_block_reward_outputs
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn compute_block_reward_outputs_splits_correctly() {
    let config = BlockRewardConfig::default(); // 0.1 PMS, 70/20/10
    let creator_addr = "creator_address_123";
    let treasury_addr = "treasury_address_456";

    let (outputs, burn) = compute_block_reward_outputs(creator_addr, treasury_addr, &config);

    // Should have 2 outputs (creator + treasury)
    assert_eq!(outputs.len(), 2);

    // Creator should receive 70% of 0.1 = 0.07
    let creator_output = outputs.iter().find(|o| o.address == creator_addr);
    assert!(creator_output.is_some());
    let creator_amount: Decimal = creator_output.unwrap().amount.parse().unwrap();
    assert_eq!(creator_amount, "0.07".parse::<Decimal>().unwrap());

    // Treasury should receive 20% of 0.1 = 0.02
    let treasury_output = outputs.iter().find(|o| o.address == treasury_addr);
    assert!(treasury_output.is_some());
    let treasury_amount: Decimal = treasury_output.unwrap().amount.parse().unwrap();
    assert_eq!(treasury_amount, "0.02".parse::<Decimal>().unwrap());

    // Burn should be 10% of 0.1 = 0.01
    assert_eq!(burn, "0.01".parse::<Decimal>().unwrap());
}

#[test]
fn compute_block_reward_outputs_zero_reward() {
    let config = BlockRewardConfig {
        reward_per_block: "0".to_string(),
        ..Default::default()
    };

    let (outputs, burn) = compute_block_reward_outputs("creator", "treasury", &config);

    assert!(outputs.is_empty());
    assert_eq!(burn, Decimal::ZERO);
}

#[test]
fn compute_block_reward_outputs_custom_percentages() {
    let config = BlockRewardConfig {
        reward_per_block: "1.0".to_string(),
        creator_percent: 50,
        treasury_percent: 30,
        burn_percent: 20,
    };

    let (outputs, burn) = compute_block_reward_outputs("c", "t", &config);

    assert_eq!(outputs.len(), 2);

    // Creator: 50% of 1.0 = 0.5
    let creator_amount: Decimal = outputs[0].amount.parse().unwrap();
    assert_eq!(creator_amount, "0.5".parse::<Decimal>().unwrap());

    // Treasury: 30% of 1.0 = 0.3
    let treasury_amount: Decimal = outputs[1].amount.parse().unwrap();
    assert_eq!(treasury_amount, "0.3".parse::<Decimal>().unwrap());

    // Burn: 20% of 1.0 = 0.2
    assert_eq!(burn, "0.2".parse::<Decimal>().unwrap());
}

// ═══════════════════════════════════════════════════════════════════════
// Tests for `validate_treasury_config` (audit finding H4)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn validate_treasury_ok_when_percent_is_zero_and_no_addresses() {
    let r = super::validate_treasury_config(0, &[], 0);
    println!("percent=0, no addresses → {:?}", r);
    assert!(r.is_ok(), "treasury tax disabled is always valid config");
}

#[test]
fn validate_treasury_ok_when_percent_positive_and_address_present() {
    let addrs = vec!["8e1treasury".to_string()];
    let r = super::validate_treasury_config(35, &addrs, 0);
    println!("percent=35, 1 address in settings → {:?}", r);
    assert!(r.is_ok());
}

#[test]
fn validate_treasury_ok_when_percent_positive_and_signed_wallet_present() {
    let r = super::validate_treasury_config(10, &[], 1);
    println!("percent=10, 1 signed wallet → {:?}", r);
    assert!(
        r.is_ok(),
        "signed-wallet-file alone must satisfy the invariant"
    );
}

#[test]
fn validate_treasury_rejects_percent_without_any_address() {
    let r = super::validate_treasury_config(35, &[], 0);
    println!("percent=35, no addresses anywhere → {:?}", r);
    let err = r.expect_err("must reject this misconfig");
    // Operator must be told exactly what to fix and at which percent value.
    assert!(err.contains("35%"));
    assert!(err.contains("treasury_fee_percent"));
    assert!(err.contains("treasury_addresses"));
}

#[test]
fn validate_treasury_rejects_percent_with_empty_address_vec() {
    let empty: Vec<String> = vec![];
    let r = super::validate_treasury_config(1, &empty, 0);
    println!("percent=1, empty vec → {:?}", r);
    assert!(
        r.is_err(),
        "even 1% with no addresses must be flagged — silent redirection to node pool hides audit trails"
    );
}
