// ═══════════════════════════════════════════════════════════════════════════════
// Fee Distribution Module
// ═══════════════════════════════════════════════════════════════════════════════
//
// Ce module gere la distribution des frais de transaction entre :
// - Le Coordinator : 65% par defaut
// - Le Treasury (wallets admin) : 35% par defaut
//
// Mode centralise : seul le Coordinator traite les transactions.
// ═══════════════════════════════════════════════════════════════════════════════

mod compute;
mod distribute;
mod rewards;
mod inflation;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

// Re-export everything to maintain the existing public API.
// All callers use `crate::fee_distribution::X` and must continue to work.

pub use compute::{FeeOutput, compute_fee_outputs};
pub use distribute::{DistributeFeesResult, perform_fee_distribution};
pub use rewards::{BlockRewardConfig, compute_block_reward_outputs};
pub use inflation::perform_daily_inflation_mint;

/// Validate that the treasury fee configuration is internally consistent.
///
/// When `treasury_fee_percent > 0` the operator wants a share of every fee
/// distribution to land in a treasury wallet. If no treasury addresses are
/// configured (neither in the signed `treasury_wallets` file nor in the
/// `[fees].treasury_addresses` list), the runtime used to silently reroute
/// the cut to the node pool — a misconfiguration nobody ever noticed from
/// the logs (audit finding H4).
///
/// This helper is called at boot time (see `serve.rs`) to surface the bug
/// with a loud `tracing::error!` immediately, and is also suitable as a
/// fail-fast check for stricter deployments.
///
/// # Returns
/// * `Ok(())` if the config is consistent (either `treasury_fee_percent == 0`
///   or at least one treasury address is configured).
/// * `Err(description)` describing exactly what to fix.
pub fn validate_treasury_config(
    treasury_fee_percent: u8,
    treasury_addresses: &[String],
    signed_treasury_wallets_len: usize,
) -> Result<(), String> {
    if treasury_fee_percent > 0
        && treasury_addresses.is_empty()
        && signed_treasury_wallets_len == 0
    {
        return Err(format!(
            "treasury_fee_percent = {}% but no treasury addresses are configured. \
             Either set [fees].treasury_fee_percent = 0 to disable the surcharge, \
             or add at least one entry to [fees].treasury_addresses (or the signed \
             treasury_wallets file). The runtime will otherwise redirect that share \
             to the node pool silently, which hides the misconfig from audit.",
            treasury_fee_percent
        ));
    }
    Ok(())
}
