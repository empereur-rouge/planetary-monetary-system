use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

use super::compute::FeeOutput;

/// Configuration for block rewards (inflation).
#[derive(Debug, Clone)]
pub struct BlockRewardConfig {
    /// Reward per block (e.g. "0.1")
    pub reward_per_block: String,
    /// Percentage to creator
    pub creator_percent: u8,
    /// Percentage to treasury
    pub treasury_percent: u8,
    /// Percentage to burn
    pub burn_percent: u8,
}

impl Default for BlockRewardConfig {
    fn default() -> Self {
        Self {
            reward_per_block: "0.1".to_string(),
            creator_percent: 70,
            treasury_percent: 20,
            burn_percent: 10,
        }
    }
}

/// Calculates the reward outputs for a new block.
///
/// # Arguments
/// * `creator_address` - Address of the block creator
/// * `treasury_address` - Treasury address to receive its share
/// * `config` - Reward configuration
///
/// # Returns
/// (Vec<FeeOutput>, burn_amount) - the outputs and the amount to burn
pub fn compute_block_reward_outputs(
    creator_address: &str,
    treasury_address: &str,
    config: &BlockRewardConfig,
) -> (Vec<FeeOutput>, Decimal) {
    let mut outputs = Vec::new();

    let total: Decimal = config.reward_per_block.parse().unwrap_or(Decimal::ZERO);
    if total == Decimal::ZERO {
        return (outputs, Decimal::ZERO);
    }

    let creator_amount =
        total * Decimal::from_u8(config.creator_percent).unwrap() / Decimal::from(100);
    let treasury_amount =
        total * Decimal::from_u8(config.treasury_percent).unwrap() / Decimal::from(100);
    let burn_amount = total * Decimal::from_u8(config.burn_percent).unwrap() / Decimal::from(100);

    // Creator
    if creator_amount > Decimal::ZERO {
        outputs.push(FeeOutput {
            address: creator_address.to_string(),
            amount: creator_amount.normalize().to_string(),
        });
    }

    // Treasury
    if treasury_amount > Decimal::ZERO {
        outputs.push(FeeOutput {
            address: treasury_address.to_string(),
            amount: treasury_amount.normalize().to_string(),
        });
    }

    (outputs, burn_amount)
}
