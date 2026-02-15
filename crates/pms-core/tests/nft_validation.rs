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

    let result = validate_nft_action(&mint, signer, None, &store);
    assert!(result.is_ok());
}

#[test]
fn test_mint_already_exists() {
    let store = InMemoryNftStore::new();
    let signer = "creator_pubkey";

    store.set_owner("nft-001", "owner").unwrap();

    let mint = NftAction::Mint {
        token_id: "nft-001".into(),
        creator: signer.into(),
        metadata: NftMetadata::default(),
    };

    let result = validate_nft_action(&mint, signer, None, &store);
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

    let result = validate_nft_action(&mint, signer, None, &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Unauthorized"));
}

#[test]
fn test_transfer_success() {
    let store = InMemoryNftStore::new();
    let owner = "owner_pubkey";
    let new_owner = "new_owner";

    store.set_owner("nft-001", owner).unwrap();

    let transfer = NftAction::Transfer {
        token_id: "nft-001".into(),
        from: owner.into(),
        to: new_owner.into(),
        encrypted_metadata: None,
        new_owner_x25519_pubkey: None,
    };

    let result = validate_nft_action(&transfer, owner, None, &store);
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
        from: attacker.into(),
        to: "someone".into(),
        encrypted_metadata: None,
        new_owner_x25519_pubkey: None,
    };

    let result = validate_nft_action(&transfer, attacker, None, &store);
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

    let result = validate_nft_action(&burn, owner, None, &store);
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

    let result = validate_nft_action(&use_action, owner, None, &store);
    assert!(result.is_ok());
}

#[test]
fn test_token_not_found() {
    let store = InMemoryNftStore::new();

    let burn = NftAction::Burn {
        token_id: "nonexistent".into(),
        burner: "someone".into(),
    };

    let result = validate_nft_action(&burn, "someone", None, &store);
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
    let result = validate_nft_action(&mint, attacker, Some(coordinator), &store);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Unauthorized"));

    // Case 2: Coordinator Key is Set -> Coordinator succeeds
    let mint_ok = NftAction::Mint {
        token_id: "nft-valid".into(),
        creator: coordinator.into(),
        metadata: NftMetadata::default(),
    };
    let result_ok = validate_nft_action(&mint_ok, coordinator, Some(coordinator), &store);
    assert!(result_ok.is_ok());

    // Case 3: No Coordinator Key (Dev Mode) -> Attacker succeeds
    let result_dev = validate_nft_action(&mint, attacker, None, &store);
    assert!(result_dev.is_ok());
}
