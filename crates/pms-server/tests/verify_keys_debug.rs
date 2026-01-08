use k256::FieldBytes;
use k256::ecdsa::{SigningKey, VerifyingKey};
use k256::elliptic_curve::sec1::ToEncodedPoint;

#[test]
fn verify_coordinator_keys_debug() {
    const PRIV_HEX: &str = "52f4cb8344e318c120f87bc0efb429bdd6b379c700731af27aaf59efffc0b248";
    const EXPECTED_PUB: &str = "03a1829d324538dee1265c879b720e42e15d19a4dbf464f0943a03b5d29aad02ff";

    let priv_bytes = hex::decode(PRIV_HEX).expect("Invalid hex");
    assert_eq!(priv_bytes.len(), 32, "Private key len must be 32");

    let signing_key =
        SigningKey::from_bytes(FieldBytes::from_slice(&priv_bytes)).expect("Invalid key");
    let verifying_key = VerifyingKey::from(&signing_key);

    let pub_key_bytes = verifying_key.to_encoded_point(true);
    let pub_hex = hex::encode(pub_key_bytes.as_bytes());

    println!("Private Key: {}", PRIV_HEX);
    println!("Derived Public Key: {}", pub_hex);
    println!("Expected Public Key: {}", EXPECTED_PUB);

    assert_eq!(pub_hex, EXPECTED_PUB, "Key mismatch!");
}
