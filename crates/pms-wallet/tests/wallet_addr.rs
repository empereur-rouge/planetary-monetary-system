use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bech32::{FromBase32, Variant, decode};
use pms_wallet::{Wallet, decode_address};
use sha2::{Digest, Sha256};

/// Entropie fixe pour tests déterministes
const ENT: [u8; 32] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00,
    0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x01,
];

const HRP: &str = "8e";

// ═══════════════════════════════════════════════════════════════════
// Golden test: hardcoded mnemonic → hardcoded expected keys/address
// Catches any regression in the derivation chain.
// ═══════════════════════════════════════════════════════════════════

/// Well-known BIP39 test vector (all "abandon" × 23 + "art")
const GOLDEN_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
const GOLDEN_PRIVATE_KEY_B64: &str = "QIsoXBI4NgBPS4hCyJMkwfATgkUMDUOa80W6f8Saz3A=";
const GOLDEN_PUBLIC_KEY_HEX: &str = "048c6c217c9f502de261f376c964cec4032ae8b7274b48b4fa932b42208bf940fae380f2103dff2ebbf85542e3e1528af485e9487a7b049193d2c4109bf7b02e28";
const GOLDEN_X25519_PUB_HEX: &str = "7f04f0090804b447e5169117a8ca670f95dc34514df838e9f6ad43c7025af73b";
const GOLDEN_ADDRESS: &str = "8e1axzgxg7lq5mm68c0g0azh9s8qq5xe5y90uz0qzggqj6y0egkjyt63jn8p72acdz3fhur360k44puwqj67uasn8ss4k";

#[test]
fn golden_mnemonic_produces_exact_keys_and_address() {
    let words: Vec<&str> = GOLDEN_MNEMONIC.split_whitespace().collect();
    let w = Wallet::from_word_list(&words).expect("from_word_list with valid BIP39 mnemonic");

    assert_eq!(
        w.private_key_b64, GOLDEN_PRIVATE_KEY_B64,
        "private key derivation changed — this is a breaking regression"
    );
    assert_eq!(
        w.public_key_hex, GOLDEN_PUBLIC_KEY_HEX,
        "public key derivation changed — this is a breaking regression"
    );
    assert_eq!(
        w.x25519_pub_hex, GOLDEN_X25519_PUB_HEX,
        "x25519 key derivation changed — this is a breaking regression"
    );
    assert_eq!(
        w.get_address(HRP),
        GOLDEN_ADDRESS,
        "address derivation changed — existing wallets would get wrong addresses"
    );
}

// ═══════════════════════════════════════════════════════════════════
// Real coordinator wallet: hardcoded from a live testnet deployment.
// Both mnemonic and private key MUST recover the exact same address.
// ═══════════════════════════════════════════════════════════════════

const COORD_MNEMONIC: &str = "gain space color filter buzz bind side before sauce twist slam history chief patch desk chunk way oblige output turtle purchase scare token rapid";
const COORD_PRIVATE_KEY_HEX: &str = "da5334ad57455d74e5150e4eb06398ce9cf27762b6f29eac7f82f178bee07406";
const COORD_PUBLIC_KEY_HEX: &str = "04593037fa9d4f6ea21b56cdd0cd504a718e3a8e2502df863ee9ee2fbd431f1c3642b309dcab07c63baa32232b643e86b433045cf31859f9279e7dc6703df02a84";
const COORD_X25519_PUB_HEX: &str = "266bcd33ab2d20d2b4465448c4efeb55005eb0de71b78501984383d20728d80b";
const COORD_ADDRESS: &str = "8e1ahpltzjauwev6lf0jql9szau3gl44u9rye4u6vat95sd9dzx23yvfmlt25q9avx7wxmc2qvcgwpaypegmq9s6u5fr9";

#[test]
fn real_coordinator_wallet_from_mnemonic() {
    let words: Vec<&str> = COORD_MNEMONIC.split_whitespace().collect();
    let w = Wallet::from_word_list(&words).expect("from_word_list");

    assert_eq!(w.public_key_hex, COORD_PUBLIC_KEY_HEX, "public key mismatch from mnemonic");
    assert_eq!(w.x25519_pub_hex, COORD_X25519_PUB_HEX, "x25519 pub mismatch from mnemonic");
    assert_eq!(w.get_address(HRP), COORD_ADDRESS, "address mismatch from mnemonic");
}

#[test]
fn real_coordinator_wallet_from_private_key() {
    let w = Wallet::from_hex(COORD_PRIVATE_KEY_HEX).expect("from_hex");

    assert_eq!(w.public_key_hex, COORD_PUBLIC_KEY_HEX, "public key mismatch from hex");
    assert_eq!(w.x25519_pub_hex, COORD_X25519_PUB_HEX, "x25519 pub mismatch from hex");
    assert_eq!(w.get_address(HRP), COORD_ADDRESS, "address mismatch from hex");
}

#[test]
fn real_coordinator_wallet_mnemonic_and_hex_produce_same_wallet() {
    let words: Vec<&str> = COORD_MNEMONIC.split_whitespace().collect();
    let from_mne = Wallet::from_word_list(&words).expect("from_word_list");
    let from_hex = Wallet::from_hex(COORD_PRIVATE_KEY_HEX).expect("from_hex");

    assert_eq!(from_mne.private_key_b64, from_hex.private_key_b64, "private key mismatch");
    assert_eq!(from_mne.public_key_hex, from_hex.public_key_hex, "public key mismatch");
    assert_eq!(from_mne.x25519_pub_hex, from_hex.x25519_pub_hex, "x25519 key mismatch");
    assert_eq!(from_mne.get_address(HRP), from_hex.get_address(HRP), "address mismatch");
    assert_eq!(from_mne.get_address(HRP), COORD_ADDRESS, "address != expected");
    assert_eq!(from_mne.public_key_hex, COORD_PUBLIC_KEY_HEX, "public key != expected");
    assert_eq!(from_mne.x25519_pub_hex, COORD_X25519_PUB_HEX, "x25519 pub != expected");
}

// ═══════════════════════════════════════════════════════════════════
// Determinism: same mnemonic called N times → identical results
// ═══════════════════════════════════════════════════════════════════

#[test]
fn from_word_list_is_deterministic_across_calls() {
    let words: Vec<&str> = GOLDEN_MNEMONIC.split_whitespace().collect();

    let w1 = Wallet::from_word_list(&words).unwrap();
    let w2 = Wallet::from_word_list(&words).unwrap();
    let w3 = Wallet::from_word_list(&words).unwrap();

    assert_eq!(w1.private_key_b64, w2.private_key_b64);
    assert_eq!(w2.private_key_b64, w3.private_key_b64);
    assert_eq!(w1.public_key_hex, w2.public_key_hex);
    assert_eq!(w2.public_key_hex, w3.public_key_hex);
    assert_eq!(w1.x25519_pub_hex, w2.x25519_pub_hex);
    assert_eq!(w2.x25519_pub_hex, w3.x25519_pub_hex);
    assert_eq!(w1.get_address(HRP), w2.get_address(HRP));
    assert_eq!(w2.get_address(HRP), w3.get_address(HRP));
}

// ═══════════════════════════════════════════════════════════════════
// generate() → extract mnemonic → from_word_list() roundtrip
// ═══════════════════════════════════════════════════════════════════

#[test]
fn generate_then_restore_from_mnemonic_same_wallet() {
    let original = Wallet::generate();
    let mnemonic = original.mnemonic_words.as_ref().expect("generate() must return mnemonic");
    assert_eq!(mnemonic.len(), 24, "mnemonic must be 24 words");

    let words: Vec<&str> = mnemonic.iter().map(|s| s.as_str()).collect();
    let restored = Wallet::from_word_list(&words).expect("from_word_list");

    assert_eq!(original.private_key_b64, restored.private_key_b64, "private key mismatch after mnemonic restore");
    assert_eq!(original.public_key_hex, restored.public_key_hex, "public key mismatch after mnemonic restore");
    assert_eq!(original.x25519_pub_hex, restored.x25519_pub_hex, "x25519 key mismatch after mnemonic restore");
    assert_eq!(
        original.get_address(HRP),
        restored.get_address(HRP),
        "address mismatch after mnemonic restore"
    );
}

// ═══════════════════════════════════════════════════════════════════
// from_hex() roundtrip: private key hex → same wallet
// ═══════════════════════════════════════════════════════════════════

#[test]
fn from_hex_roundtrip_same_keys_and_address() {
    let original = Wallet::generate();

    // Convert b64 → bytes → hex (simulates what a user would do)
    let priv_bytes = STANDARD.decode(&original.private_key_b64).expect("valid b64");
    let priv_hex = hex::encode(&priv_bytes);
    assert_eq!(priv_hex.len(), 64, "hex private key must be 64 chars");

    let restored = Wallet::from_hex(&priv_hex).expect("from_hex");

    assert_eq!(original.private_key_b64, restored.private_key_b64, "private key mismatch after hex restore");
    assert_eq!(original.public_key_hex, restored.public_key_hex, "public key mismatch after hex restore");
    assert_eq!(original.x25519_pub_hex, restored.x25519_pub_hex, "x25519 key mismatch after hex restore");
    assert_eq!(
        original.get_address(HRP),
        restored.get_address(HRP),
        "address mismatch after hex restore"
    );
    // from_hex should not have mnemonic (not derivable from raw key)
    assert!(restored.mnemonic_words.is_none(), "from_hex must not produce mnemonic");
}

// ═══════════════════════════════════════════════════════════════════
// generate_with_entropy → from_word_list roundtrip
// ═══════════════════════════════════════════════════════════════════

#[test]
fn from_word_list_reconstructs_same_wallet() -> Result<()> {
    let (w1, mnemo) = Wallet::generate_with_entropy(ENT).expect("gen");
    let words: Vec<&str> = mnemo.split_whitespace().collect();
    assert_eq!(words.len(), 24);

    let w2 = Wallet::from_word_list(&words).expect("from_word_list");

    assert_eq!(w1.private_key_b64, w2.private_key_b64, "private key mismatch");
    assert_eq!(w1.public_key_hex, w2.public_key_hex, "public key mismatch");
    assert_eq!(w1.x25519_pub_hex, w2.x25519_pub_hex, "x25519 key mismatch");
    assert_eq!(w1.get_address(HRP), w2.get_address(HRP), "address mismatch");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Address structure & encoding tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn address_roundtrip_and_lengths() -> Result<()> {
    let (w, _mnemo) = Wallet::generate_with_entropy(ENT).expect("gen");
    assert_eq!(w.x25519_pub_hex.len(), 64, "x25519 pub must be 64 hex chars");

    let addr = w.get_address(HRP);
    let (h20_hex, xpk_hex) = decode_address(&addr).expect("decode ok");

    let pub_bytes = hex::decode(&w.public_key_hex).expect("pub hex");
    let hash = Sha256::digest(&pub_bytes);
    let expect_h20 = hex::encode(&hash[..20]);

    assert_eq!(h20_hex, expect_h20, "hash20 in address");
    assert_eq!(xpk_hex, w.x25519_pub_hex, "x25519 pub in address");
    assert!(addr.len() > 16);

    Ok(())
}

#[test]
fn bech32_hrp_variant_and_payload_len() -> Result<()> {
    let (w, _) = Wallet::generate_with_entropy(ENT).expect("gen");
    let addr = w.get_address(HRP);

    let (hrp, data, variant) = decode(&addr).expect("bech32 decode");
    assert_eq!(hrp, "8e");
    assert_eq!(variant, Variant::Bech32m);

    let bytes: Vec<u8> = Vec::<u8>::from_base32(&data).expect("from_base32");
    assert_eq!(bytes.len(), 52, "payload = 20 (hash) + 32 (x25519)");

    Ok(())
}

#[test]
fn short_address_is_prefix_of_full() -> Result<()> {
    let (w, _) = Wallet::generate_with_entropy(ENT).expect("gen");
    let full = w.get_address(HRP);
    let short = w.short_address(HRP);
    assert!(full.starts_with(&short));
    assert_eq!(short.len(), 12);

    Ok(())
}

#[test]
fn tampered_address_fails_decode() -> Result<()> {
    let (w, _) = Wallet::generate_with_entropy(ENT).expect("gen");
    let mut addr = w.get_address(HRP);

    let last = addr.pop().unwrap();
    addr.push(if last == 'q' { 'p' } else { 'q' });

    assert!(decode_address(&addr).is_err());

    Ok(())
}
