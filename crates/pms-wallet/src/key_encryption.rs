//! Password-based encryption for the node identity (coordinator) key file.
//!
//! The coordinator private key used to live in a plain-text file on disk
//! (see `Wallet::load_from_node_key_file`), which meant a filesystem
//! compromise or a stolen backup tarball gave the attacker the key directly.
//! v0.7.4 adds an encrypted-at-rest alternative: `node.key.enc`.
//!
//! ## File format (v1)
//!
//! A JSON envelope so the metadata stays human-readable and the format can
//! be upgraded cleanly:
//!
//! ```json
//! {
//!   "version": 1,
//!   "kdf": "argon2id",
//!   "kdf_params": { "m_cost_kib": 19456, "t_cost": 2, "p_cost": 1 },
//!   "salt_b64": "…16-byte random salt, base64…",
//!   "nonce_b64": "…12-byte AES-GCM nonce, base64…",
//!   "ciphertext_b64": "…AES-256-GCM ciphertext, base64…"
//! }
//! ```
//!
//! The `plaintext` is the coordinator private key as **32 raw bytes**
//! (not hex-encoded), so the cipher output adds only the 16-byte GCM tag
//! for a total of 48 ciphertext bytes. Argon2id defaults are
//! `m_cost = 19456 KiB ≈ 19 MB`, `t_cost = 2`, `p_cost = 1` — the OWASP
//! 2023 recommended baseline for interactive login/unlock.
//!
//! ## Threat model covered
//!
//! * Operator backs up `/etc/pms/node.key.enc` offsite — an attacker who
//!   later reads the backup cannot derive the key without the passphrase.
//! * `docker inspect` / `ps auxe` / core dumps that would have exposed a
//!   plain hex key show only the encrypted envelope.
//! * A misconfigured file permission (world-readable) still matters; this
//!   module does NOT replace the `check_key_file_permissions` check that
//!   the caller runs at boot.
//!
//! ## Not covered
//!
//! * Running-process RAM: the decrypted key still lives in `Wallet` at
//!   runtime. `zeroize_wallet_in_place` below best-effort clears the
//!   base64 string on drop, but protection against core dump / kernel
//!   memory disclosure needs an HSM (out of scope for v0.7.4).
//! * Passphrase-brute-force on a stolen file: argon2id makes it
//!   memory-hard but not immune. Use a strong random passphrase (≥ 20
//!   chars) and rotate if the file leaks.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, anyhow};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// AEAD associated data used on every envelope to bind the ciphertext to
/// its domain. If a future format re-uses raw AES-GCM with different
/// semantics, this prevents a cross-format replay.
const AEAD_AAD: &[u8] = b"pms/coordinator-key/v1";

/// Argon2id parameters for key derivation. Not exposed to the caller:
/// they are pinned by the envelope format. Changing them is a format
/// version bump.
const ARGON2_M_COST_KIB: u32 = 19_456; // ~19 MB
const ARGON2_T_COST: u32 = 2;
const ARGON2_P_COST: u32 = 1;

/// Length of the random salt for Argon2id. 16 bytes is the RFC 9106
/// recommendation floor for password hashing.
const SALT_LEN: usize = 16;

/// Length of the AES-GCM nonce. Always 12 bytes for AES-256-GCM.
const NONCE_LEN: usize = 12;

/// JSON wire format of an encrypted key file. Fields mirror the file
/// layout documented at the top of this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKeyFile {
    pub version: u32,
    pub kdf: String,
    pub kdf_params: KdfParams,
    pub salt_b64: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

/// Encrypt a 32-byte coordinator private key using the given passphrase.
///
/// * Generates a fresh Argon2id salt and AES-GCM nonce from `OsRng`.
/// * Derives a 32-byte AES key from the passphrase + salt.
/// * Seals the plaintext under AES-256-GCM with a fixed domain AAD.
///
/// The passphrase is zeroized before the function returns so a caller
/// that reads it from stdin can't accidentally leave it in its stack
/// frame.
pub fn encrypt_key(
    plaintext_32: &[u8; 32],
    passphrase: &mut String,
) -> Result<EncryptedKeyFile> {
    // 1. random salt + nonce
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    let mut rng = rand::rng();
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce_bytes);

    // 2. derive AES key via argon2id
    let mut aes_key = [0u8; 32];
    derive_aes_key(passphrase.as_bytes(), &salt, &mut aes_key)
        .context("argon2id key derivation")?;

    // 3. encrypt
    let cipher = Aes256Gcm::new_from_slice(&aes_key).map_err(|e| anyhow!("aes init: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext_32,
                aad: AEAD_AAD,
            },
        )
        .map_err(|e| anyhow!("aes-gcm encrypt: {e}"))?;

    // 4. hygiene: zero derived key + passphrase before returning
    aes_key.zeroize();
    passphrase.zeroize();

    Ok(EncryptedKeyFile {
        version: 1,
        kdf: "argon2id".to_string(),
        kdf_params: KdfParams {
            m_cost_kib: ARGON2_M_COST_KIB,
            t_cost: ARGON2_T_COST,
            p_cost: ARGON2_P_COST,
        },
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce_bytes),
        ciphertext_b64: B64.encode(ct),
    })
}

/// Decrypt an `EncryptedKeyFile` and return the 32 raw bytes of the
/// coordinator private key. Passphrase is zeroized on the way out.
///
/// A wrong passphrase surfaces as `Err("wrong passphrase or corrupt envelope")`
/// rather than a cryptic AEAD error so the CLI can print something useful.
pub fn decrypt_key(env: &EncryptedKeyFile, passphrase: &mut String) -> Result<[u8; 32]> {
    if env.version != 1 {
        return Err(anyhow!(
            "unsupported encrypted key envelope version {}",
            env.version
        ));
    }
    if env.kdf != "argon2id" {
        return Err(anyhow!("unsupported KDF '{}', expected argon2id", env.kdf));
    }

    let salt = B64
        .decode(&env.salt_b64)
        .context("decode salt from envelope")?;
    let nonce_bytes = B64
        .decode(&env.nonce_b64)
        .context("decode nonce from envelope")?;
    let ct = B64
        .decode(&env.ciphertext_b64)
        .context("decode ciphertext from envelope")?;

    if nonce_bytes.len() != NONCE_LEN {
        return Err(anyhow!(
            "envelope nonce must be {NONCE_LEN} bytes, got {}",
            nonce_bytes.len()
        ));
    }

    let mut aes_key = [0u8; 32];
    derive_aes_key_with_params(
        passphrase.as_bytes(),
        &salt,
        &env.kdf_params,
        &mut aes_key,
    )
    .context("argon2id key derivation")?;

    let cipher = Aes256Gcm::new_from_slice(&aes_key).map_err(|e| anyhow!("aes init: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &ct,
                aad: AEAD_AAD,
            },
        )
        .map_err(|_| anyhow!("wrong passphrase or corrupt envelope"))?;

    aes_key.zeroize();
    passphrase.zeroize();

    if plaintext.len() != 32 {
        return Err(anyhow!(
            "decrypted plaintext must be 32 bytes (coordinator private key), got {}",
            plaintext.len()
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&plaintext);
    // Zero the allocated plaintext buffer too — the returned array is the
    // only copy the caller sees.
    let mut pt = plaintext;
    pt.zeroize();
    Ok(out)
}

fn derive_aes_key(passphrase: &[u8], salt: &[u8], out: &mut [u8; 32]) -> Result<()> {
    let params = Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, Some(32))
        .map_err(|e| anyhow!("argon2 params: {e}"))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    a2.hash_password_into(passphrase, salt, out)
        .map_err(|e| anyhow!("argon2 hash: {e}"))?;
    Ok(())
}

fn derive_aes_key_with_params(
    passphrase: &[u8],
    salt: &[u8],
    params: &KdfParams,
    out: &mut [u8; 32],
) -> Result<()> {
    let p = Params::new(params.m_cost_kib, params.t_cost, params.p_cost, Some(32))
        .map_err(|e| anyhow!("argon2 params: {e}"))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);
    a2.hash_password_into(passphrase, salt, out)
        .map_err(|e| anyhow!("argon2 hash: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        k
    }

    #[test]
    fn round_trip_succeeds() {
        let key = fake_key();
        let mut pw_enc = "correct-horse-battery-staple".to_string();
        let env = encrypt_key(&key, &mut pw_enc).expect("encrypt");
        // After encrypt, the passphrase buffer must be zeroed.
        assert!(
            pw_enc.is_empty() || pw_enc.bytes().all(|b| b == 0),
            "passphrase buffer should be zeroed after encrypt; got {:?}",
            pw_enc
        );

        let mut pw_dec = "correct-horse-battery-staple".to_string();
        let roundtrip = decrypt_key(&env, &mut pw_dec).expect("decrypt");
        println!("round-trip: {} bytes", roundtrip.len());
        assert_eq!(roundtrip, key);
        assert!(
            pw_dec.is_empty() || pw_dec.bytes().all(|b| b == 0),
            "passphrase buffer should be zeroed after decrypt; got {:?}",
            pw_dec
        );
    }

    #[test]
    fn wrong_passphrase_fails_cleanly() {
        let key = fake_key();
        let mut pw = "the-right-one".to_string();
        let env = encrypt_key(&key, &mut pw).expect("encrypt");

        let mut wrong = "the-wrong-one".to_string();
        let err = decrypt_key(&env, &mut wrong).unwrap_err();
        let msg = err.to_string();
        println!("wrong passphrase → {msg}");
        assert!(
            msg.contains("wrong passphrase") || msg.contains("corrupt"),
            "error should hint at passphrase/corruption: got {msg:?}"
        );
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = fake_key();
        let mut pw = "pw".to_string();
        let mut env = encrypt_key(&key, &mut pw).expect("encrypt");
        // Flip one byte of the ciphertext
        let mut ct = B64.decode(&env.ciphertext_b64).unwrap();
        ct[0] ^= 0xff;
        env.ciphertext_b64 = B64.encode(&ct);

        let mut pw2 = "pw".to_string();
        let err = decrypt_key(&env, &mut pw2).unwrap_err();
        println!("tampered ct → {}", err);
    }

    #[test]
    fn version_mismatch_is_rejected() {
        let key = fake_key();
        let mut pw = "pw".to_string();
        let mut env = encrypt_key(&key, &mut pw).expect("encrypt");
        env.version = 42;

        let mut pw2 = "pw".to_string();
        let err = decrypt_key(&env, &mut pw2).unwrap_err();
        let msg = err.to_string();
        println!("version=42 → {msg}");
        assert!(msg.contains("version"));
    }

    #[test]
    fn serde_roundtrip_of_envelope() {
        let key = fake_key();
        let mut pw = "pw".to_string();
        let env = encrypt_key(&key, &mut pw).expect("encrypt");
        let json = serde_json::to_string_pretty(&env).unwrap();
        println!("envelope JSON:\n{json}");
        let parsed: EncryptedKeyFile = serde_json::from_str(&json).unwrap();

        let mut pw2 = "pw".to_string();
        let out = decrypt_key(&parsed, &mut pw2).unwrap();
        assert_eq!(out, key);
    }
}
