//! BIP32/BIP39/BIP44 hierarchical deterministic wallet derivation.
//!
//! Generates N child wallets from a single master secret (seed or mnemonic),
//! so a SaaS platform can issue per-user deposit addresses without storing N
//! private keys. Each derived wallet is a full [`Wallet`] (secp256k1 +
//! X25519) usable for receiving and decrypting.
//!
//! # Quick start
//!
//! ```ignore
//! use pms_wallet::hd;
//!
//! let mnemonic = "abandon abandon abandon abandon abandon abandon \
//!                 abandon abandon abandon abandon abandon about";
//! let master = hd::master_xprv_from_mnemonic(mnemonic, "")?;
//!
//! // Derive deposit address #42 for a SaaS user.
//! let user_wallet = hd::derive_child_wallet_at_index(&master, 0, 42)?;
//! let deposit_addr = user_wallet.get_address("8e");
//! ```
//!
//! # Cold/hot pattern (recommended)
//!
//! - **Cold**: master mnemonic stored encrypted (HSM, hardware wallet, sealed
//!   `node.key.enc`). Never on the live server.
//! - **Hot**: server holds the encrypted master, decrypts it momentarily to
//!   derive a [`Wallet`] for one user, signs/dispatches, then drops the
//!   master from RAM. The derived child wallet may persist for that user's
//!   lifetime; the master must not.
//!
//! # Watch-only limitation (not yet supported)
//!
//! True watch-only mode (xpub on server, master never online) is **not**
//! implemented in this module. Reason: PMS addresses bind two pubkeys —
//! `secp256k1` (signing) AND `x25519` (encryption). The X25519 key is
//! derived from the secp256k1 *private* key via HKDF, so an xpub alone
//! cannot reconstruct it. Two designs are on the table for Phase 2.5:
//!
//! 1. Add a parallel SLIP-0010 derivation path for X25519 — produces a
//!    second xpub for the encryption key, watch-only consumer combines both.
//! 2. Introduce a "deposit-only" address variant that omits the X25519
//!    component — receives PMS but cannot decrypt incoming private payloads.
//!
//! Until then, the SaaS pattern above (master encrypted at rest, decrypted
//! per-derivation) gives ~95% of the benefit: a single secret in cold,
//! deterministic addresses, audit trail via the BIP44 path index.

use crate::wallet::Wallet;
use anyhow::{Result, anyhow};
use bip32::{DerivationPath, XPrv};
use bip39::Mnemonic;
use std::str::FromStr;

/// SLIP-44 coin type for PMS in BIP44 paths.
///
/// Currently uses the private-use range value `0x7FFFFFFF` while we wait
/// for an official SLIP-44 registration. When the official number is
/// assigned, change this constant and ship a one-time migration tool that
/// re-derives addresses for existing users (the new path is incompatible
/// with old derivations).
pub const PMS_COIN_TYPE: u32 = 0x7FFF_FFFF;

/// Build a BIP44 derivation path string for PMS.
///
/// `m / 44' / PMS_COIN_TYPE' / account' / change / index`.
///
/// `change = 0` is the standard external (receive) chain — use this for
/// per-user deposit addresses. `change = 1` is the internal (change) chain.
pub fn pms_bip44_path(account: u32, change: u32, index: u32) -> String {
    format!(
        "m/44'/{}'/{}'/{}/{}",
        PMS_COIN_TYPE, account, change, index
    )
}

/// Convert a BIP39 mnemonic + optional passphrase into a BIP32 master xprv.
/// `passphrase = ""` is standard (no passphrase). Words must be valid
/// English-wordlist BIP39.
pub fn master_xprv_from_mnemonic(mnemonic: &str, passphrase: &str) -> Result<XPrv> {
    let m = Mnemonic::parse_in_normalized(bip39::Language::English, mnemonic)
        .map_err(|e| anyhow!("invalid BIP39 mnemonic: {e}"))?;
    let seed = m.to_seed_normalized(passphrase);
    master_xprv_from_seed(&seed)
}

/// Convert a 64-byte BIP39 seed (or any 16–64 byte secret) into a BIP32
/// master xprv. Direct entry point if the caller already has a seed (e.g.
/// derived from an HSM).
pub fn master_xprv_from_seed(seed: &[u8]) -> Result<XPrv> {
    XPrv::new(seed).map_err(|e| anyhow!("BIP32 master derivation failed: {e}"))
}

/// Derive a child [`Wallet`] from a master xprv at the given BIP32 path.
///
/// Path syntax: `m/44'/PMS_COIN_TYPE'/account'/change/index` (standard BIP44).
/// Use [`pms_bip44_path`] to build it; or pass any custom path string the
/// `bip32` crate accepts.
///
/// The returned [`Wallet`] has both a fresh secp256k1 keypair (BIP32-derived)
/// and an X25519 keypair (HKDF-derived from the secp256k1 private key, same
/// invariant as [`Wallet::from_seed`]).
pub fn derive_child_wallet(master: &XPrv, path: &str) -> Result<Wallet> {
    let dp = DerivationPath::from_str(path)
        .map_err(|e| anyhow!("invalid derivation path '{path}': {e}"))?;
    let mut xprv = master.clone();
    for child_number in dp.into_iter() {
        xprv = xprv
            .derive_child(child_number)
            .map_err(|e| anyhow!("BIP32 derive_child at {child_number}: {e}"))?;
    }
    Wallet::from_priv_bytes(&xprv.private_key().to_bytes())
        .map_err(|e| anyhow!("Wallet::from_priv_bytes on derived xprv: {e}"))
}

/// Convenience: derive a child wallet at `m/44'/PMS_COIN_TYPE'/account'/0/index`.
///
/// `account` is typically `0` for a single-tenant SaaS; bump it per logical
/// account (e.g. one per platform tenant). `index` is the per-user counter
/// stored in the SaaS DB and incremented on each new deposit address.
pub fn derive_child_wallet_at_index(
    master: &XPrv,
    account: u32,
    index: u32,
) -> Result<Wallet> {
    derive_child_wallet(master, &pms_bip44_path(account, 0, index))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check: deriving the same path from the same mnemonic yields
    /// the same child wallet across calls. This is the foundational
    /// invariant for "regenerate-from-cold-backup" disaster recovery.
    #[test]
    fn derivation_is_deterministic() {
        // Standard BIP39 test vector mnemonic ("all abandon" + "about").
        let mnemonic = "abandon abandon abandon abandon abandon abandon \
                        abandon abandon abandon abandon abandon about";
        let master = master_xprv_from_mnemonic(mnemonic, "").unwrap();

        let w1 = derive_child_wallet_at_index(&master, 0, 0).unwrap();
        let w2 = derive_child_wallet_at_index(&master, 0, 0).unwrap();

        assert_eq!(w1.public_key_hex, w2.public_key_hex);
        assert_eq!(w1.x25519_pub_hex, w2.x25519_pub_hex);
        assert_eq!(w1.private_key_b64, w2.private_key_b64);

        // Cross-check: changing the index changes the wallet.
        let w_other = derive_child_wallet_at_index(&master, 0, 1).unwrap();
        assert_ne!(w1.public_key_hex, w_other.public_key_hex);

        println!(
            "DERIVATION DETERMINISTIC OK\n  index 0 secp_pub: {}\n  index 0 x25519:   {}\n  index 1 secp_pub: {}",
            &w1.public_key_hex[..16],
            &w1.x25519_pub_hex[..16],
            &w_other.public_key_hex[..16],
        );
    }
}
