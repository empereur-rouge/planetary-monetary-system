//! Tests de sécurité du Minting
//!
//! Ces tests vérifient que SEUL le Coordinateur peut créer des tokens (Mint).
//! C'est une fonctionnalité critique pour la sécurité économique du réseau.

use pms_core::validations::check::ValidatePolicy;
use pms_core::validations::mint::validate_mint_security_logic;
use pms_errors::ValidationError;
use rust_decimal::Decimal;

/// Crée une politique de test avec une clé coordinateur spécifique
fn make_policy_with_coordinator(coord_key: Option<&str>) -> ValidatePolicy {
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

/// Teste que le Coordinateur peut minter
#[test]
fn test_coordinator_can_mint() {
    // La clé du Coordinateur (celle dans pms-consensus)
    let coord_key = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
    let policy = make_policy_with_coordinator(Some(coord_key));

    // Le Coordinateur signe un bloc
    let result = validate_mint_security_logic(coord_key, "block-123", &policy);

    // Doit réussir
    assert!(result.is_ok(), "Le Coordinateur devrait pouvoir minter");
}

/// Teste que la comparaison de clé est insensible à la casse
#[test]
fn test_coordinator_key_case_insensitive() {
    // Clé en minuscules dans la policy
    let coord_key_lower = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
    let policy = make_policy_with_coordinator(Some(coord_key_lower));

    // Clé en majuscules dans le bloc
    let coord_key_upper = "036ED4D5AD1C927FE972EF9728AC1888D237AF57A488B6CBE50228FAC442B5AE6B";
    let result = validate_mint_security_logic(coord_key_upper, "block-456", &policy);

    // Doit réussir (comparaison insensible à la casse)
    assert!(
        result.is_ok(),
        "La comparaison de clé devrait être insensible à la casse"
    );
}

/// Teste qu'une clé aléatoire ne peut PAS minter
#[test]
fn test_random_key_cannot_mint() {
    // La vraie clé Coordinateur
    let coord_key = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
    let policy = make_policy_with_coordinator(Some(coord_key));

    // Une clé aléatoire différente
    let random_key = "02abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab";
    let result = validate_mint_security_logic(random_key, "block-789", &policy);

    // Doit échouer avec UnauthorizedMint
    assert!(
        result.is_err(),
        "Une clé aléatoire ne devrait PAS pouvoir minter"
    );

    match result.unwrap_err() {
        ValidationError::UnauthorizedMint { id, signer_pk_hex } => {
            assert_eq!(id, "block-789");
            assert_eq!(signer_pk_hex, random_key);
        }
        other => panic!("Mauvais type d'erreur: {:?}", other),
    }
}

/// Teste qu'un signataire vide ne peut PAS minter
#[test]
fn test_empty_signer_cannot_mint() {
    let coord_key = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
    let policy = make_policy_with_coordinator(Some(coord_key));

    // Signature vide
    let result = validate_mint_security_logic("", "block-empty", &policy);

    // Doit échouer
    assert!(
        result.is_err(),
        "Une signature vide ne devrait PAS pouvoir minter"
    );
}

/// Teste que des espaces blancs ne permettent pas de minter
#[test]
fn test_whitespace_signer_cannot_mint() {
    let coord_key = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
    let policy = make_policy_with_coordinator(Some(coord_key));

    // Signature avec espaces seulement
    let result = validate_mint_security_logic("   \n\t  ", "block-whitespace", &policy);

    // Doit échouer (après trim, c'est vide)
    assert!(
        result.is_err(),
        "Des espaces ne devraient PAS pouvoir minter"
    );
}

/// Teste qu'en mode Dev (sans clé configurée), le mint est autorisé
#[test]
fn test_dev_mode_allows_mint_without_coordinator_key() {
    // Pas de clé Coordinateur configurée (Mode Dev)
    let policy = make_policy_with_coordinator(None);

    // N'importe quelle clé
    let any_key = "02anything1234567890abcdef";
    let result = validate_mint_security_logic(any_key, "block-dev", &policy);

    // En mode Dev, on autorise (mais avec warning)
    assert!(
        result.is_ok(),
        "En mode Dev (pas de clé configurée), le mint devrait être autorisé"
    );
}

/// Teste qu'une clé très similaire mais différente d'un caractère échoue
#[test]
fn test_similar_but_different_key_fails() {
    let coord_key = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
    let policy = make_policy_with_coordinator(Some(coord_key));

    // Clé très similaire mais différente d'un caractère (dernier char différent)
    let similar_key = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6c";
    let result = validate_mint_security_logic(similar_key, "block-similar", &policy);

    // Doit échouer
    assert!(
        result.is_err(),
        "Une clé différente d'un seul caractère ne devrait PAS passer"
    );
}
