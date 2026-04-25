//! End-to-end integration test for the encrypted coordinator key flow
//! (audit finding H-key, v0.7.4).
//!
//! Covers the operator journey:
//!   1. Operator has a plain 64-hex coordinator key on disk.
//!   2. They run the equivalent of `tools-cli encrypt-coordinator-key`,
//!      which serialises an `EncryptedKeyFile` envelope to disk.
//!   3. At server boot, `Wallet::load_from_encrypted_file` reads the
//!      envelope, decrypts with the passphrase, and returns a working
//!      `Wallet` whose ECDSA public key matches the original.
//!
//! The unit tests in `key_encryption::tests` already cover the crypto
//! primitives; this test wires them through the on-disk JSON envelope
//! and the `Wallet` construction path, which is what the server actually
//! exercises at boot.

use pms_wallet::Wallet;
use pms_wallet::key_encryption::{EncryptedKeyFile, encrypt_key};
use std::fs;
use tempfile::TempDir;

/// Deterministic 32-byte coordinator key for reproducible assertions.
const TEST_PRIV_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TEST_PASSPHRASE: &str = "correct-horse-battery-staple-9!";

fn seeded_priv_bytes() -> [u8; 32] {
    let v = hex::decode(TEST_PRIV_HEX).unwrap();
    let mut a = [0u8; 32];
    a.copy_from_slice(&v);
    a
}

#[test]
fn load_from_encrypted_file_round_trip() {
    let tmp = TempDir::new().unwrap();
    let enc_path = tmp.path().join("node.key.enc");

    // 1. simulate `tools-cli encrypt-coordinator-key`: encrypt the key and
    //    serialise the envelope to disk.
    let priv_bytes = seeded_priv_bytes();
    let mut pw_enc = TEST_PASSPHRASE.to_string();
    let env = encrypt_key(&priv_bytes, &mut pw_enc).expect("encrypt");
    let json = serde_json::to_string_pretty(&env).expect("serialise envelope");
    fs::write(&enc_path, &json).expect("write envelope");

    println!("wrote encrypted envelope to {}", enc_path.display());
    println!("envelope size = {} bytes", json.len());

    // 2. what the server does at boot:
    let wallet = Wallet::load_from_encrypted_file(
        enc_path.to_str().unwrap(),
        TEST_PASSPHRASE.to_string(),
    )
    .expect("load_from_encrypted_file");

    // 3. the resulting Wallet must match a Wallet built directly from the
    //    same private key — otherwise the server would sign with a
    //    different identity than the operator intended.
    let direct = Wallet::from_hex(TEST_PRIV_HEX).expect("Wallet::from_hex");

    println!("encrypted-loaded pubkey  = {}", wallet.public_key_hex);
    println!("direct-hex-loaded pubkey = {}", direct.public_key_hex);
    println!("encrypted-loaded x25519  = {}", wallet.x25519_pub_hex);
    println!("direct-hex-loaded x25519 = {}", direct.x25519_pub_hex);

    assert_eq!(wallet.public_key_hex, direct.public_key_hex);
    assert_eq!(wallet.x25519_pub_hex, direct.x25519_pub_hex);
    assert_eq!(wallet.private_key_b64, direct.private_key_b64);
}

#[test]
fn load_from_encrypted_file_rejects_wrong_passphrase() {
    let tmp = TempDir::new().unwrap();
    let enc_path = tmp.path().join("node.key.enc");

    let priv_bytes = seeded_priv_bytes();
    let mut pw = TEST_PASSPHRASE.to_string();
    let env = encrypt_key(&priv_bytes, &mut pw).expect("encrypt");
    fs::write(&enc_path, serde_json::to_vec(&env).unwrap()).unwrap();

    let err =
        Wallet::load_from_encrypted_file(enc_path.to_str().unwrap(), "wrong-passphrase".into())
            .expect_err("wrong passphrase must fail");

    println!("wrong passphrase → {err}");
    let msg = err.to_string();
    assert!(
        msg.contains("passphrase") || msg.contains("corrupt"),
        "error must hint at passphrase/corruption: got {msg:?}"
    );
}

#[test]
fn load_from_encrypted_file_rejects_corrupt_envelope() {
    let tmp = TempDir::new().unwrap();
    let enc_path = tmp.path().join("node.key.enc");
    fs::write(&enc_path, b"not actually json at all").unwrap();

    let err = Wallet::load_from_encrypted_file(
        enc_path.to_str().unwrap(),
        TEST_PASSPHRASE.to_string(),
    )
    .expect_err("non-JSON envelope must fail");

    println!("corrupt envelope → {err}");
    assert!(err.to_string().contains("parse"));
}

#[test]
fn envelope_roundtrip_through_disk_and_serde() {
    // Tests the shape the operator will actually see — JSON on disk,
    // human-readable, parseable back into the same struct.
    let priv_bytes = seeded_priv_bytes();
    let mut pw = TEST_PASSPHRASE.to_string();
    let env = encrypt_key(&priv_bytes, &mut pw).expect("encrypt");

    let json = serde_json::to_string_pretty(&env).unwrap();
    println!("---- envelope JSON ----\n{json}\n-----------------------");

    // Document the observable wire shape so a future format change is
    // noticed in code review.
    assert!(json.contains("\"version\": 1"));
    assert!(json.contains("\"kdf\": \"argon2id\""));
    assert!(json.contains("\"m_cost_kib\": 19456"));
    assert!(json.contains("\"salt_b64\":"));
    assert!(json.contains("\"nonce_b64\":"));
    assert!(json.contains("\"ciphertext_b64\":"));

    let parsed: EncryptedKeyFile = serde_json::from_str(&json).unwrap();
    let mut pw2 = TEST_PASSPHRASE.to_string();
    let decrypted = pms_wallet::key_encryption::decrypt_key(&parsed, &mut pw2).unwrap();
    assert_eq!(decrypted, priv_bytes);
}
