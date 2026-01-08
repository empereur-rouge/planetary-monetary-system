//! Tests d'intégration pour pms-types-nft

use pms_types_nft::{Nft, NftAction, NftMetadata};

#[test]
fn test_nft_creation() {
    let nft = Nft::new("nft-001".into(), "8e1owner".into(), "8e1creator".into());

    assert_eq!(nft.token_id, "nft-001");
    assert_eq!(nft.owner, "8e1owner");
    assert!(nft.metadata.name.is_none());
}

#[test]
fn test_nft_with_metadata() {
    let nft = Nft::with_metadata(
        "nft-002".into(),
        "8e1owner".into(),
        "8e1creator".into(),
        NftMetadata {
            name: Some("Test NFT".into()),
            nft_type: Some("clicker".into()),
            ..Default::default()
        },
    );

    assert_eq!(nft.metadata.nft_type, Some("clicker".into()));
}

#[test]
fn test_nft_serialization() {
    let nft = Nft::with_metadata(
        "nft-003".into(),
        "8e1owner".into(),
        "8e1creator".into(),
        NftMetadata {
            name: Some("Serialization Test".into()),
            nft_type: Some("ticket".into()),
            ..Default::default()
        },
    );

    let json = serde_json::to_string(&nft).unwrap();
    assert!(json.contains("ticket"));

    let parsed: Nft = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.metadata.nft_type, Some("ticket".into()));
}

#[test]
fn test_action_types() {
    let mint = NftAction::Mint {
        token_id: "t1".into(),
        creator: "c1".into(),
        metadata: NftMetadata::default(),
    };
    assert_eq!(mint.action_type_str(), "mint");
    assert_eq!(mint.token_id(), "t1");

    let use_action = NftAction::Use {
        token_id: "t2".into(),
        user: "u1".into(),
        action_type: "clicker_confirm".into(),
        action_data: Some("10 clicks".into()),
    };
    assert_eq!(use_action.action_type_str(), "use");

    let transfer = NftAction::Transfer {
        token_id: "t3".into(),
        from: "from-addr".into(),
        to: "to-addr".into(),
    };
    assert_eq!(transfer.action_type_str(), "transfer");

    let burn = NftAction::Burn {
        token_id: "t4".into(),
        burner: "burner-addr".into(),
    };
    assert_eq!(burn.action_type_str(), "burn");
}
