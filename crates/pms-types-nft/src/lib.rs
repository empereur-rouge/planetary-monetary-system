//! # pms-types-nft
//!
//! Types pour les NFTs (Non-Fungible Tokens) dans PMS.
//!
//! Ce crate définit :
//! - `Nft` : Structure représentant un NFT
//! - `NftAction` : Actions possibles sur un NFT (Mint, Transfer, Use, Burn)
//! - `NftMetadata` : Métadonnées extensibles
//!
//! ## Voir aussi
//! - Chapitre 5 du Rust Book : Using Structs to Structure Related Data
//!   https://doc.rust-lang.org/book/ch05-00-structs.html

mod action;
mod nft;

pub use action::NftAction;
pub use nft::{Nft, NftMetadata};
