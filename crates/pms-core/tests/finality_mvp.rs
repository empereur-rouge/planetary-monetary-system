use std::sync::Arc;
use pms_core::{CoreAdapter, Dag};
use pms_utils::compute_block_id;
use anyhow::Result;
use tokio::sync::Mutex;
use pms_config::load_config;
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult, StoredBlock};
use pms_testkit::{forge_signed_wire_block_for_test, test_rocks_store};
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_wallet::{SignerBackend, Wallet};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireMeta};

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
    };
    let _ = store.append_block_atomic(&sb).await?;

    // 4) DAG RAM à partir du genesis
    let dag = Arc::new(Mutex::new(Dag::new_with_genesis(genesis.clone())));

    // 5) Adapter prod-like (c’est lui qui va gérer store + DAG)
    let adapter_concrete = CoreAdapter::new(dag.clone(), store.clone());
    let adapter: Arc<dyn NetDagAdapter> = adapter_concrete.clone();

    // 6) Wallet de test pour signer le milestone
    let wallet = Wallet::from_seed(&[9u8; 32], None)
        .expect("wallet seed ok");

    // 7) Choisir les parents pour le milestone
    let mut parents = store.top_tips(2).await?;
    if parents.is_empty() {
        parents.push(genesis.id.clone());
    }
    parents.sort();
    parents.dedup();

    // 8) Construire le payload Milestone
    let plain_ms = PlainPayload::Milestone { approved: vec![] };
    let env = PayloadEnvelope::Plain(plain_ms);

    // 9) Construire un WireBlock *sans* signature mais avec tous les champs stables
    let wb = forge_signed_wire_block_for_test(
        parents.clone(),
        &meta,
        &wallet,
        0,
        Option::from(env)
    );

    // 13) Persistance via l’adapter (chemin réel: validation + finalité + RAM)
    let res = adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "milestone persist_block doit réussir, got={res:?}"
    );

    // 14) Lire l’état de finalité depuis le DAG mis à jour par l’adapter
    let d = dag.lock().await;

    // Le milestone doit être bien enregistré comme dernier milestone
    assert_eq!(
        d.finality.last_milestone.as_deref(),
        Some(wb.id.as_str())
    );

    // Et il doit être considéré comme final
    assert!(
        d.is_final(&wb.id),
        "le milestone doit être final dès insertion"
    );

    Ok(())
}

#[tokio::test]
async fn k_depth_finalizes_blocks_rocks() -> anyhow::Result<()> {
    // 1) Store RocksDB temporaire (namespace de test isolé)
    let tr = test_rocks_store("kdepth").await?;
    let store = tr.store.clone();

    // 2) Charger la config réelle (config.prod.toml ou autre) puis construire WireMeta
    //    WireMeta contient :
    //      - network_id (ex: "pms-dev", "pms-mainnet")
    //      - protocol_version (u16)
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 3) Construire un bloc genesis en mémoire (structure Block)
    let genesis = Block::genesis(compute_block_id);

    // 4) Persister ce genesis dans Rocks en tant que StoredBlock complet
    //    On lui met des métadonnées réseau cohérentes, mais pas de signature.
    let sb = StoredBlock {
        id: genesis.id.clone(),
        parents: genesis.parents.clone(),                    // [] pour un genesis
        payload_json: serde_json::to_string(&genesis.payload).ok(),
        nonce: genesis.nonce,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),                     // sans importance pour genesis
        signature_hex:    String::new(),                     // idem
    };
    let _ = store.append_block_atomic(&sb).await?;

    // 5) Construire un DAG en RAM basé sur ce genesis
    //    Important : le DAG ne “voit” que ce qu’on lui donne ici.
    let dag = Arc::new(Mutex::new(Dag::new_with_genesis(genesis.clone())));

    // 6) Créer un adapter “prod-like” :
    //    - il connaît le DAG (Arc<Mutex<Dag>>)
    //    - il connaît le store Rocks
    //    - persist_block() met à jour Rocks + DAG + finalité
    let adapter_concrete = CoreAdapter::new(dag.clone(), store.clone());
    let adapter: Arc<dyn NetDagAdapter> = adapter_concrete.clone();

    // 7) Wallet de test (identité qui “signera” les blocs)
    let wallet = Wallet::from_seed(&[3u8; 32], None)
        .expect("wallet seed ok");

    // 8) Paramètre de finalité: profondeur k = 2
    //    => un bloc devient final quand il est enterré par au moins 2 descendants
    {
        let mut d = dag.lock().await;
        d.finality.depth_k = 2;
        d.finality.last_milestone = Some(genesis.id.clone());
        println!(
            "[DEBUG][k_depth] seed finalité = {:?}, depth_k={}",
            d.finality.last_milestone, d.finality.depth_k
        );
    }

    // 9) Insérer 3 nouveaux blocs signés via le pipeline prod-like :
    //    POUR CHAQUE BLOC :
    //      - choisir des parents (tips du store)
    //      - fabriquer un WireBlock signé cohérent (helper mk_signed_block_for_test)
    //      - appeler adapter.persist_block(&wb):
    //            → store.append_block_atomic
    //            → mise à jour DAG RAM
    //            → update_finality_after_insert()
    for i in 0..3 {
        // 9.a) Parents = tips actuelles (vue store)
        //      Si pas de tips (au tout début), on retombe sur le genesis.
        let mut parents = store.top_tips(2).await?;
        if parents.is_empty() {
            parents.push(genesis.id.clone());
        }
        parents.sort();
        parents.dedup();

        // 9.b) Construire un WireBlock signé pour ces parents
        let wb = forge_signed_wire_block_for_test(
            parents,
            &meta,
            &wallet,
            i as u64,
            Option::from(None)
        );
        // 9.c) Pipeline prod : persist_block (Rocks + DAG + finality)
        let res = adapter.persist_block(&wb).await?;
        assert!(
            matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
            "persist_block doit insérer ou idempoter, got={res:?}"
        );

        // DEBUG : état du DAG après chaque insertion
        {
            let d = dag.lock().await;
            println!(
                "[DEBUG][k_depth] après insert #{i}: blocks={}, finals={:?}, last_ms={:?}, depth_k={}",
                d.blocks.len(),
                d.finality.finalized,        // HashSet<String> en général
                d.finality.last_milestone,   // Option<String>
                d.finality.depth_k,
            );
        }
    }

    // 10) Relire le DAG en RAM pour vérifier la finalité
    let d = dag.lock().await;

    // On s’attend à ce qu’au moins un bloc soit finalisé selon la profondeur k
    let any_final = d.blocks.keys().any(|id| d.is_final(id));
    assert!(
        any_final,
        "au moins un bloc devrait être finalisé par profondeur k"
    );

    Ok(())
}