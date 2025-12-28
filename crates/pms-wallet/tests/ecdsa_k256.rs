
use pms_wallet::{SignError, SignerBackend, VerifyError, Wallet};

#[test]
fn test_sign_and_verify_success() {
    let backend = Wallet::generate();
    let message = "Hello DAG!";

    let signature = backend.sign(message).expect("Signature should succeed");

    let is_valid = backend
        .verify(message, &signature)
        .expect("Verification should succeed");

    assert!(is_valid, "Signature should be valid");
}

#[test]
fn test_verify_with_invalid_signature() {
    let backend = Wallet::generate();
    let message = "This is a message";

    // Signature générée à partir d’un autre message
    let bad_signature = backend.sign("other message").unwrap();

    let result = backend.verify(message, &bad_signature);

    assert!(
        matches!(result, Ok(false)),
        "Invalid signature should return false"
    );
}

#[test]
fn test_sign_with_invalid_private_key_format() {
    let mut backend = Wallet::generate();
    backend.private_key_b64 = "invalid-base64".to_string();

    let result = backend.sign("msg");

    assert!(matches!(result, Err(SignError::Base64Decode)));
}

#[test]
fn test_verify_with_invalid_pubkey_format() {
    let mut wallet = Wallet::generate();

    // 🔐 Signature valide en DER + base64
    let signature = wallet.sign("message").expect("signature");

    // 🧨 Clé publique non-hexadécimale
    wallet.public_key_hex = "zz!!@@".to_string();

    let result = wallet.verify("message", &signature);

    println!("Résultat = {:?}", result);
    assert!(matches!(result, Err(VerifyError::HexDecode)));
}

#[test]
fn test_from_mnemonic_sign_verify() {
    // Génère un wallet avec les 24 mots inclus
    let wallet = Wallet::generate();

    // Récupère la liste des mots
    let words = wallet
        .mnemonic_words()
        .expect("mnemonic words should be present");

    let word_refs: Vec<&str> = words.iter().map(|s| *s).collect(); // <- pas de .as_str() car déjà &str

    // Restaure un nouveau wallet depuis cette liste
    let recovered = Wallet::from_word_list(&word_refs).expect("restore from word list");


    // Vérifie que les clés sont identiques
    assert_eq!(wallet.public_key_hex, recovered.public_key_hex);
    assert_eq!(wallet.private_key_b64, recovered.private_key_b64);

    // Signature
    let message = "hello dag!";
    let signature = recovered.sign(message).expect("sign");

    // Vérification
    let valid = recovered.verify(message, &signature).expect("verify");
    assert!(valid, "signature should be valid after restore");
}