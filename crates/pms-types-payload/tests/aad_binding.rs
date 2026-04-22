//! Non-regression tests for audit finding M4.
//!
//! History: the AES-GCM AAD on the body ciphertext used to bind only
//! `len_hint`. An attacker who obtained an `EncryptedPayload` (outside the
//! signed block context) could rewrite the `recipients` list or
//! `ephem_pub` without invalidating the tag — a malleability that's
//! uncomfortable for a banking engine even if the signed block wrapper
//! usually catches it.
//!
//! v0.7.2 contract (this test suite locks in):
//! * Every envelope produced with `KEY_VERSION_CURRENT = 2` carries an
//!   `aad.binding` that hashes `scheme`, `key_version`, `ephem_pub`, and
//!   the sorted set of recipient `kid`s.
//! * Decryption rejects any envelope whose binding no longer matches its
//!   publicly visible fields (tamper detection with a crisp error).
//! * The AES-GCM tag itself still catches everything the binding doesn't
//!   cover, so the two layers together reject every known substitution.
//! * v1 envelopes (`aad.binding == None`) still decrypt cleanly — old
//!   testnet blocks are not rejected.

use pms_types_payload::{EncryptedPayload, KEY_VERSION_CURRENT, PlainPayload};
use pms_types_transaction::TxOutput;
use x25519_dalek::{PublicKey, StaticSecret};

fn make_keypair() -> (String, String) {
    // `aes_gcm::aead::OsRng` re-exports the `rand_core 0.6` OsRng that
    // `x25519-dalek 2.0.1` requires; using `rand::rngs::OsRng` directly
    // would hit the `rand_core 0.6` vs `0.9` trait mismatch.
    use aes_gcm::aead::OsRng;
    let sk = StaticSecret::random_from_rng(OsRng);
    let pk = PublicKey::from(&sk);
    (hex::encode(sk.to_bytes()), hex::encode(pk.to_bytes()))
}

fn sample_plain() -> PlainPayload {
    PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: "8e1recipient".into(),
            amount: "42.00000000".into(),
            asset_id: None,
        }],
    }
}

#[test]
fn v2_envelope_round_trips_and_advertises_binding() {
    let (sk_a_hex, pk_a_hex) = make_keypair();
    let (_sk_b_hex, pk_b_hex) = make_keypair();

    let plain = sample_plain();
    let pt = serde_json::to_vec(&plain).unwrap();

    let env =
        EncryptedPayload::encrypt_for(&pt, &[pk_a_hex.clone(), pk_b_hex.clone()], pt.len() as u32)
            .unwrap();

    println!("v2 envelope key_version = {}", env.key_version);
    println!(
        "v2 envelope aad.binding    = {:?}",
        env.aad.binding.as_deref().map(|s| &s[..16.min(s.len())])
    );
    println!("v2 envelope recipients     = {}", env.recipients.len());

    assert_eq!(env.key_version, KEY_VERSION_CURRENT);
    assert!(
        env.aad.binding.is_some(),
        "key-version 2 envelopes MUST carry an envelope binding"
    );
    assert_eq!(env.recipients.len(), 2);

    let back = env.decrypt_as_payload(&sk_a_hex).unwrap();
    assert!(matches!(back, PlainPayload::Mint { .. }));
}

#[test]
fn binding_tampering_is_rejected() {
    let (sk_hex, pk_hex) = make_keypair();
    let plain = sample_plain();
    let pt = serde_json::to_vec(&plain).unwrap();

    let mut env = EncryptedPayload::encrypt_for(&pt, &[pk_hex], pt.len() as u32).unwrap();

    // Overwrite the binding with a bogus hex string. A well-formed envelope
    // must refuse to decrypt, not silently fall back to the body AAD.
    env.aad.binding = Some("00".repeat(32));

    let err = env
        .decrypt_with(&sk_hex)
        .expect_err("tampered binding MUST fail to decrypt");

    println!("binding-tampered decrypt error = {err}");
    assert!(
        err.contains("binding"),
        "error should point at the binding: got {err:?}"
    );
}

#[test]
fn ephem_pub_swap_is_rejected() {
    let (sk_hex, pk_hex) = make_keypair();
    let plain = sample_plain();
    let pt = serde_json::to_vec(&plain).unwrap();

    let mut env = EncryptedPayload::encrypt_for(&pt, &[pk_hex], pt.len() as u32).unwrap();

    // Swap in a different ephem_pub — the wrap can no longer be opened and
    // the binding check fires before we even reach the AES-GCM body tag.
    let (_sk_other, pk_other) = make_keypair();
    env.recipients[0].ephem_pub = pk_other;

    let err = env
        .decrypt_with(&sk_hex)
        .expect_err("ephem_pub substitution MUST fail");

    println!("ephem_pub-swap decrypt error = {err}");
    assert!(err.contains("binding") || err.contains("no matching"));
}

#[test]
fn recipient_removal_is_rejected_for_remaining_recipients() {
    let (sk_a_hex, pk_a_hex) = make_keypair();
    let (_sk_b_hex, pk_b_hex) = make_keypair();
    let plain = sample_plain();
    let pt = serde_json::to_vec(&plain).unwrap();

    let mut env =
        EncryptedPayload::encrypt_for(&pt, &[pk_a_hex.clone(), pk_b_hex], pt.len() as u32).unwrap();

    // Drop recipient B from the wrap list. The binding still claims two kids;
    // the recomputed binding now covers only one kid, so the v2 check fails.
    env.recipients.truncate(1);

    let err = env
        .decrypt_with(&sk_a_hex)
        .expect_err("dropping a recipient MUST break the binding");

    println!("recipient-removal decrypt error = {err}");
    assert!(err.contains("binding"));
}

#[test]
fn kid_substitution_is_rejected() {
    let (sk_hex, pk_hex) = make_keypair();
    let plain = sample_plain();
    let pt = serde_json::to_vec(&plain).unwrap();

    let mut env = EncryptedPayload::encrypt_for(&pt, &[pk_hex], pt.len() as u32).unwrap();

    // Flip one byte of the kid. Binding is over the set of kids, so the
    // recomputed hash is different and decrypt must refuse.
    let mut kid_bytes = hex::decode(&env.recipients[0].kid).unwrap();
    kid_bytes[0] ^= 0xff;
    env.recipients[0].kid = hex::encode(kid_bytes);

    let err = env
        .decrypt_with(&sk_hex)
        .expect_err("kid tampering MUST fail");

    println!("kid-substitution decrypt error = {err}");
    assert!(err.contains("binding"));
}

#[test]
fn v1_envelopes_still_decrypt_for_backward_compat() {
    let (sk_hex, pk_hex) = make_keypair();
    let plain = sample_plain();
    let pt = serde_json::to_vec(&plain).unwrap();

    // Build a real envelope and then "downgrade" it to v1 by stripping the
    // binding and rewriting the body ciphertext under the len_hint-only AAD
    // — this simulates what v0.7.1 blocks look like on disk.
    let env_v2 = EncryptedPayload::encrypt_for(&pt, &[pk_hex.clone()], pt.len() as u32).unwrap();

    // Re-encrypt the body with only `{"len_hint":N}` as AAD to get a real
    // v1-style ciphertext; we reuse the DEK indirectly by inverting decrypt.
    let plaintext = env_v2.decrypt_with(&sk_hex).unwrap();

    // Manually construct a v1 envelope: key_version=1, aad.binding=None,
    // ciphertext computed with the v1 AAD.
    use aes_gcm::aead::{Aead, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use base64::{Engine, engine::general_purpose::STANDARD};
    use rand::RngCore;

    let mut rng = rand::rng();
    let mut dek = [0u8; 32];
    rng.fill_bytes(&mut dek);
    let mut nonce = [0u8; 12];
    rng.fill_bytes(&mut nonce);

    let aad_v1 = pms_types_payload::AAD {
        len_hint: plaintext.len() as u32,
        binding: None,
    };
    let aad_bytes = serde_json::to_vec(&aad_v1).unwrap();
    assert_eq!(
        String::from_utf8(aad_bytes.clone()).unwrap(),
        format!("{{\"len_hint\":{}}}", plaintext.len()),
        "v1 AAD wire format must stay byte-identical to pre-v0.7.2"
    );

    let cipher = Aes256Gcm::new_from_slice(&dek).unwrap();
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad_bytes,
            },
        )
        .unwrap();

    // Build the v1 recipient wrap the same way `encrypt_for` does, just
    // inlined here so we can keep key_version=1.
    use hkdf::Hkdf;
    use sha2::Sha256;
    let eph_sk = StaticSecret::random_from_rng(aes_gcm::aead::OsRng);
    let eph_pk = PublicKey::from(&eph_sk);
    let recip_pk_bytes: [u8; 32] = hex::decode(&pk_hex).unwrap().try_into().unwrap();
    let recip_pk = PublicKey::from(recip_pk_bytes);
    let shared = eph_sk.diffie_hellman(&recip_pk);
    let hk = Hkdf::<Sha256>::new(Some(b"pms-dek-wrap"), shared.as_bytes());
    let mut kek = [0u8; 32];
    hk.expand(b"kek-v1", &mut kek).unwrap();
    let mut kid16 = [0u8; 16];
    hk.expand(b"kid-v1", &mut kid16).unwrap();
    let kid = hex::encode(kid16);
    let mut kw_nonce = [0u8; 12];
    rng.fill_bytes(&mut kw_nonce);
    let kw_cipher = Aes256Gcm::new_from_slice(&kek).unwrap();
    let wrapped = kw_cipher
        .encrypt(
            Nonce::from_slice(&kw_nonce),
            Payload {
                msg: &dek,
                aad: kid.as_bytes(),
            },
        )
        .unwrap();

    let commitment = {
        use sha2::Digest;
        let mut h = Sha256::new();
        h.update(&plaintext);
        hex::encode(h.finalize())
    };

    let v1 = EncryptedPayload {
        scheme: "x25519+aes256gcm".into(),
        key_version: 1,
        aad: aad_v1,
        commitment,
        ciphertext_b64: STANDARD.encode(&ct),
        recipients: vec![pms_types_payload::KeyWrap {
            kid,
            ephem_pub: hex::encode(eph_pk.to_bytes()),
            wrapped_key_b64: STANDARD.encode(&wrapped),
            kw_nonce_b64: STANDARD.encode(&kw_nonce),
        }],
        nonce_b64: STANDARD.encode(&nonce),
    };

    println!("v1 envelope key_version = {}", v1.key_version);
    println!("v1 envelope aad.binding = {:?}", v1.aad.binding);

    // This is the critical backward-compat invariant: v1 envelopes MUST
    // still decrypt in v0.7.2 — otherwise every encrypted block already
    // stored on testnet becomes unreadable.
    let back = v1.decrypt_with(&sk_hex).expect("v1 must still decrypt");
    assert_eq!(back, plaintext);
}

#[test]
fn v1_envelope_serialisation_omits_binding_field() {
    let aad_v1 = pms_types_payload::AAD {
        len_hint: 99,
        binding: None,
    };
    let json = serde_json::to_string(&aad_v1).unwrap();
    println!("v1 AAD wire format = {json}");
    assert_eq!(
        json, r#"{"len_hint":99}"#,
        "omitting `binding` keeps v1 byte-identical; any drift breaks old block decode"
    );

    let aad_v2 = pms_types_payload::AAD {
        len_hint: 99,
        binding: Some("deadbeef".into()),
    };
    let json2 = serde_json::to_string(&aad_v2).unwrap();
    println!("v2 AAD wire format = {json2}");
    assert_eq!(json2, r#"{"len_hint":99,"binding":"deadbeef"}"#);
}
