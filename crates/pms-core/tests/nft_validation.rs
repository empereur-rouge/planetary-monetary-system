//! Tests d'intégration pour la validation NFT.

use pms_core::validations::nft::validate_nft_action;
use pms_storage::NftStorage; // Import trait pour accéder aux méthodes
use pms_storage::nft_store::mock::InMemoryNftStore;
use pms_types_nft::{NftAction, NftMetadata};

#[test]
fn test_mint_success() {
    let store = InMemoryNftStore::new();
    let signer = "creator_pubkey";

    let mint = NftAction::Mint {
        token_id: "nft-001".into(),
        creator: signer.into(),
        metadata: NftMetadata::default(),
    };

    // Le mint devrait réussir (token n'existe pas)
    // Mode Dev (pas de coordinator_pk ni authority_pk)
    let result = validate_nft_action(&mint, signer, None, &[], &store);
    assert!(result.is_ok());
}

#[test]
fn test_mint_already_exists() {
    let store = InMemoryNftStore::new();
    let signer = "creator_pubkey";

    // Pré-enregistre le token
    store.set_owner("nft-001", "owner").unwrap();

    let mint = NftAction::Mint {
        token_id: "nft-001".into(),
        creator: signer.into(),
        metadata: NftMetadata::default(),
    };

    // Le mint devrait échouer (token existe déjà)
    let result = validate_nft_action(&mint, signer, None, &[], &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}

#[test]
fn test_mint_unauthorized_signer() {
    let store = InMemoryNftStore::new();
    let signer = "wrong_signer";

    let mint = NftAction::Mint {
        token_id: "nft-001".into(),
        creator: "actual_creator".into(),
        metadata: NftMetadata::default(),
    };

    // Le mint devrait échouer (signer != creator)
    let result = validate_nft_action(&mint, signer, None, &[], &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Unauthorized"));
}

#[test]
fn test_transfer_success() {
    let store = InMemoryNftStore::new();
    let owner = "owner_pubkey";
    let new_owner = "new_owner";

    // Enregistre le NFT avec le owner
    store.set_owner("nft-001", owner).unwrap();

    let transfer = NftAction::Transfer {
        token_id: "nft-001".into(),
        from: owner.into(),
        to: new_owner.into(),
        encrypted_metadata: None,
        new_owner_x25519_pubkey: None,
    };

    // Le transfer devrait réussir
    let result = validate_nft_action(&transfer, owner, None, &[], &store);
    assert!(result.is_ok());
}

#[test]
fn test_transfer_not_owner() {
    let store = InMemoryNftStore::new();
    let owner = "owner_pubkey";
    let attacker = "attacker";

    store.set_owner("nft-001", owner).unwrap();

    let transfer = NftAction::Transfer {
        token_id: "nft-001".into(),
        from: attacker.into(), // L'attaquant prétend être le owner
        to: "someone".into(),
        encrypted_metadata: None,
        new_owner_x25519_pubkey: None,
    };

    // Devrait échouer (from != owner réel)
    let result = validate_nft_action(&transfer, attacker, None, &[], &store);
    assert!(result.is_err());
}

#[test]
fn test_burn_success() {
    let store = InMemoryNftStore::new();
    let owner = "owner_pubkey";

    store.set_owner("nft-001", owner).unwrap();

    let burn = NftAction::Burn {
        token_id: "nft-001".into(),
        burner: owner.into(),
    };

    let result = validate_nft_action(&burn, owner, None, &[], &store);
    assert!(result.is_ok());
}

#[test]
fn test_use_success() {
    let store = InMemoryNftStore::new();
    let owner = "owner_pubkey";

    store.set_owner("nft-001", owner).unwrap();

    let use_action = NftAction::Use {
        token_id: "nft-001".into(),
        user: owner.into(),
        action_type: "clicker_confirm".into(),
        action_data: Some("10 clicks".into()),
    };

    let result = validate_nft_action(&use_action, owner, None, &[], &store);
    assert!(result.is_ok());
}

#[test]
fn test_token_not_found() {
    let store = InMemoryNftStore::new();
    // Pas de token enregistré

    let burn = NftAction::Burn {
        token_id: "nonexistent".into(),
        burner: "someone".into(),
    };

    let result = validate_nft_action(&burn, "someone", None, &[], &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn test_mint_restricted_to_coordinator() {
    let store = InMemoryNftStore::new();
    let coordinator = "coordinator_pk";
    let attacker = "attacker_pk";

    let mint = NftAction::Mint {
        token_id: "nft-restricted".into(),
        creator: attacker.into(),
        metadata: NftMetadata::default(),
    };

    // Case 1: Coordinator Key is Set -> Attacker fails
    let result = validate_nft_action(&mint, attacker, Some(coordinator), &[], &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Unauthorized"));

    // Case 2: Coordinator Key is Set -> Coordinator succeeds
    // Coordinator mints for themselves
    let mint_ok = NftAction::Mint {
        token_id: "nft-valid".into(),
        creator: coordinator.into(),
        metadata: NftMetadata::default(),
    };
    let result_ok = validate_nft_action(&mint_ok, coordinator, Some(coordinator), &[], &store);
    assert!(result_ok.is_ok());

    // Case 3: No Coordinator Key (Dev Mode) -> Attacker succeeds (with warning logic)
    let result_dev = validate_nft_action(&mint, attacker, None, &[], &store);
    assert!(result_dev.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests de validation Cube Authority
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cube_mint_without_authority_rejects_if_configured() {
    let store = InMemoryNftStore::new();
    let signer = "coordinator_pk";
    let authority = "04abcd1234"; // Fake authority key

    // Cube sans signature dans extra
    let metadata = NftMetadata {
        name: Some("Fake Cube".into()),
        nft_type: Some("cube".into()),
        extra: Some(
            r#"{"rarity":"Common","attributes":{"weight":50,"size":50,"density":50},"roll":123}"#
                .into(),
        ),
        ..Default::default()
    };

    let mint = NftAction::Mint {
        token_id: "cube-001".into(),
        creator: signer.into(),
        metadata,
    };

    // Avec authority configurée -> doit échouer (pas de signature)
    let result = validate_nft_action(&mint, signer, None, &[authority.to_string()], &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("signature"));
}

#[test]
fn test_cube_mint_without_authority_succeeds_in_dev_mode() {
    let store = InMemoryNftStore::new();
    let signer = "creator_pk";

    // Cube sans signature
    let metadata = NftMetadata {
        name: Some("Dev Cube".into()),
        nft_type: Some("cube".into()),
        extra: Some(
            r#"{"rarity":"Common","attributes":{"weight":50,"size":50,"density":50},"roll":123}"#
                .into(),
        ),
        ..Default::default()
    };

    let mint = NftAction::Mint {
        token_id: "cube-dev".into(),
        creator: signer.into(),
        metadata,
    };

    // Sans authority configurée (Dev mode) -> doit réussir avec warning
    let result = validate_nft_action(&mint, signer, None, &[], &store);
    assert!(result.is_ok());
}

#[test]
fn test_non_cube_nft_ignores_authority_validation() {
    let store = InMemoryNftStore::new();
    let signer = "creator_pk";
    let authority = "04abcd1234";

    // NFT collectible (pas un cube)
    let metadata = NftMetadata {
        name: Some("Cool Collectible".into()),
        nft_type: Some("collectible".into()),
        extra: None,
        ..Default::default()
    };

    let mint = NftAction::Mint {
        token_id: "collectible-001".into(),
        creator: signer.into(),
        metadata,
    };

    // Même avec authority configurée, un non-cube passe sans validation signature
    let result = validate_nft_action(&mint, signer, None, &[authority.to_string()], &store);
    assert!(result.is_ok());
}

#[test]
fn test_cube_mint_with_valid_authority_signature_succeeds() {
    use base64::Engine;
    use base64::engine::general_purpose;
    use k256::ecdsa::{SigningKey, signature::Signer};

    let store = InMemoryNftStore::new();
    let signer = "coordinator_pk";

    // 1. Générer une vraie paire de clés Authority
    // Utilise OsRng qui est compatible avec k256
    let authority_secret = SigningKey::random(&mut rand_core::OsRng);
    let authority_public = authority_secret.verifying_key();
    let authority_pk_hex = hex::encode(authority_public.to_sec1_bytes());

    // 2. Définir les attributs du Cube
    let weight = 50u32;
    let size = 75u32;
    let density = 25u32;

    // 3. Construire le message canonique (même format que burn_refund.rs)
    let message = format!("weight:{},size:{},density:{}", weight, size, density);

    // 4. Signer le message
    let signature: k256::ecdsa::Signature = authority_secret.sign(message.as_bytes());
    let signature_b64 = general_purpose::STANDARD.encode(signature.to_der());

    // 5. Construire les métadonnées avec la signature
    let extra = serde_json::json!({
        "rarity": "Rare",
        "attributes": {
            "weight": weight,
            "size": size,
            "density": density
        },
        "roll": 12345,
        "signature": signature_b64
    });

    let metadata = NftMetadata {
        name: Some("Valid Cube".into()),
        nft_type: Some("cube".into()),
        extra: Some(extra.to_string()),
        ..Default::default()
    };

    let mint = NftAction::Mint {
        token_id: "cube-valid".into(),
        creator: signer.into(),
        metadata,
    };

    // 6. Le mint doit réussir avec une signature valide
    let result = validate_nft_action(&mint, signer, None, &[authority_pk_hex.clone()], &store);
    assert!(
        result.is_ok(),
        "Expected success but got: {:?}",
        result.err()
    );
}
