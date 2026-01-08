use std::sync::Arc;

use anyhow::Result;
use tempfile::tempdir;
use tokio::time::{Duration, sleep};

use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::Block;
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;

type DagRef = Arc<ConcurrentDag>;

/// Test d'intégration "happy path"
#[tokio::test]
async fn signed_block_goes_through_full_pipeline() -> Result<()> {
    // 1) RocksDB éphémère + store
    let dir = tempdir()?;
    let db_path = dir.path().join("rocks-core-pipeline");

    let store =
        Arc::new(RocksStore::new(db_path.to_string_lossy().as_ref(), 256, "pms:test").await?);

    // Initialize store schema
    store.ensure_schema().await?;
    store.bootstrap_once_for_production()?;

    // 2) Config réseau + meta
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 3) DAG avec genesis
    let genesis = Block::genesis(compute_block_id);

    // Persist genesis to store
    store.persist_genesis(&genesis, &meta).await?;

    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));

    // 4) Adapter
    let adapter = CoreAdapter::new(dag.clone(), store.clone());

    // 5) Parents = genesis
    let parents = vec![genesis.id.clone()];

    // 6) Wallet de test
    let wallet =
        Wallet::from_seed(&[3u8; 32], None).expect("Wallet::from_seed ne doit pas fail en test");

    // 7) Forge d'un WireBlock signé
    let payload = None;

    let wb = forge_signed_wire_block_for_test(parents.clone(), &meta, &wallet, 1, payload);

    // 8) persist_block()
    let res = adapter.persist_block(&wb).await?;

    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "bloc signé devrait être accepté par la pipeline, obtenu: {res:?}"
    );

    // 9) Wait for background persist to complete
    // The persist_block uses fire-and-forget async persistence
    // Wait a bit for the background writer to flush
    sleep(Duration::from_millis(100)).await;

    // 10) Vérif store
    let stored = store.get_block(&wb.id).await?;

    assert!(
        stored.is_some(),
        "le store doit contenir le bloc après persist_block() + flush"
    );

    // 11) Vérif DAG (lock-free access)
    assert!(
        dag.blocks.contains_key(&wb.id),
        "le DAG en RAM doit contenir le bloc inséré"
    );

    // Finalité : vérifier accès sans panique
    let f = dag.finality.read().unwrap();
    let _finalized: Vec<String> = f.finalized.iter().cloned().collect();

    Ok(())
}
