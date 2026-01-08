//! Tests d'intégration pour pms-event

use pms_event::{EventBus, PmsEvent};
use pms_types_nft::{NftAction, NftMetadata};

#[test]
fn test_nft_event_types() {
    let mint_action = NftAction::Mint {
        token_id: "t1".into(),
        creator: "addr".into(),
        metadata: NftMetadata::default(),
    };

    let event = PmsEvent::nft("block-123".into(), mint_action);
    assert_eq!(event.event_type(), "nft_minted");
    assert_eq!(event.block_id(), "block-123");
}

#[test]
fn test_event_serialization() {
    let use_action = NftAction::Use {
        token_id: "t2".into(),
        user: "user-addr".into(),
        action_type: "clicker_confirm".into(),
        action_data: Some("10 clicks".into()),
    };

    let event = PmsEvent::nft("block-456".into(), use_action);

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("clicker_confirm"));

    let parsed: PmsEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.event_type(), "nft_used");
}

#[tokio::test]
async fn test_bus_emit_and_receive() {
    let bus = EventBus::new(16);
    let mut rx = bus.subscribe();

    bus.emit(PmsEvent::BlockAdded {
        block_id: "test-block".into(),
    });

    let received = rx.recv().await.unwrap();
    assert_eq!(received.block_id(), "test-block");
}

#[tokio::test]
async fn test_bus_multiple_subscribers() {
    let bus = EventBus::new(16);
    let mut rx1 = bus.subscribe();
    let mut rx2 = bus.subscribe();

    bus.emit(PmsEvent::nft(
        "b1".into(),
        NftAction::Burn {
            token_id: "nft-42".into(),
            burner: "addr".into(),
        },
    ));

    let e1 = rx1.recv().await.unwrap();
    let e2 = rx2.recv().await.unwrap();

    assert_eq!(e1.block_id(), e2.block_id());
}

#[test]
fn test_bus_clone_shares_channel() {
    let bus1 = EventBus::new(16);
    let _rx = bus1.subscribe();

    let bus2 = bus1.clone();

    // Les deux bus partagent le même canal
    assert_eq!(bus1.subscriber_count(), 1);
    assert_eq!(bus2.subscriber_count(), 1);
}
