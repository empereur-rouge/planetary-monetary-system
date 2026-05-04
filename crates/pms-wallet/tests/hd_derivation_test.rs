//! Integration tests for `pms_wallet::hd` (BIP32/BIP39/BIP44 hierarchical
//! deterministic key derivation).
//!
//! Run with `cargo test -p pms-wallet --test hd_derivation_test -- --nocapture`.

use bip32::XPrv;
use pms_types_transaction::{OutputId, Transaction, TxInput, TxOutput};
use pms_wallet::SignerBackend;
use pms_wallet::hd::{
    PMS_COIN_TYPE, derive_child_wallet, derive_child_wallet_at_index, master_xprv_from_mnemonic,
    pms_bip44_path,
};

/// Standard BIP39 test vector — "all abandon" + "about". The Wallet pubkey
/// output is fingerprinted by the println so a future regression (someone
/// changes the path or the seed conversion) is loud and bisectable.
const ABANDON_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon \
                                abandon abandon abandon abandon abandon about";

fn fixture_master() -> XPrv {
    master_xprv_from_mnemonic(ABANDON_MNEMONIC, "").unwrap()
}

#[test]
fn known_mnemonic_yields_stable_first_address() {
    let master = fixture_master();
    let w0 = derive_child_wallet_at_index(&master, 0, 0).unwrap();
    let w1 = derive_child_wallet_at_index(&master, 0, 1).unwrap();

    let addr0 = w0.get_address("8e");
    let addr1 = w1.get_address("8e");

    println!("ABANDON test vector:");
    println!("  m/44'/{PMS_COIN_TYPE}'/0'/0/0  → {}", addr0);
    println!("  m/44'/{PMS_COIN_TYPE}'/0'/0/1  → {}", addr1);
    println!("  secp pub[0]    : {}", &w0.public_key_hex[..16]);
    println!("  x25519 pub[0]  : {}", &w0.x25519_pub_hex[..16]);

    // Bech32m + 52-byte payload = ~ 90+ chars. Sanity-check the format.
    assert!(addr0.starts_with("8e1"), "address must start with hrp '8e1'");
    assert!(addr0.len() > 80 && addr0.len() < 110, "unexpected address length: {}", addr0.len());
    assert_ne!(addr0, addr1, "different indices must produce different addresses");

    // Re-derivation reproduces the same wallet bytes (regression canary).
    let w0_again = derive_child_wallet_at_index(&master, 0, 0).unwrap();
    assert_eq!(w0.public_key_hex, w0_again.public_key_hex);
    assert_eq!(w0.private_key_b64, w0_again.private_key_b64);
    assert_eq!(w0.x25519_pub_hex, w0_again.x25519_pub_hex);
    println!("RE-DERIVATION SAME → {} (16 hex chars)", &w0_again.public_key_hex[..16]);
}

/// 1000 derived addresses must all be unique (collision = catastrophic).
/// Also asserts each address is a valid bech32m string the rest of the
/// codebase will accept (decode_address would reject otherwise).
#[test]
fn one_thousand_derived_addresses_all_unique() {
    use pms_wallet::decode_address;
    use std::collections::HashSet;

    let master = fixture_master();

    let mut addresses = HashSet::new();
    let mut secp_pubkeys = HashSet::new();
    let mut x25519_pubkeys = HashSet::new();

    let n = 1_000u32;
    for i in 0..n {
        let w = derive_child_wallet_at_index(&master, 0, i).unwrap();
        let addr = w.get_address("8e");
        decode_address(&addr).unwrap_or_else(|e| panic!("address {addr} fails decode: {e}"));
        assert!(addresses.insert(addr.clone()), "duplicate address at index {i}: {addr}");
        assert!(
            secp_pubkeys.insert(w.public_key_hex.clone()),
            "duplicate secp pubkey at {i}"
        );
        assert!(
            x25519_pubkeys.insert(w.x25519_pub_hex.clone()),
            "duplicate x25519 pubkey at {i}"
        );
    }

    println!(
        "DERIVED {n} ADDRESSES — all unique:\n  first: {}\n  last:  {}",
        derive_child_wallet_at_index(&master, 0, 0).unwrap().get_address("8e"),
        derive_child_wallet_at_index(&master, 0, n - 1).unwrap().get_address("8e"),
    );
    assert_eq!(addresses.len(), n as usize);
    assert_eq!(secp_pubkeys.len(), n as usize);
    assert_eq!(x25519_pubkeys.len(), n as usize);
}

/// Different `account` values must produce disjoint address sets — this is
/// what enables the BIP44 multi-tenant pattern (one account per SaaS
/// platform tenant).
#[test]
fn different_accounts_yield_different_addresses() {
    let master = fixture_master();
    let tenant_a = derive_child_wallet_at_index(&master, 0, 42).unwrap();
    let tenant_b = derive_child_wallet_at_index(&master, 1, 42).unwrap();
    let tenant_c = derive_child_wallet_at_index(&master, 99, 42).unwrap();

    let a = tenant_a.get_address("8e");
    let b = tenant_b.get_address("8e");
    let c = tenant_c.get_address("8e");

    println!("MULTI-TENANT SAME INDEX 42:");
    println!("  account 0 → {}", a);
    println!("  account 1 → {}", b);
    println!("  account 99 → {}", c);

    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
}

/// BIP39 passphrase must produce a different chain (security feature —
/// "25th word" protects the master seed even if the 12 words leak).
#[test]
fn passphrase_produces_independent_chain() {
    let master_no_pass = master_xprv_from_mnemonic(ABANDON_MNEMONIC, "").unwrap();
    let master_with_pass = master_xprv_from_mnemonic(ABANDON_MNEMONIC, "TREZOR").unwrap();

    let w_no = derive_child_wallet_at_index(&master_no_pass, 0, 0).unwrap();
    let w_yes = derive_child_wallet_at_index(&master_with_pass, 0, 0).unwrap();

    println!("BIP39 PASSPHRASE PROTECTION:");
    println!("  no passphrase  index 0 secp_pub: {}", &w_no.public_key_hex[..16]);
    println!("  passphrase=TREZOR index 0 secp_pub: {}", &w_yes.public_key_hex[..16]);

    assert_ne!(w_no.public_key_hex, w_yes.public_key_hex);
}

/// A child wallet from HD derivation must be usable for the SaaS use case:
/// receive PMS, then sign a TX spending it. This crosses Phase 1 (replay
/// protection) — the derived wallet signs a TX bound to network_id and the
/// signature verifies under the same network_id.
#[test]
fn derived_wallet_can_sign_tx_with_network_binding() {
    let master = fixture_master();
    let user_wallet = derive_child_wallet_at_index(&master, 0, 7).unwrap();

    // Build a synthetic TX (the actual UTXO doesn't matter for this test —
    // we're proving the signing pipeline works end-to-end).
    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "deadbeef".repeat(8),
                index: 0,
            },
        }],
        outputs: vec![TxOutput {
            address: "recipient_addr".into(),
            amount: "10.0".into(),
            asset_id: None,
        }],
        fee: "0.001".into(),
        unlocks: vec![],
    };

    let network_id = "pms-mainnet-v1";
    let msg = tx.signing_message(network_id).unwrap();
    let sig = user_wallet.sign(&msg).expect("derived wallet must sign");

    println!("DERIVED WALLET SIGNS:");
    println!("  network_id: {}", network_id);
    println!("  msg (32 hex): {}", &msg[..32]);
    println!("  sig (24 b64): {}", &sig[..24]);

    // Verify the signature with the wallet's own pubkey — proves the
    // BIP32-derived secp256k1 key is internally consistent.
    let verified = user_wallet.verify(&msg, &sig).unwrap();
    assert!(verified, "signature must verify with wallet's own pubkey");

    // Cross-network sanity: signing for a different network MUST yield a
    // different message hash (Phase 1 replay protection invariant).
    let other_net_msg = tx.signing_message("pms-testnet-v1").unwrap();
    assert_ne!(msg, other_net_msg, "different network_id must yield different msg");
    println!(
        "CROSS-NETWORK MSG DIFFERS: mainnet[0..16]={}  testnet[0..16]={}",
        &msg[..16],
        &other_net_msg[..16]
    );
}

/// Custom path test: the bench path syntax accepts arbitrary BIP32 paths,
/// not just the BIP44 helper. Useful for non-BIP44 setups (e.g. a custom
/// derivation scheme for treasury wallets).
#[test]
fn arbitrary_path_works() {
    let master = fixture_master();
    let w_bip44 = derive_child_wallet(&master, &pms_bip44_path(0, 0, 0)).unwrap();
    let w_custom = derive_child_wallet(&master, "m/0'/0/0").unwrap();

    println!("ARBITRARY PATHS:");
    println!("  BIP44 m/44'/{PMS_COIN_TYPE}'/0'/0/0  → {}", &w_bip44.public_key_hex[..16]);
    println!("  custom m/0'/0/0                       → {}", &w_custom.public_key_hex[..16]);

    assert_ne!(w_bip44.public_key_hex, w_custom.public_key_hex);
}

/// Invalid mnemonic / path inputs must error gracefully (no panic, no
/// silent default that could mask a bug).
#[test]
fn invalid_inputs_error_cleanly() {
    let bad_mnemonic = master_xprv_from_mnemonic("not a real mnemonic at all here", "");
    assert!(bad_mnemonic.is_err());
    println!("BAD MNEMONIC ERR: {:?}", bad_mnemonic.err().unwrap());

    let master = fixture_master();
    let bad_path = derive_child_wallet(&master, "not_a_path");
    assert!(bad_path.is_err());
    println!("BAD PATH ERR: {:?}", bad_path.err().unwrap());
}
