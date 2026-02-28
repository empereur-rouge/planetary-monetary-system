use anyhow::Result;
use pms_storage::{DagStorage, StoredBlock};
use pms_testkit::test_rocks_store_with_limit;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_types::TxOutput;
use tokio::time::{Duration, sleep};

fn sb_with_payload(id: &str, payload: PlainPayload) -> StoredBlock {
    let env = PayloadEnvelope::Plain(payload);
    StoredBlock {
        id: id.into(),
        parents: vec![],
        payload_json: Some(serde_json::to_string(&env).unwrap()),
        nonce: 0,
        network_id: "".to_string(),
        protocol_version: 0,
        signer_pk_hex: "".to_string(),
        signature_hex: "".to_string(),
        metadata: None,
    }
}

fn mint_payload(addr: &str, amount: &str) -> PlainPayload {
    PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: addr.into(),
            amount: amount.into(),
            asset_id: None,
        }],
    }
}

fn reward_payload(addr: &str) -> PlainPayload {
    PlainPayload::Reward {
        fee_outputs: vec![TxOutput {
            address: addr.into(),
            amount: "1.0".into(),
            asset_id: None,
        }],
        reward_outputs: vec![],
        burned: "0.05".into(),
        tx_block_id: "txblk".into(),
    }
}

#[tokio::test]
async fn addr_activity_indexes_mint_blocks() -> Result<()> {
    let store = test_rocks_store_with_limit("addr-act-mint", 64).await?;

    let b1 = sb_with_payload("blk1", mint_payload("alice", "100"));
    store.append_block_atomic_with_utxo(&b1, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b2 = sb_with_payload("blk2", mint_payload("bob", "50"));
    store.append_block_atomic_with_utxo(&b2, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b3 = sb_with_payload("blk3", mint_payload("alice", "200"));
    store.append_block_atomic_with_utxo(&b3, None).await?;

    // Alice should have 2 blocks
    let (alice_ids, _) = store.recent_ids_by_address("alice", None, None, 100).await?;
    assert_eq!(alice_ids.len(), 2, "alice should have 2 activity entries");
    // newest first
    assert_eq!(alice_ids[0], "blk3");
    assert_eq!(alice_ids[1], "blk1");

    // Bob should have 1 block
    let (bob_ids, _) = store.recent_ids_by_address("bob", None, None, 100).await?;
    assert_eq!(bob_ids.len(), 1);
    assert_eq!(bob_ids[0], "blk2");

    // Carol should have 0 blocks
    let (carol_ids, _) = store.recent_ids_by_address("carol", None, None, 100).await?;
    assert!(carol_ids.is_empty());

    Ok(())
}

#[tokio::test]
async fn addr_activity_indexes_reward_blocks() -> Result<()> {
    let store = test_rocks_store_with_limit("addr-act-reward", 64).await?;

    let b1 = sb_with_payload("rblk1", reward_payload("coordinator"));
    store.append_block_atomic_with_utxo(&b1, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b2 = sb_with_payload("rblk2", reward_payload("coordinator"));
    store.append_block_atomic_with_utxo(&b2, None).await?;

    let (ids, _) = store.recent_ids_by_address("coordinator", None, None, 100).await?;
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], "rblk2");
    assert_eq!(ids[1], "rblk1");

    Ok(())
}

#[tokio::test]
async fn addr_activity_pagination() -> Result<()> {
    let store = test_rocks_store_with_limit("addr-act-page", 64).await?;

    // Insert 5 blocks for alice
    for i in 0..5 {
        let id = format!("pblk{}", i);
        let b = sb_with_payload(&id, mint_payload("alice", "10"));
        store.append_block_atomic_with_utxo(&b, None).await?;
        sleep(Duration::from_millis(5)).await;
    }

    // Fetch first page (limit 2)
    let (page1, cursor1) = store.recent_ids_by_address("alice", None, None, 2).await?;
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0], "pblk4"); // newest
    assert_eq!(page1[1], "pblk3");
    assert!(cursor1.is_some(), "should have more pages");

    // Fetch second page using cursor
    let (ts, id, has_more) = cursor1.unwrap();
    assert!(has_more);
    let (page2, cursor2) = store.recent_ids_by_address("alice", Some(ts), Some(id), 2).await?;
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0], "pblk2");
    assert_eq!(page2[1], "pblk1");

    // Fetch third page
    let (ts, id, _) = cursor2.unwrap();
    let (page3, cursor3) = store.recent_ids_by_address("alice", Some(ts), Some(id), 2).await?;
    assert_eq!(page3.len(), 1);
    assert_eq!(page3[0], "pblk0");
    assert!(cursor3.is_none(), "no more pages");

    Ok(())
}

#[tokio::test]
async fn addr_activity_no_index_for_genesis() -> Result<()> {
    let store = test_rocks_store_with_limit("addr-act-genesis", 64).await?;

    // Genesis has no involved addresses
    let env = PayloadEnvelope::Plain(PlainPayload::Genesis);
    let b = StoredBlock {
        id: "gen".into(),
        parents: vec![],
        payload_json: Some(serde_json::to_string(&env).unwrap()),
        nonce: 0,
        network_id: "".to_string(),
        protocol_version: 0,
        signer_pk_hex: "".to_string(),
        signature_hex: "".to_string(),
        metadata: None,
    };
    store.append_block_atomic_with_utxo(&b, None).await?;

    // No address should have any activity
    let (ids, _) = store.recent_ids_by_address("alice", None, None, 100).await?;
    assert!(ids.is_empty());

    Ok(())
}

#[tokio::test]
async fn addr_activity_multi_address_in_one_block() -> Result<()> {
    let store = test_rocks_store_with_limit("addr-act-multi", 64).await?;

    // A mint block with outputs for both alice and bob
    let payload = PlainPayload::Mint {
        outputs: vec![
            TxOutput { address: "alice".into(), amount: "50".into(), asset_id: None },
            TxOutput { address: "bob".into(), amount: "50".into(), asset_id: None },
        ],
    };
    let b = sb_with_payload("multi1", payload);
    store.append_block_atomic_with_utxo(&b, None).await?;

    // Both should find the block
    let (alice_ids, _) = store.recent_ids_by_address("alice", None, None, 100).await?;
    assert_eq!(alice_ids, vec!["multi1"]);

    let (bob_ids, _) = store.recent_ids_by_address("bob", None, None, 100).await?;
    assert_eq!(bob_ids, vec!["multi1"]);

    Ok(())
}
