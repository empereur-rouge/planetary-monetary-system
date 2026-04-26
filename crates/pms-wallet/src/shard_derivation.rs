//! Coordinator sub-address derivation (audit follow-up to v0.7.4 — UTXO
//! accumulation bottleneck, sub-addresses sharding option).
//!
//! Why this exists. Pre-fix, every transaction's fee output landed on
//! one master coordinator address. Under sustained traffic that single
//! address accumulated UTXOs without bound (~1.5M per 90s in the
//! `test_tps_degradation_profile` measurement); the per-address index
//! HashSet kept rehashing and write throughput halved over a few
//! minutes. With sharding enabled, fees are routed across N derived
//! sub-addresses so the per-address set stays bounded — N=32 keeps
//! each shard at a few thousand entries even at 10K TPS sustained.
//!
//! Derivation contract:
//!
//!   * Each sub-wallet's private key is HKDF-SHA256(master_priv_bytes,
//!     salt, info) where `info` includes the shard index.
//!   * Salt is fixed per release (`SHARD_HKDF_SALT`); changing it
//!     invalidates every previously-derived shard, so it is treated as
//!     a v1 constant.
//!   * `info` includes a domain separator (`"pms/coord-shard/v1/"`) so
//!     this derivation cannot collide with the X25519 derivation
//!     (`"pms/x25519-sk/v1"`) used elsewhere in the wallet.
//!   * The resulting 32 bytes are interpreted as a secp256k1 private
//!     key. If the bytes happen to be ≥ curve order (probability
//!     ~2^-128), `Wallet::from_seed` will reject and we bubble up the
//!     error — the caller can retry with the next index.
//!
//! Auditability:
//!
//!   * Anyone with the master public key cannot derive shard private
//!     keys (HKDF needs the secret), but anyone with the **list of
//!     shard public keys** can verify the cumulative coordinator
//!     balance by summing each shard's UTXO set. The server exposes
//!     the list via `GET /v1/coordinator/info`.

use crate::types::SignerBackend;
use crate::wallet::Wallet;
use base64::{Engine as _, engine::general_purpose};
use hkdf::Hkdf;
use sha2::Sha256;

/// Fixed HKDF salt for v1 of the shard-derivation scheme. Treated as
/// part of the protocol — changing this byte sequence invalidates
/// every previously-derived shard address. Bumped to `v2`/`v3` if the
/// scheme is ever revised.
pub const SHARD_HKDF_SALT: &[u8] = b"pms/coord-shard-salt/v1";

/// Build the HKDF `info` parameter for a given shard index. Includes a
/// domain separator so this derivation cannot collide with the X25519
/// sk derivation in `wallet.rs::derive_x25519_pair_from_private_key_b64`
/// (which uses `b"pms/x25519-sk/v1"` as its info).
fn shard_info(idx: u32) -> Vec<u8> {
    let mut info = Vec::with_capacity(32);
    info.extend_from_slice(b"pms/coord-shard/v1/");
    info.extend_from_slice(&idx.to_be_bytes());
    info
}

/// Deterministically derive coordinator shard wallet `idx` from the
/// master wallet's private key. Same input → same output every time.
///
/// The returned `Wallet` is a fully-formed PMS wallet:
/// - its own secp256k1 keypair (the shard's signing key).
/// - its own X25519 pubkey (from the same derivation chain as the
///   master's, but distinct because the source priv differs).
/// - `mnemonic_words` is `None` because the shard is not standalone
///   recoverable — it's reproduced from the master.
///
/// Errors only when the HKDF output happens to be ≥ secp256k1 curve
/// order (~2^-128 probability per index) — caller can advance to the
/// next index in that vanishingly rare case.
pub fn derive_coord_shard_wallet(master: &Wallet, idx: u32) -> Result<Wallet, String> {
    let master_priv_bytes = general_purpose::STANDARD
        .decode(&master.private_key_b64)
        .map_err(|e| format!("master private key b64 decode: {e}"))?;

    let hk = Hkdf::<Sha256>::new(Some(SHARD_HKDF_SALT), &master_priv_bytes);
    let mut shard_seed = [0u8; 32];
    hk.expand(&shard_info(idx), &mut shard_seed)
        .map_err(|e| format!("HKDF expand failed for shard {idx}: {e}"))?;

    Wallet::from_seed(&shard_seed, None)
}

/// Derive `count` consecutive shard wallets starting at index 0. The
/// common case at boot — collected once into `state.coord_shard_wallets`.
/// Returns an error early if any single derivation fails (so the caller
/// can fail-fast at boot rather than discovering a broken shard later).
pub fn derive_coord_shard_set(master: &Wallet, count: u32) -> Result<Vec<Wallet>, String> {
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        out.push(derive_coord_shard_wallet(master, i)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_master() -> Wallet {
        // Deterministic seed so the test's expected shard addresses
        // are stable across runs.
        Wallet::from_seed(&[0x42u8; 32], None).expect("master wallet from fixed seed")
    }

    #[test]
    fn derivation_is_deterministic_across_calls() {
        let master = fresh_master();
        let s1a = derive_coord_shard_wallet(&master, 0).unwrap();
        let s1b = derive_coord_shard_wallet(&master, 0).unwrap();
        println!("addr_0 (call 1): {}", s1a.get_address("8e"));
        println!("addr_0 (call 2): {}", s1b.get_address("8e"));
        assert_eq!(s1a.private_key_b64, s1b.private_key_b64);
        assert_eq!(s1a.public_key_hex, s1b.public_key_hex);
        assert_eq!(s1a.get_address("8e"), s1b.get_address("8e"));
    }

    #[test]
    fn different_indices_produce_different_addresses() {
        let master = fresh_master();
        let mut addrs = std::collections::HashSet::new();
        for i in 0..16 {
            let w = derive_coord_shard_wallet(&master, i).unwrap();
            let addr = w.get_address("8e");
            println!("shard {i:02}: {addr}");
            assert!(addrs.insert(addr), "shard {i} produced a duplicate address");
        }
        assert_eq!(addrs.len(), 16);
    }

    #[test]
    fn shard_address_differs_from_master() {
        let master = fresh_master();
        let master_addr = master.get_address("8e");
        let shard0 = derive_coord_shard_wallet(&master, 0).unwrap();
        let shard0_addr = shard0.get_address("8e");
        println!("master: {master_addr}");
        println!("shard 0: {shard0_addr}");
        assert_ne!(master_addr, shard0_addr);
    }

    #[test]
    fn different_masters_produce_different_shards() {
        let m1 = Wallet::from_seed(&[0x42u8; 32], None).unwrap();
        let m2 = Wallet::from_seed(&[0x43u8; 32], None).unwrap();
        let s1 = derive_coord_shard_wallet(&m1, 0).unwrap();
        let s2 = derive_coord_shard_wallet(&m2, 0).unwrap();
        println!("m1 shard 0: {}", s1.get_address("8e"));
        println!("m2 shard 0: {}", s2.get_address("8e"));
        assert_ne!(s1.private_key_b64, s2.private_key_b64);
        assert_ne!(s1.get_address("8e"), s2.get_address("8e"));
    }

    #[test]
    fn shard_can_sign_and_verify() {
        // The shard must be a fully-functional wallet (the coordinator
        // needs to sign UTXO unlocks when spending from a shard).
        let master = fresh_master();
        let shard = derive_coord_shard_wallet(&master, 7).unwrap();

        let msg = "test message for shard 7";
        let sig = shard.sign(msg).expect("shard sign");
        let ok = shard.verify(msg, &sig).expect("shard verify");
        println!("shard 7 sign/verify roundtrip: {ok}");
        assert!(ok);
    }

    #[test]
    fn derive_set_returns_n_distinct_wallets() {
        let master = fresh_master();
        let set = derive_coord_shard_set(&master, 32).unwrap();
        assert_eq!(set.len(), 32);
        let addrs: std::collections::HashSet<_> =
            set.iter().map(|w| w.get_address("8e")).collect();
        assert_eq!(addrs.len(), 32);
        println!("derived 32 distinct shard addresses");
    }

    #[test]
    fn shard_x25519_consistency() {
        // Each shard must have a self-consistent X25519 key (priv ↔ pub
        // match), otherwise the encrypted-payload paths would silently
        // misroute when a shard is ever a recipient.
        let master = fresh_master();
        for i in 0..4 {
            let shard = derive_coord_shard_wallet(&master, i).unwrap();
            assert!(
                shard.assert_x25519_consistent(),
                "shard {i} X25519 inconsistent"
            );
        }
    }
}
