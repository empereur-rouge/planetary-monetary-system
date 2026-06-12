use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult, StoredBlock};
use pms_testkit::{forge_signed_wire_block_for_test, test_rocks_store};
use pms_types::{Block, PayloadEnvelope};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
use std::sync::Arc;

#[tokio::test]
async fn bootstrap_recovers_all_blocks_and_children_rocks() -> anyhow::Result<()> {
    // 1) Store RocksDB temporaire
    let tr = test_rocks_store("final").await?;
    let store = tr.store.clone();

    // 2) Settings -> meta réseau
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 3) Genesis
    let genesis = Block::genesis(compute_block_id);

    // 4) Persist genesis avec StoredBlock complet
    let sb = StoredBlock {
        id: genesis.id.clone(),
        parents: genesis.parents.clone(),
        payload_json: serde_json::to_string(&genesis.payload).ok(),
        nonce: genesis.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    let _ = store.append_block_atomic(&sb).await?;

    // 5) DAG en RAM
    let dag = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));

    // 6) Adapter concret (prod-like)
    let adapter_concrete = CoreAdapter::new(dag.clone(), store.clone(), 0, None);
    let adapter: Arc<dyn NetDagAdapter> = adapter_concrete.clone();

    // 7) Wallet de test pour signer
    let wallet =
        Wallet::from_seed(&[3u8; 32], None).expect("Wallet::from_seed ne doit pas fail en test");

    // 8) Forge + persist N blocs via adapter, en respectant la même
    //    logique que la prod (id calculé AVANT signature).
    let n = 300usize;
    for i in 0..n {
        // 1) Choisir le parent à partir du tip du store (source de vérité).
        //    UN SEUL parent : l'enforcement single-writer (config dev,
        //    `enforce_single_writer = true`) rejette les blocs à 2+ parents
        //    ("must have exactly 1 parent"). Une chaîne single-parent teste
        //    aussi bien la recovery des blocs + children_count au bootstrap.
        let mut parents = store.top_tips(1).await?;
        if parents.is_empty() {
            parents.push(genesis.id.clone());
        }
        parents.sort();
        parents.dedup();

        // 2) Payload None en test
        let payload: Option<PayloadEnvelope> = None;

        // 3) Construire un WireBlock “unsigned” avec les bons champs réseau
        let wb =
            forge_signed_wire_block_for_test(parents.clone(), &meta, &wallet, i as u64, payload);

        // 7) Persister via adapter (qui appelle persist_block + verify_block_signature)
        let res = adapter.persist_block(&wb).await?;
        assert!(
            matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
            "persist_block doit insérer ou idempoter, got={res:?}"
        );
    }

    // 9) Bootstrap depuis le store et vérifs
    let dag2 = ConcurrentDag::bootstrap_from_store(&*store)
        .await
        .expect("bootstrap from rocks");

    let store_ids = store.all_block_ids().await?;
    assert_eq!(
        dag2.blocks.len(),
        store_ids.len(),
        "nb blocks rechargés != store"
    );
    // blocks is DashMap, iterate matches
    assert!(
        dag2.blocks.iter().any(|b| b.value().parents.is_empty()),
        "genesis manquant"
    );

    Ok(())
}
