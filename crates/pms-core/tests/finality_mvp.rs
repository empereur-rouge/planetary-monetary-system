// crates/pms-core/tests/finality_mvp.rs

use anyhow::Result;
use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult, StoredBlock};
use pms_testkit::{forge_signed_wire_block_for_test, test_rocks_store};
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
use std::sync::Arc;

#[tokio::test]
async fn milestone_sets_seed_and_finalizes_rocks() -> Result<()> {
    // 1) RocksDB temporaire
    let tr = test_rocks_store("milestone").await?;
    let store = tr.store.clone();

    // 2) Settings -> meta réseau (network_id, protocol_version)
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 3) Créer et persister un genesis complet
    let genesis = Block::genesis(compute_block_id);

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

    // 4) DAG RAM à partir du genesis, lock-free
    let dag = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));

    // 5) Adapter prod-like (c'est lui qui va gérer store + DAG)
    let adapter_concrete = CoreAdapter::new(dag.clone(), store.clone());
    let adapter: Arc<dyn NetDagAdapter> = adapter_concrete.clone();

    // 6) Wallet de test pour signer le milestone
    let wallet = Wallet::from_seed(&[9u8; 32], None).expect("wallet seed ok");

    // 7) Choisir les parents pour le milestone
    let mut parents = store.top_tips(2).await?;
    if parents.is_empty() {
        parents.push(genesis.id.clone());
    }
    parents.sort();
    parents.dedup();

    // 8) Construire le payload Milestone
    let plain_ms = PlainPayload::Milestone {
        approved: vec![],
        distribute_node_rewards: false,
    };
    let env = PayloadEnvelope::Plain(plain_ms);

    // 9) Construire un WireBlock signé
    let wb =
        forge_signed_wire_block_for_test(parents.clone(), &meta, &wallet, 0, Option::from(env));

    // 10) Persistance via l'adapter
    let res = adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "milestone persist_block doit réussir, got={res:?}"
    );

    // 11) Lire l'état de finalité (synchronous RwLock)
    {
        let f = dag.finality.read().unwrap();
        assert_eq!(f.last_milestone.as_deref(), Some(wb.id.as_str()));
    }

    // 12) Vérifier que le bloc est final (synchronous method)
    assert!(
        dag.is_final(&wb.id),
        "le milestone doit être final dès insertion"
    );

    Ok(())
}

#[tokio::test]
async fn k_depth_finalizes_blocks_rocks() -> anyhow::Result<()> {
    // 1) Store RocksDB temporaire
    let tr = test_rocks_store("kdepth").await?;
    let store = tr.store.clone();

    // 2) Config + meta
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 3) Genesis
    let genesis = Block::genesis(compute_block_id);

    // 4) Persister genesis
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

    // 5) DAG + adapter
    let dag = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));
    let adapter_concrete = CoreAdapter::new(dag.clone(), store.clone());
    let adapter: Arc<dyn NetDagAdapter> = adapter_concrete.clone();

    // 6) Wallet de test
    let wallet = Wallet::from_seed(&[3u8; 32], None).expect("wallet seed ok");

    // 7) Paramètre de finalité: profondeur k = 2 (synchronous RwLock)
    {
        let mut f = dag.finality.write().unwrap();
        f.depth_k = 2;
        f.last_milestone = Some(genesis.id.clone());
        println!(
            "[DEBUG][k_depth] seed finalité = {:?}, depth_k={}",
            f.last_milestone, f.depth_k
        );
    }

    // 8) Insérer 3 blocs
    for i in 0..3 {
        let mut parents = store.top_tips(2).await?;
        if parents.is_empty() {
            parents.push(genesis.id.clone());
        }
        parents.sort();
        parents.dedup();

        let wb =
            forge_signed_wire_block_for_test(parents, &meta, &wallet, i as u64, Option::from(None));

        let res = adapter.persist_block(&wb).await?;
        assert!(
            matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
            "persist_block doit insérer ou idempoter, got={res:?}"
        );

        // DEBUG (synchronous RwLock)
        {
            let f = dag.finality.read().unwrap();
            println!(
                "[DEBUG][k_depth] après insert #{i}: blocks={}, finals={:?}, last_ms={:?}, depth_k={}",
                dag.blocks.len(),
                f.finalized,
                f.last_milestone,
                f.depth_k,
            );
        }
    }

    // 9) Vérifier finalité (synchronous method)
    let keys: Vec<String> = dag.blocks.iter().map(|entry| entry.key().clone()).collect();
    let mut any_final = false;
    for id in keys {
        if dag.is_final(&id) {
            any_final = true;
            break;
        }
    }

    assert!(
        any_final,
        "au moins un bloc devrait être finalisé par profondeur k"
    );

    Ok(())
}
