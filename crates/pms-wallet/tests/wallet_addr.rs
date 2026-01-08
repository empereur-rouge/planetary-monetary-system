use anyhow::Result;
use bech32::{FromBase32, Variant, decode};
use pms_wallet::{Wallet, decode_address};
use sha2::{Digest, Sha256};

/// Entropie fixe pour tests déterministes
const ENT: [u8; 32] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00,
    0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x01,
];

#[test]
fn address_roundtrip_and_lengths() -> Result<()> {
    let settings = pms_config::load_config()?;
    let (w, _mnemo) = Wallet::generate_with_entropy(ENT).expect("gen");
    assert_eq!(w.x25519_pub_hex.len(), 64, "x25519 pub doit faire 64 hex");

    let addr = w.get_address(&settings.address.hrp);
    let (h20_hex, xpk_hex) = decode_address(&addr).expect("decode ok");

    // hash20 attendu depuis public_key_hex
    let pub_bytes = hex::decode(&w.public_key_hex).expect("pub hex");
    let hash = Sha256::digest(&pub_bytes);
    let expect_h20 = hex::encode(&hash[..20]);

    assert_eq!(h20_hex, expect_h20, "hash20 dans l'adresse");
    assert_eq!(xpk_hex, w.x25519_pub_hex, "x25519 pub dans l'adresse");
    assert!(addr.len() > 16);

    Ok(())
}

#[test]
fn bech32_hrp_variant_and_payload_len() -> Result<()> {
    let settings = pms_config::load_config()?;
    let (w, _) = Wallet::generate_with_entropy(ENT).expect("gen");
    let addr = w.get_address(&settings.address.hrp);

    let (hrp, data, variant) = decode(&addr).expect("bech32 decode");
    // HRP actuel dans ton code est "8e". Adapte si tu changes.
    assert_eq!(hrp, "8e");
    assert_eq!(variant, Variant::Bech32m);

    let bytes: Vec<u8> = Vec::<u8>::from_base32(&data).expect("from_base32");
    assert_eq!(bytes.len(), 52, "payload = 20 (hash) + 32 (x25519)");

    Ok(())
}

#[test]
fn short_address_is_prefix_of_full() -> Result<()> {
    let settings = pms_config::load_config()?;
    let (w, _) = Wallet::generate_with_entropy(ENT).expect("gen");
    let full = w.get_address(&settings.address.hrp);
    let short = w.short_address(&settings.address.hrp);
    assert!(full.starts_with(&short));
    assert_eq!(short.len(), 12);
    assert!(full.starts_with(&short));
    assert_eq!(short.len(), 12);

    Ok(())
}

#[test]
fn tampered_address_fails_decode() -> Result<()> {
    let settings = pms_config::load_config()?;
    let (w, _) = Wallet::generate_with_entropy(ENT).expect("gen");
    let mut addr = w.get_address(&settings.address.hrp);

    // Altère 1 char → checksum invalide
    let last = addr.pop().unwrap();
    addr.push(if last == 'q' { 'p' } else { 'q' });

    assert!(decode_address(&addr).is_err());

    Ok(())
}

#[test]
fn from_word_list_reconstructs_same_wallet() -> Result<()> {
    let settings = pms_config::load_config()?;
    let (w1, mnemo) = Wallet::generate_with_entropy(ENT).expect("gen");
    let words: Vec<&str> = mnemo.split_whitespace().collect();
    assert_eq!(words.len(), 24);

    let w2 = Wallet::from_word_list(&words).expect("from_word_list");

    assert_eq!(w1.public_key_hex, w2.public_key_hex);
    assert_eq!(w1.x25519_pub_hex, w2.x25519_pub_hex);
    assert_eq!(
        w1.get_address(&settings.address.hrp),
        w2.get_address(&settings.address.hrp)
    );

    Ok(())
}
