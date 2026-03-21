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
