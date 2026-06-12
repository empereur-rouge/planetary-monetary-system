use anyhow::Result;
use pms_storage::helpers::ActivityCategory;
use pms_storage::{DagStorage, StoredBlock};
use pms_testkit::test_rocks_store_with_limit;
use pms_types::TxOutput;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
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
    let (alice_ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert_eq!(alice_ids.len(), 2, "alice should have 2 activity entries");
    // newest first
    assert_eq!(alice_ids[0], "blk3");
    assert_eq!(alice_ids[1], "blk1");

    // Bob should have 1 block
    let (bob_ids, _) = store.recent_ids_by_address("bob", None, None, 100).await?;
    assert_eq!(bob_ids.len(), 1);
    assert_eq!(bob_ids[0], "blk2");

    // Carol should have 0 blocks
    let (carol_ids, _) = store
        .recent_ids_by_address("carol", None, None, 100)
        .await?;
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

    let (ids, _) = store
        .recent_ids_by_address("coordinator", None, None, 100)
        .await?;
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
    let (page2, cursor2) = store
        .recent_ids_by_address("alice", Some(ts), Some(id), 2)
        .await?;
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0], "pblk2");
    assert_eq!(page2[1], "pblk1");

    // Fetch third page
    let (ts, id, _) = cursor2.unwrap();
    let (page3, cursor3) = store
        .recent_ids_by_address("alice", Some(ts), Some(id), 2)
        .await?;
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
    let (ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert!(ids.is_empty());

    Ok(())
}

#[tokio::test]
async fn addr_activity_multi_address_in_one_block() -> Result<()> {
    let store = test_rocks_store_with_limit("addr-act-multi", 64).await?;

    // A mint block with outputs for both alice and bob
    let payload = PlainPayload::Mint {
        outputs: vec![
            TxOutput {
                address: "alice".into(),
                amount: "50".into(),
                asset_id: None,
            },
            TxOutput {
                address: "bob".into(),
                amount: "50".into(),
                asset_id: None,
            },
        ],
    };
    let b = sb_with_payload("multi1", payload);
    store.append_block_atomic_with_utxo(&b, None).await?;

    // Both should find the block
    let (alice_ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert_eq!(alice_ids, vec!["multi1"]);

    let (bob_ids, _) = store.recent_ids_by_address("bob", None, None, 100).await?;
    assert_eq!(bob_ids, vec!["multi1"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Per-type activity index tests (addr_type_activity)
// ═══════════════════════════════════════════════════════════════════

fn reward_payload_with_both(fee_addr: &str, reward_addr: &str) -> PlainPayload {
    PlainPayload::Reward {
        fee_outputs: vec![TxOutput {
            address: fee_addr.into(),
            amount: "1.0".into(),
            asset_id: None,
        }],
        reward_outputs: vec![TxOutput {
            address: reward_addr.into(),
            amount: "0.5".into(),
            asset_id: None,
        }],
        burned: "0.05".into(),
        tx_block_id: "txblk".into(),
    }
}

#[tokio::test]
async fn typed_index_filters_by_category() -> Result<()> {
    let store = test_rocks_store_with_limit("typed-filter", 64).await?;

    // Insert a mint block and a reward block for "coord"
    let b1 = sb_with_payload("mint1", mint_payload("coord", "1000"));
    store.append_block_atomic_with_utxo(&b1, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b2 = sb_with_payload("reward1", reward_payload("coord"));
    store.append_block_atomic_with_utxo(&b2, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b3 = sb_with_payload("mint2", mint_payload("coord", "2000"));
    store.append_block_atomic_with_utxo(&b3, None).await?;

    // Filter by Mint category: should only return mint blocks
    let cats = &[ActivityCategory::Mint.as_byte()];
    let (mint_ids, _) = store
        .recent_ids_by_address_and_categories("coord", cats, None, None, 100)
        .await?;
    assert_eq!(mint_ids.len(), 2);
    assert_eq!(mint_ids[0], "mint2");
    assert_eq!(mint_ids[1], "mint1");

    // Filter by Fee category: should only return fee blocks
    let cats = &[ActivityCategory::Fee.as_byte()];
    let (fee_ids, _) = store
        .recent_ids_by_address_and_categories("coord", cats, None, None, 100)
        .await?;
    assert_eq!(fee_ids.len(), 1);
    assert_eq!(fee_ids[0], "reward1");

    // Filter by Reward category: should return nothing (reward_outputs was empty)
    let cats = &[ActivityCategory::Reward.as_byte()];
    let (reward_ids, _) = store
        .recent_ids_by_address_and_categories("coord", cats, None, None, 100)
        .await?;
    assert!(reward_ids.is_empty());

    Ok(())
}

#[tokio::test]
async fn typed_index_reward_fee_split() -> Result<()> {
    let store = test_rocks_store_with_limit("typed-fee-split", 64).await?;

    // Reward block where "coord" gets fee and "validator" gets reward
    let b = sb_with_payload("rblk", reward_payload_with_both("coord", "validator"));
    store.append_block_atomic_with_utxo(&b, None).await?;

    // coord should have Fee category
    let cats = &[ActivityCategory::Fee.as_byte()];
    let (ids, _) = store
        .recent_ids_by_address_and_categories("coord", cats, None, None, 100)
        .await?;
    assert_eq!(ids, vec!["rblk"]);

    // coord should NOT have Reward category
    let cats = &[ActivityCategory::Reward.as_byte()];
    let (ids, _) = store
        .recent_ids_by_address_and_categories("coord", cats, None, None, 100)
        .await?;
    assert!(ids.is_empty());

    // validator should have Reward category
    let cats = &[ActivityCategory::Reward.as_byte()];
    let (ids, _) = store
        .recent_ids_by_address_and_categories("validator", cats, None, None, 100)
        .await?;
    assert_eq!(ids, vec!["rblk"]);

    // validator should NOT have Fee category
    let cats = &[ActivityCategory::Fee.as_byte()];
    let (ids, _) = store
        .recent_ids_by_address_and_categories("validator", cats, None, None, 100)
        .await?;
    assert!(ids.is_empty());

    Ok(())
}

#[tokio::test]
async fn typed_index_multi_category_merge() -> Result<()> {
    let store = test_rocks_store_with_limit("typed-multi-cat", 64).await?;

    // Insert interleaved mint and fee blocks for "coord"
    let b1 = sb_with_payload("mint1", mint_payload("coord", "100"));
    store.append_block_atomic_with_utxo(&b1, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b2 = sb_with_payload("fee1", reward_payload("coord"));
    store.append_block_atomic_with_utxo(&b2, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b3 = sb_with_payload("mint2", mint_payload("coord", "200"));
    store.append_block_atomic_with_utxo(&b3, None).await?;
    sleep(Duration::from_millis(5)).await;

    let b4 = sb_with_payload("fee2", reward_payload("coord"));
    store.append_block_atomic_with_utxo(&b4, None).await?;

    // Query both Mint + Fee categories: should merge newest-first
    let cats = &[
        ActivityCategory::Mint.as_byte(),
        ActivityCategory::Fee.as_byte(),
    ];
    let (ids, _) = store
        .recent_ids_by_address_and_categories("coord", cats, None, None, 100)
        .await?;
    assert_eq!(ids.len(), 4);
    assert_eq!(ids[0], "fee2");
    assert_eq!(ids[1], "mint2");
    assert_eq!(ids[2], "fee1");
    assert_eq!(ids[3], "mint1");

    Ok(())
}

#[tokio::test]
async fn typed_index_pagination() -> Result<()> {
    let store = test_rocks_store_with_limit("typed-page", 64).await?;

    // Insert 5 mint blocks
    for i in 0..5 {
        let id = format!("m{i}");
        let b = sb_with_payload(&id, mint_payload("alice", "10"));
        store.append_block_atomic_with_utxo(&b, None).await?;
        sleep(Duration::from_millis(5)).await;
    }

    let cats = &[ActivityCategory::Mint.as_byte()];

    // Page 1 (limit 2)
    let (page1, cursor1) = store
        .recent_ids_by_address_and_categories("alice", cats, None, None, 2)
        .await?;
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0], "m4");
    assert_eq!(page1[1], "m3");
    assert!(cursor1.is_some());

    // Page 2
    let (ts, id, has_more) = cursor1.unwrap();
    assert!(has_more);
    let (page2, cursor2) = store
        .recent_ids_by_address_and_categories("alice", cats, Some(ts), Some(id), 2)
        .await?;
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0], "m2");
    assert_eq!(page2[1], "m1");

    // Page 3
    let (ts, id, _) = cursor2.unwrap();
    let (page3, cursor3) = store
        .recent_ids_by_address_and_categories("alice", cats, Some(ts), Some(id), 2)
        .await?;
    assert_eq!(page3.len(), 1);
    assert_eq!(page3[0], "m0");
    assert!(cursor3.is_none());

    Ok(())
}

#[tokio::test]
async fn typed_index_empty_categories_returns_empty() -> Result<()> {
    let store = test_rocks_store_with_limit("typed-empty-cat", 64).await?;

    let b = sb_with_payload("blk1", mint_payload("alice", "100"));
    store.append_block_atomic_with_utxo(&b, None).await?;

    // Empty categories slice should return nothing
    let (ids, _) = store
        .recent_ids_by_address_and_categories("alice", &[], None, None, 100)
        .await?;
    assert!(ids.is_empty());

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// write_addr_activity_entries_with_categories (for encrypted payloads)
// ═══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn write_with_categories_indexes_both_cfs() -> Result<()> {
    let store = test_rocks_store_with_limit("write-both-cfs", 64).await?;

    // Simulate what the coordinator does for an encrypted TxUtxo payload:
    // it extracts addresses and categories from the plain payload before encryption.
    let addrs = vec!["alice".to_string(), "bob".to_string(), "admin".to_string()];
    let typed = vec![
        ("alice".to_string(), ActivityCategory::Transfer),
        ("bob".to_string(), ActivityCategory::Transfer),
        ("admin".to_string(), ActivityCategory::Fee),
    ];
    store.write_addr_activity_entries_with_categories("enc_blk1", &addrs, &typed, None)?;

    // Untyped index: all three addresses should find the block
    let (alice_ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert_eq!(alice_ids, vec!["enc_blk1"]);

    let (bob_ids, _) = store.recent_ids_by_address("bob", None, None, 100).await?;
    assert_eq!(bob_ids, vec!["enc_blk1"]);

    let (admin_ids, _) = store
        .recent_ids_by_address("admin", None, None, 100)
        .await?;
    assert_eq!(admin_ids, vec!["enc_blk1"]);

    // Typed index: Transfer category
    let cats = &[ActivityCategory::Transfer.as_byte()];
    let (transfer_alice, _) = store
        .recent_ids_by_address_and_categories("alice", cats, None, None, 100)
        .await?;
    assert_eq!(transfer_alice, vec!["enc_blk1"]);

    let (transfer_bob, _) = store
        .recent_ids_by_address_and_categories("bob", cats, None, None, 100)
        .await?;
    assert_eq!(transfer_bob, vec!["enc_blk1"]);

    // Admin should NOT appear in Transfer category
    let (transfer_admin, _) = store
        .recent_ids_by_address_and_categories("admin", cats, None, None, 100)
        .await?;
    assert!(transfer_admin.is_empty());

    // Typed index: Fee category — only admin
    let cats = &[ActivityCategory::Fee.as_byte()];
    let (fee_admin, _) = store
        .recent_ids_by_address_and_categories("admin", cats, None, None, 100)
        .await?;
    assert_eq!(fee_admin, vec!["enc_blk1"]);

    let (fee_alice, _) = store
        .recent_ids_by_address_and_categories("alice", cats, None, None, 100)
        .await?;
    assert!(fee_alice.is_empty());

    Ok(())
}

#[tokio::test]
async fn write_with_categories_empty_is_noop() -> Result<()> {
    let store = test_rocks_store_with_limit("write-both-empty", 64).await?;

    // Both empty — should succeed without error
    store.write_addr_activity_entries_with_categories("blk", &[], &[], None)?;

    let (ids, _) = store
        .recent_ids_by_address("anyone", None, None, 100)
        .await?;
    assert!(ids.is_empty());

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// reindex_all_activity
// ═══════════════════════════════════════════════════════════════════

fn tx_payload(from_addr: &str, to_addr: &str, amount: &str) -> PlainPayload {
    PlainPayload::TxUtxo(pms_types::Transaction {
        inputs: vec![],
        outputs: vec![
            TxOutput {
                address: to_addr.into(),
                amount: amount.into(),
                asset_id: None,
            },
            TxOutput {
                address: from_addr.into(),
                amount: "9".into(),
                asset_id: None,
            },
        ],
        fee: "1".into(),
        unlocks: vec![],
    })
}

/// Insert a block directly into "blocks" and "id2ts" CFs, bypassing
/// `append_block_atomic` (which would already write activity indices).
fn insert_block_raw(store: &pms_storage::rocks_store::store::RocksStore, sb: &StoredBlock) {
    use pms_storage::helpers::ts_to_be;

    let cf_blocks = store.cf("blocks");
    let cf_i2t = store.cf("id2ts");

    let json = serde_json::to_vec(sb).unwrap();
    let ts = pms_storage::helpers::now_ms_i64();

    store.db.put_cf(&cf_blocks, sb.id.as_bytes(), &json).unwrap();
    store.db.put_cf(&cf_i2t, sb.id.as_bytes(), ts_to_be(ts)).unwrap();
}

#[tokio::test]
async fn reindex_rebuilds_plain_payload_indexes() -> Result<()> {
    let store = test_rocks_store_with_limit("reindex-plain", 64).await?;

    // Insert blocks RAW (no activity indexes)
    let b1 = sb_with_payload("rblk1", mint_payload("alice", "100"));
    insert_block_raw(&store, &b1);
    sleep(Duration::from_millis(5)).await;

    let b2 = sb_with_payload("rblk2", tx_payload("alice", "bob", "50"));
    insert_block_raw(&store, &b2);
    sleep(Duration::from_millis(5)).await;

    let b3 = sb_with_payload("rblk3", reward_payload("coord"));
    insert_block_raw(&store, &b3);

    // Before reindex: no activity entries
    let (alice_ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert!(alice_ids.is_empty(), "alice should have 0 entries before reindex");

    let (bob_ids, _) = store.recent_ids_by_address("bob", None, None, 100).await?;
    assert!(bob_ids.is_empty(), "bob should have 0 entries before reindex");

    // Run reindex
    let stats = store.reindex_all_activity()?;
    assert_eq!(stats.total_blocks, 3);
    assert_eq!(stats.indexed, 3);
    assert_eq!(stats.skipped_encrypted, 0);

    // After reindex: activity entries present
    let (alice_ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert_eq!(alice_ids.len(), 2, "alice should have 2 entries (mint + tx change)");

    let (bob_ids, _) = store.recent_ids_by_address("bob", None, None, 100).await?;
    assert_eq!(bob_ids.len(), 1, "bob should have 1 entry (tx recipient)");

    let (coord_ids, _) = store
        .recent_ids_by_address("coord", None, None, 100)
        .await?;
    assert_eq!(coord_ids.len(), 1, "coord should have 1 entry (fee_received)");

    // Typed index should also work
    let cats = &[ActivityCategory::Transfer.as_byte()];
    let (transfer_bob, _) = store
        .recent_ids_by_address_and_categories("bob", cats, None, None, 100)
        .await?;
    assert_eq!(transfer_bob, vec!["rblk2"]);

    let cats = &[ActivityCategory::Mint.as_byte()];
    let (mint_alice, _) = store
        .recent_ids_by_address_and_categories("alice", cats, None, None, 100)
        .await?;
    assert_eq!(mint_alice, vec!["rblk1"]);

    Ok(())
}

#[tokio::test]
async fn reindex_skips_encrypted_payloads() -> Result<()> {
    let store = test_rocks_store_with_limit("reindex-encrypted", 64).await?;

    // Insert a plain block
    let b1 = sb_with_payload("blk1", mint_payload("alice", "100"));
    insert_block_raw(&store, &b1);
    sleep(Duration::from_millis(5)).await;

    // Insert a fake encrypted block (just needs to parse as Encrypted variant)
    let enc_payload = pms_types_payload::EncryptedPayload {
        scheme: "x25519+aes256gcm".into(),
        key_version: 1,
        aad: pms_types_payload::AAD {
            len_hint: 0,
            binding: None,
        },
        commitment: "0000".into(),
        ciphertext_b64: "AAAA".into(),
        recipients: vec![],
        nonce_b64: "AAAA".into(),
    };
    let env = PayloadEnvelope::Encrypted(enc_payload);
    let b2 = StoredBlock {
        id: "enc1".into(),
        parents: vec![],
        payload_json: Some(serde_json::to_string(&env).unwrap()),
        nonce: 0,
        network_id: "".to_string(),
        protocol_version: 0,
        signer_pk_hex: "".to_string(),
        signature_hex: "".to_string(),
        metadata: None,
    };
    insert_block_raw(&store, &b2);

    // Run reindex
    let stats = store.reindex_all_activity()?;
    assert_eq!(stats.total_blocks, 2);
    assert_eq!(stats.indexed, 1, "only the plain block should be indexed");
    assert_eq!(stats.skipped_encrypted, 1);

    // Only alice's plain block should be indexed
    let (alice_ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert_eq!(alice_ids, vec!["blk1"]);

    Ok(())
}

#[tokio::test]
async fn reindex_is_idempotent() -> Result<()> {
    let store = test_rocks_store_with_limit("reindex-idempotent", 64).await?;

    let b1 = sb_with_payload("blk1", mint_payload("alice", "100"));
    insert_block_raw(&store, &b1);

    // Reindex twice
    let stats1 = store.reindex_all_activity()?;
    let stats2 = store.reindex_all_activity()?;

    assert_eq!(stats1.indexed, 1);
    assert_eq!(stats2.indexed, 1);

    // Alice should still have exactly 1 entry (not duplicated)
    let (alice_ids, _) = store
        .recent_ids_by_address("alice", None, None, 100)
        .await?;
    assert_eq!(alice_ids.len(), 1);

    Ok(())
}
