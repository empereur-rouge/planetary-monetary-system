//! # pms-contracts
//!
//! Crate dédié à l'évaluation des contrats déclaratifs PMS.
//!
//! Ce crate contient :
//! - **engine** : Évaluation pure des contrats (formulas, matching de triggers)
//! - **listener** : Subscriber EventBus qui réagit aux burns NFT et accumule les refunds
//!
//! ## Architecture
//!
//! Les contrats sont enregistrés via les endpoints admin de `pms-server`.
//! L'évaluation est déclenchée par des événements `NftBurnProcessed` émis
//! par les handlers NFT burn. Le listener écoute ces événements, évalue les
//! contrats matchants, et pousse les refunds via le trait `RefundSink`.
//!
//! ```text
//! NFT burn handler → emit NftBurnProcessed → EventBus
//!                                              ↓
//!                                     contract listener
//!                                              ↓
//!                               engine::evaluate_nft_burn()
//!                                              ↓
//!                                  RefundSink::add_burn_refund()
//! ```

pub mod engine;
pub mod listener;

pub use engine::{ContractResult, evaluate_nft_burn};
pub use listener::{RefundSink, spawn_contract_listener};
