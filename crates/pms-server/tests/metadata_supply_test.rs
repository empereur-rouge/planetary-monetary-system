use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::{Block, BlockMetadata, PayloadEnvelope, PlainPayload, TxOutput};
use pms_utils::compute_block_id;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
async fn test_metadata_persistence_and_supply() {
    let dir = tempdir().unwrap();
    // Fix: to_str() for path
    let store = Arc::new(
        RocksStore::new(dir.path().to_str().unwrap(), 1000, "test", None)
            .await
            .unwrap(),
    );

    // Bootstrap DAG
    let genesis_block = Block::genesis(compute_block_id);
    // Fix: ConcurrentDag::new() takes no args. Insert genesis manually.
    let dag = Arc::new(ConcurrentDag::new());
    dag.insert_block(genesis_block.clone());

    let adapter = CoreAdapter::new(dag.clone(), store.clone());

    // 0. Bootstrap UTXO
    adapter.bootstrap_utxos().await.unwrap();

    // Initial supply
    let (supply, _) = adapter.circulating_supply().await;
    assert_eq!(supply, Decimal::ZERO);

    // 1. Create a Mint block with Metadata
    let parent = genesis_block.id.clone();

    let output = TxOutput {
        address: "addr_test".to_string(),
        amount: "100.0".to_string(),
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: vec![output],
    }));

    let mut block = Block::new(vec![parent.clone()], payload, 0, None, compute_block_id).unwrap();

    // Mine
    block.nonce = 12345;
    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

    // Add Metadata
    let meta = BlockMetadata {
        description: Some("Test Block".to_string()),
        tags: vec!["test".to_string(), "supply".to_string()],
        extra: None,
        signer_x25519_hex: None,
    };
    block.metadata = Some(meta.clone());

    // 2. Persist via Adapter
    let wb = pms_core::forge::to_wire(&block);

    // Ensure metadata is in WireBlock
    assert!(wb.metadata.is_some());
    assert_eq!(
        wb.metadata.as_ref().unwrap().description.as_deref(),
        Some("Test Block")
    );

    // Manual persist pipeline
    let sb = pms_storage::StoredBlock::from(wb.clone());
    assert!(sb.metadata.is_some());

    store.append_block_atomic(&sb).await.unwrap();

    // Verify Retrieve
    let retrieved = store.get_block(&block.id).await.unwrap().unwrap();
    assert_eq!(retrieved.metadata.as_ref().unwrap().tags[0], "test");

    // Update DAG & UTXO (manual since bypassing persist_block)
    dag.insert_block(block.clone());

    let oid = pms_types::OutputId {
        txid: block.id.clone(),
        index: 0,
    };
    adapter
        .utxos
        .add(
            oid,
            TxOutput {
                address: "addr_test".into(),
                amount: "100.0".into(),
            },
        )
        .await;

    // 3. Verify Supply
    let (supply, count) = adapter.circulating_supply().await;
    let expected = Decimal::from_str_exact("100.0").unwrap();
    assert_eq!(supply, expected);
    assert_eq!(count, 1);

    println!("Test metadata & supply passed!");
}
