use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::{OsRng, RngCore};

/// Génère une clé, signe un message, vérifie que la signature est valide.
#[test]
fn ed25519_roundtrip() {
    let mut rng = OsRng;
    let mut secret = [0u8; 32];
    rng.fill_bytes(&mut secret);
    let sk = SigningKey::from_bytes(&secret);
    let pk: VerifyingKey = sk.verifying_key();

    let msg = b"mvp input bytes";
    let sig: Signature = sk.sign(msg);

    assert!(pk.verify(msg, &sig).is_ok(), "verify_input doit être true");
}
