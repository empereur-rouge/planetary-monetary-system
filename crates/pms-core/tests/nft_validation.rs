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
    let result = validate_nft_action(&mint, signer, &store);
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
    let result = validate_nft_action(&mint, signer, &store);
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
    let result = validate_nft_action(&mint, signer, &store);
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
    };

    // Le transfer devrait réussir
    let result = validate_nft_action(&transfer, owner, &store);
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
    };

    // Devrait échouer (from != owner réel)
    let result = validate_nft_action(&transfer, attacker, &store);
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

    let result = validate_nft_action(&burn, owner, &store);
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

    let result = validate_nft_action(&use_action, owner, &store);
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

    let result = validate_nft_action(&burn, "someone", &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}
