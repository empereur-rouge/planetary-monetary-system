//! Helper utilities for the `net_adapter` module.
//!
//! Contains small pure functions used across the persist pipeline
//! and event emission logic.

use pms_types::PlainPayload;

/// Retourne le nom du variant PlainPayload sous forme de &str.
pub(super) fn plain_payload_type_str(p: &PlainPayload) -> &'static str {
    match p {
        PlainPayload::Genesis => "Genesis",
        PlainPayload::Mint { .. } => "Mint",
        PlainPayload::TxUtxo(_) => "TxUtxo",
        PlainPayload::TokenBurn { .. } => "TokenBurn",
        PlainPayload::MarketSettle { .. } => "MarketSettle",
        PlainPayload::Milestone { .. } => "Milestone",
        PlainPayload::Nft(_) => "Nft",
        PlainPayload::ConfigUpdate(_) => "ConfigUpdate",
        PlainPayload::GovernanceProposal { .. } => "GovernanceProposal",
        PlainPayload::GovernanceEnact { .. } => "GovernanceEnact",
        PlainPayload::GovernanceCancel { .. } => "GovernanceCancel",
        PlainPayload::Reward { .. } => "Reward",
        PlainPayload::EncryptedReward { .. } => "EncryptedReward",
        PlainPayload::TokenCreate(_) => "TokenCreate",
        PlainPayload::SftClassCreate(_) => "SftClassCreate",
        PlainPayload::RoyaltyUpdate { .. } => "RoyaltyUpdate",
        PlainPayload::CustodialMint { .. } => "CustodialMint",
        PlainPayload::BridgeLock { .. } => "BridgeLock",
        PlainPayload::BridgeMint { .. } => "BridgeMint",
        PlainPayload::Freeze { .. } => "Freeze",
        PlainPayload::Unfreeze { .. } => "Unfreeze",
        PlainPayload::Seize { .. } => "Seize",
        PlainPayload::Reverse { .. } => "Reverse",
        PlainPayload::ContractRegister(_) => "ContractRegister",
        PlainPayload::ContractUpdate { .. } => "ContractUpdate",
        PlainPayload::LedgerOwnershipTransfer { .. } => "LedgerOwnershipTransfer",
        PlainPayload::CoordinatorKeyRotate { .. } => "CoordinatorKeyRotate",
        PlainPayload::ReserveSnapshot { .. } => "ReserveSnapshot",
    }
}
