//! Tests d'intégration : Sécurité complète du Minting
//!
//! Ces tests vérifient la chaîne de sécurité COMPLÈTE :
//! 1. Signature cryptographique (ECDSA) du bloc
//! 2. Autorisation (le signataire est-il le Coordinateur ?)
//!
//! Un bloc Mint est rejeté si :
//! - La signature ne correspond pas à la clé publique (forgery)
//! - La clé publique n'est pas celle du Coordinateur (unauthorized)

use k256::ecdsa::signature::Verifier;
use k256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use pms_core::validations::check::ValidatePolicy;
use pms_core::validations::mint::validate_mint_security_logic;
use pms_errors::ValidationError;
use rust_decimal::Decimal;

/// Simule un bloc Mint avec signature
struct SignedMintBlock {
    id: String,
    signer_pk_hex: String,
    signature_b64: String,
    message: Vec<u8>,
}

impl SignedMintBlock {
    /// Crée un bloc Mint signé avec la clé donnée
    fn new_signed(id: &str, message: &[u8], signing_key: &SigningKey) -> Self {
        // Dérive la clé publique
        let verifying_key = signing_key.verifying_key();
        let pk_hex = hex::encode(verifying_key.to_sec1_bytes());

        // Signe le message
        let signature: Signature = signing_key.sign(message);
        let sig_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_der().as_bytes(),
        );

        Self {
            id: id.to_string(),
            signer_pk_hex: pk_hex,
            signature_b64: sig_b64,
            message: message.to_vec(),
        }
    }

    /// Vérifie que la signature est cryptographiquement valide
    fn verify_signature(&self) -> Result<(), String> {
        // Décode la clé publique
        let pk_bytes =
            hex::decode(&self.signer_pk_hex).map_err(|e| format!("Invalid pubkey hex: {e}"))?;
        let verifying_key = VerifyingKey::from_sec1_bytes(&pk_bytes)
            .map_err(|e| format!("Invalid SEC1 pubkey: {e}"))?;

        // Décode la signature
        let sig_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &self.signature_b64,
        )
        .map_err(|e| format!("Invalid base64 signature: {e}"))?;
        let signature =
            Signature::from_der(&sig_bytes).map_err(|e| format!("Invalid DER signature: {e}"))?;

        // Vérifie la signature
        verifying_key
            .verify(&self.message, &signature)
            .map_err(|e| format!("Signature verification failed: {e}"))
    }
}

/// Crée une clé à partir d'une seed déterministe (pour les tests)
fn make_key_from_seed(seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_slice(seed).expect("Invalid seed for SigningKey")
}

/// Crée une politique de test avec une clé coordinateur spécifique
fn make_policy(coord_key: Option<&str>) -> ValidatePolicy {
    ValidatePolicy {
        min_pow_leading_zero_bits: 0,
        max_payload_bytes: 1_000_000,
        min_parents_after_boot: 1,
        max_parents: 8,
        require_unique_parents: true,
        forbid_self_parent: true,
        max_inputs: 128,
        max_outputs: 128,
        max_tx_bytes: 100_000,
        max_fee_per_tx: Decimal::from(1_000_000),
        enforce_parent_existence: false,
        enforce_fee_recipient: false,
        allowed_fee_addresses: vec![],
        skip_utxo_checks: false,
        platform_address: None,
        platform_fee_ratio: Decimal::ZERO,
        coordinator_public_key: coord_key.map(String::from),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// TESTS D'INTÉGRATION COMPLETS
// ═══════════════════════════════════════════════════════════════════════════════

/// Test 1: Le Coordinateur peut minter (signature valide + autorisation OK)
#[test]
fn test_coordinator_full_chain_success() {
    // 1. Génère la clé du Coordinateur (déterministe pour reproductibilité)
    let coordinator_seed: [u8; 32] = [1u8; 32];
    let coordinator_key = make_key_from_seed(&coordinator_seed);
    let coordinator_pk_hex = hex::encode(coordinator_key.verifying_key().to_sec1_bytes());

    // 2. Configure la policy avec cette clé
    let policy = make_policy(Some(&coordinator_pk_hex));

    // 3. Crée et signe un bloc Mint
    let message = b"Mint block content hash";
    let block = SignedMintBlock::new_signed("block-coord-001", message, &coordinator_key);

    // 4. Vérifie la signature cryptographique
    let sig_result = block.verify_signature();
    assert!(
        sig_result.is_ok(),
        "La signature devrait être valide: {:?}",
        sig_result
    );

    // 5. Vérifie l'autorisation (est-ce le Coordinateur ?)
    let auth_result = validate_mint_security_logic(&block.signer_pk_hex, &block.id, &policy);
    assert!(auth_result.is_ok(), "Le Coordinateur devrait être autorisé");

    println!("✅ SUCCÈS: Coordinateur signé et autorisé");
}

/// Test 2: Un attaquant avec sa propre clé est rejeté (signature valide MAIS pas autorisé)
#[test]
fn test_attacker_valid_signature_but_unauthorized() {
    // 1. Le vrai Coordinateur
    let coordinator_seed: [u8; 32] = [1u8; 32];
    let coordinator_key = make_key_from_seed(&coordinator_seed);
    let coordinator_pk_hex = hex::encode(coordinator_key.verifying_key().to_sec1_bytes());

    // 2. Un attaquant génère sa propre clé
    let attacker_seed: [u8; 32] = [99u8; 32];
    let attacker_key = make_key_from_seed(&attacker_seed);
    let attacker_pk_hex = hex::encode(attacker_key.verifying_key().to_sec1_bytes());

    // 3. Configure la policy avec la clé du VRAI Coordinateur
    let policy = make_policy(Some(&coordinator_pk_hex));

    // 4. L'attaquant crée un bloc signé avec SA clé
    let message = b"Attacker trying to mint";
    let block = SignedMintBlock::new_signed("block-attack-001", message, &attacker_key);

    // 5. La signature est techniquement valide (il a bien signé avec sa clé)
    let sig_result = block.verify_signature();
    assert!(
        sig_result.is_ok(),
        "La signature de l'attaquant est valide techniquement"
    );

    // 6. MAIS l'autorisation échoue (ce n'est pas le Coordinateur)
    let auth_result = validate_mint_security_logic(&block.signer_pk_hex, &block.id, &policy);
    assert!(
        auth_result.is_err(),
        "L'attaquant NE devrait PAS être autorisé"
    );

    match auth_result.unwrap_err() {
        ValidationError::UnauthorizedMint { signer_pk_hex, .. } => {
            assert_eq!(signer_pk_hex, attacker_pk_hex);
            println!("✅ SUCCÈS: Attaquant rejeté (UnauthorizedMint)");
        }
        other => panic!("Mauvais type d'erreur: {:?}", other),
    }
}

/// Test 3: Une signature forgée est rejetée (clé du Coordinateur MAIS mauvaise signature)
#[test]
fn test_forged_signature_rejected() {
    // 1. Le vrai Coordinateur
    let coordinator_seed: [u8; 32] = [1u8; 32];
    let coordinator_key = make_key_from_seed(&coordinator_seed);
    let coordinator_pk_hex = hex::encode(coordinator_key.verifying_key().to_sec1_bytes());

    // 2. Un attaquant génère sa propre clé
    let attacker_seed: [u8; 32] = [99u8; 32];
    let attacker_key = make_key_from_seed(&attacker_seed);

    // 3. L'attaquant signe un message avec SA clé
    let message = b"Forged mint block";
    let attacker_sig: Signature = attacker_key.sign(message);
    let forged_sig_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        attacker_sig.to_der().as_bytes(),
    );

    // 4. Crée un bloc avec la PK du Coordinateur mais la signature de l'attaquant
    let forged_block = SignedMintBlock {
        id: "block-forged-001".to_string(),
        signer_pk_hex: coordinator_pk_hex.clone(), // Prétend être le Coordinateur
        signature_b64: forged_sig_b64,             // Mais signature de l'attaquant
        message: message.to_vec(),
    };

    // 5. La vérification de signature ÉCHOUE
    let sig_result = forged_block.verify_signature();
    assert!(
        sig_result.is_err(),
        "Une signature forgée devrait être rejetée"
    );

    println!(
        "✅ SUCCÈS: Signature forgée rejetée - {:?}",
        sig_result.unwrap_err()
    );
}

/// Test 4: Message modifié après signature est rejeté (tampering)
#[test]
fn test_tampered_message_rejected() {
    // 1. Le Coordinateur
    let coordinator_seed: [u8; 32] = [1u8; 32];
    let coordinator_key = make_key_from_seed(&coordinator_seed);

    // 2. Crée un bloc signé légitimement
    let original_message = b"Original mint block content";
    let mut block =
        SignedMintBlock::new_signed("block-tamper-001", original_message, &coordinator_key);

    // 3. La signature est valide pour le message original
    assert!(
        block.verify_signature().is_ok(),
        "Signature valide originalement"
    );

    // 4. L'attaquant modifie le message
    block.message = b"Tampered: give me 1 billion tokens".to_vec();

    // 5. La signature ne correspond plus
    let sig_result = block.verify_signature();
    assert!(
        sig_result.is_err(),
        "Un message modifié devrait invalider la signature"
    );

    println!(
        "✅ SUCCÈS: Message modifié rejeté - {:?}",
        sig_result.unwrap_err()
    );
}
