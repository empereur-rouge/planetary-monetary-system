use anyhow::Result;
use pms_wallet::{SignerBackend, Wallet};

#[test]
fn x25519_sk_hex_fallback_without_mnemonic_is_stable() -> Result<()> {
    // Wallet sans mnemonic
    let w1 = Wallet::from_seed(&[10u8; 32], None).unwrap();
    assert!(w1.mnemonic_words.is_none());

    let sk1 = w1
        .x25519_sk_hex()
        .expect("x25519_sk_hex must be Some without mnemonic");
    let sk1_bis = w1
        .x25519_sk_hex()
        .expect("x25519_sk_hex must be Some without mnemonic");

    // Stable (déterministe)
    assert_eq!(sk1, sk1_bis);

    // Format attendu: 32 bytes en hex => 64 chars
    assert_eq!(sk1.len(), 64);
    assert!(sk1.chars().all(|c| c.is_ascii_hexdigit()));

    // Deux seeds différentes => clé différente
    let w2 = Wallet::from_seed(&[11u8; 32], None).unwrap();
    let sk2 = w2
        .x25519_sk_hex()
        .expect("x25519_sk_hex must be Some without mnemonic");
    assert_ne!(sk1, sk2);

    Ok(())
}

#[test]
fn x25519_pk_matches_sk_derivation() {
    let w = Wallet::from_seed(&[1u8; 32], None).unwrap();
    let sk = w.x25519_sk_hex().unwrap();

    use x25519_dalek::{PublicKey as XPublic, StaticSecret};
    let mut sk_bytes = [0u8; 32];
    sk_bytes.copy_from_slice(&hex::decode(sk).unwrap());
    let sk = StaticSecret::from(sk_bytes);
    let pk = XPublic::from(&sk);

    assert_eq!(hex::encode(pk.as_bytes()), w.x25519_pub_hex);
}
