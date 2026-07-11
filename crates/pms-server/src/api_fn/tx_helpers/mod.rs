mod fee_policy;
mod coin_selection;
mod block_ops;
mod fee_accumulation;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

// Re-export everything to maintain the existing public API.
// All callers use `crate::api_fn::tx_helpers::X` and must continue to work.

pub use fee_policy::{
    EffectiveFees,
    resolve_effective_fees,
    load_fee_policy,
    dynamic_fee_multiplier,
    load_mint_fee_policy,
    load_token_creation_fee,
    load_nft_mint_fee,
    load_contract_deployment_fee,
    load_storage_fee_per_kb,
    load_burn_rate_bps,
    is_nft_type_fee_exempt,
    try_consume_gas,
};

pub use coin_selection::{select_utxos, select_utxos_multi};

pub use block_ops::{
    get_block_parents,
    forge_and_sign_block,
    persist_and_broadcast,
    persist_and_broadcast_with_delta,
    apply_utxo_delta,
    replicate_unlocks,
};

pub use fee_accumulation::{
    accumulate_tx_fee,
    create_reward_block,
};
