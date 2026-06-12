//! Tests d'application du mode Single Writer (Private DAG)
//!
//! Ces tests vérifient que le mode `enforce_single_writer`:
//! 1. Rejette les blocs non signés par le Coordinateur
//! 2. Rejette les blocs avec plus d'un parent
//! 3. Accepte les blocs valides (Coordinateur + 1 parent)
//!
//! Voir: The Rust Programming Language, Chapitre 11 - Writing Automated Tests

use pms_core::validations::authority::validate_payload_authority;
use pms_core::validations::check::ValidatePolicy;
use pms_core::validations::parents::enforce_single_parent;
use pms_errors::ValidationError;
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;

// ============================================================================
// TEST 1: Bloc avec 1 parent accepté (mode Single Writer)
// ============================================================================

/// En mode Single Writer, un bloc avec exactement 1 parent est valide.
#[test]
fn test_single_parent_accepted() {
    // Un bloc avec exactement 1 parent
    let block = Block {
        id: "block-1".to_string(),
        parents: vec!["genesis".to_string()], // 1 parent
        payload: None,
        nonce: 0,
        metadata: None,
        signer_pk: None,
        signature: None,
    };

    // Vérification: non-genesis, donc strictement 1 parent requis
    let result = enforce_single_parent(&block, false);

    assert!(
        result.is_ok(),
        "Bloc avec 1 parent doit être accepté, got: {:?}",
        result
    );
}

// ============================================================================
// TEST 2: Bloc avec plusieurs parents rejeté
// ============================================================================

/// En mode Single Writer, un bloc avec plus d'un parent doit être rejeté.
#[test]
fn test_multiple_parents_rejected() {
    // Un bloc avec 2 parents (comme un DAG classique)
    let block = Block {
        id: "block-2".to_string(),
        parents: vec!["parent-a".to_string(), "parent-b".to_string()], // 2 parents
        payload: None,
        nonce: 0,
        metadata: None,
        signer_pk: None,
        signature: None,
    };

    // Vérification: non-genesis, donc strictement 1 parent requis
    let result = enforce_single_parent(&block, false);

    // Doit être rejeté
    assert!(
        result.is_err(),
        "Bloc avec 2 parents doit être REJETÉ en mode Single Writer"
    );

    // Vérifier le type d'erreur
    match result.unwrap_err() {
        ValidationError::TooManyParents(msg) => {
            assert!(
                msg.contains("1 parent"),
                "Message d'erreur doit mentionner '1 parent', got: {}",
                msg
            );
        }
        other => panic!("Mauvais type d'erreur: {:?}", other),
    }
}

// ============================================================================
// TEST 3: Bloc avec 0 parents rejeté (sauf genesis)
// ============================================================================

/// En mode Single Writer, un bloc non-genesis avec 0 parents doit être rejeté.
#[test]
fn test_zero_parents_rejected_if_not_genesis() {
    // Un bloc sans parents (mais ce n'est pas le genesis)
    let block = Block {
        id: "orphan-block".to_string(),
        parents: vec![], // 0 parents
        payload: None,   // Pas de payload Genesis
        nonce: 0,
        metadata: None,
        signer_pk: None,
        signature: None,
    };

    // Vérification: non-genesis, donc strictement 1 parent requis
    let result = enforce_single_parent(&block, false);

    // Doit être rejeté
    assert!(
        result.is_err(),
        "Bloc non-genesis avec 0 parents doit être REJETÉ"
    );
}

// ============================================================================
// TEST 4: Genesis avec 0 parents accepté
// ============================================================================

/// Le genesis (is_genesis = true) doit avoir 0 parents.
#[test]
fn test_genesis_zero_parents_accepted() {
    let genesis = Block::genesis(compute_block_id);

    // Vérification: c'est un genesis, donc 0 parents attendu
    let result = enforce_single_parent(&genesis, true);

    assert!(
        result.is_ok(),
        "Genesis avec 0 parents doit être accepté, got: {:?}",
        result
    );
}

// ============================================================================
// TEST 5: Genesis avec parents rejeté
// ============================================================================

/// Un genesis qui aurait des parents doit être rejeté.
#[test]
fn test_genesis_with_parents_rejected() {
    // Un "faux" genesis avec des parents
    let fake_genesis = Block {
        id: "genesis".to_string(),
        parents: vec!["parent".to_string()], // ERREUR: genesis ne doit pas avoir de parents
        payload: Some(PayloadEnvelope::Plain(PlainPayload::Genesis)),
        nonce: 0,
        metadata: None,
        signer_pk: None,
        signature: None,
    };

    // Vérification: c'est un genesis, donc 0 parents attendu
    let result = enforce_single_parent(&fake_genesis, true);

    // Doit être rejeté car genesis ne doit pas avoir de parents
    assert!(result.is_err(), "Genesis avec parents doit être REJETÉ");

    // Vérifier le type d'erreur
    match result.unwrap_err() {
        ValidationError::InvalidGenesis(msg) => {
            assert!(
                msg.contains("parent"),
                "Message d'erreur doit mentionner 'parent', got: {}",
                msg
            );
        }
        other => panic!("Mauvais type d'erreur pour genesis invalide: {:?}", other),
    }
}

// ============================================================================
// TEST 6: Autorité Coordinateur (vrai code de prod — validate_payload_authority)
// ============================================================================

/// Construit une policy avec une clé coordinateur donnée.
fn policy_with_coord(pk: &str) -> ValidatePolicy {
    let mut p = ValidatePolicy::default();
    p.coordinator_public_key = Some(pk.to_string());
    p
}

/// Un payload réservé au Coordinateur (Freeze) pour exercer le gate d'autorité.
fn coordinator_only_payload() -> PayloadEnvelope {
    PayloadEnvelope::Plain(PlainPayload::Freeze {
        address: "8e1victimaddr".to_string(),
        reason: "compliance test".to_string(),
    })
}

/// Vérifie le VRAI code d'autorité coordinateur (`validate_payload_authority`),
/// pas une ré-implémentation. La comparaison de clé est **case-insensitive**
/// (la prod utilise `eq_ignore_ascii_case`, cf. `validations/authority.rs:36`).
///
/// CRITICAL: l'ancienne version de ce test (v0.9.2) comparait deux littéraux de
/// chaîne entre eux (tautologique, n'appelait aucun code de prod) ET affirmait à
/// tort que la comparaison était case-SENSITIVE — l'inverse du comportement réel.
/// Un faux test qui documentait le mauvais comportement.
#[test]
fn test_coordinator_authority_is_case_insensitive() {
    let canonical = "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
    let same_upper = "036ED4D5AD1C927FE972EF9728AC1888D237AF57A488B6CBE50228FAC442B5AE6B";
    let attacker = "02abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab";

    let policy = policy_with_coord(canonical);
    let payload = coordinator_only_payload();

    // 1. Signataire = clé canonique exacte → accepté.
    let exact = validate_payload_authority(Some(canonical), Some(&payload), &policy);
    println!("exact-key Freeze        → {exact:?}");
    assert!(exact.is_ok(), "exact coordinator key must be accepted");

    // 2. Même clé en MAJUSCULES → accepté (case-insensitive — comportement réel).
    let upper = validate_payload_authority(Some(same_upper), Some(&payload), &policy);
    println!("uppercase-key Freeze    → {upper:?}");
    assert!(
        upper.is_ok(),
        "uppercase coordinator key MUST be accepted (prod uses eq_ignore_ascii_case)"
    );

    // 3. Clé d'attaquant → rejeté avec InvalidSignature.
    let foreign = validate_payload_authority(Some(attacker), Some(&payload), &policy);
    println!("attacker-key Freeze     → {foreign:?}");
    let err = format!("{:?}", foreign.expect_err("attacker key must be rejected"));
    assert!(
        err.contains("InvalidSignature"),
        "attacker rejection must be InvalidSignature, got: {err}"
    );

    // 4. Bloc non signé (signer_pk = None) sur un payload coordinator-only → rejeté.
    let unsigned = validate_payload_authority(None, Some(&payload), &policy);
    println!("unsigned Freeze         → {unsigned:?}");
    assert!(
        unsigned.is_err(),
        "coordinator-only payload without a signer must be rejected"
    );
}

// ============================================================================
// TEST 7: Nombre de parents exact
// ============================================================================

/// Vérifie que les messages d'erreur contiennent le bon nombre de parents.
#[test]
fn test_error_message_contains_parent_count() {
    // Bloc avec 5 parents (beaucoup trop)
    let block = Block {
        id: "block-multi".to_string(),
        parents: vec![
            "p1".to_string(),
            "p2".to_string(),
            "p3".to_string(),
            "p4".to_string(),
            "p5".to_string(),
        ],
        payload: None,
        nonce: 0,
        metadata: None,
        signer_pk: None,
        signature: None,
    };

    let result = enforce_single_parent(&block, false);

    match result.unwrap_err() {
        ValidationError::TooManyParents(msg) => {
            assert!(
                msg.contains("5"),
                "Message doit contenir le nombre de parents (5), got: {}",
                msg
            );
        }
        other => panic!("Mauvais type d'erreur: {:?}", other),
    }
}
