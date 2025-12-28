use std::sync::Arc;
use anyhow::Result;
use rand::Rng;
use tokio::sync::Mutex;
use pms_config::load_config;
use pms_core::{CoreAdapter, Dag};
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult, StoredBlock};
use pms_types::{Block, PayloadEnvelope};
use pms_utils::compute_block_id;

// 👉 helper commun (crate pms-testkit) pour ouvrir un RocksStore éphémère
use pms_testkit::{forge_signed_wire_block_for_test, test_rocks_store};
use pms_wallet::{SignerBackend, Wallet};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireMeta;

#[tokio::test]
async fn stress_persist_2000_blocks_rocks() -> Result<()> {
    // DB éphémère + prefix unique
    let tr = test_rocks_store("stress-2000").await?;
    let store = tr.store.clone();

    // 0) Settings + meta réseau
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 1) Genesis en mémoire + persistance idempotente (avec méta complète)
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

    // 2) DAG en RAM + adapter réel
    let dag = Arc::new(Mutex::new(Dag::new_with_genesis(genesis.clone())));
    let adapter_concrete = CoreAdapter::new(dag.clone(), store.clone());
    let adapter: Arc<dyn NetDagAdapter> = adapter_concrete.clone();

    // 👉 On active la finalité k-depth pour ce test
    {
        let mut d = dag.lock().await;
        d.finality.depth_k = 5; // par ex. k = 5 confirmations
    }

    // Wallet de test pour signer les blocs
    let wallet = Wallet::from_seed(&[5u8; 32], None)
        .expect("wallet seed pour test ne doit pas échouer");

    // 3) Insère 2000 blocs ultra-lights (payload=None, difficulté=0)
    let n: usize = 2_000;
    let mut created_ids: Vec<String> = Vec::with_capacity(n);

    for i in 0..n {
        // 1) Parents choisis d'après les tips RAM, sans garder le lock pendant l'await
        let parents = {
            let d = dag.lock().await;
            let mut tips = d.find_tips();
            if tips.is_empty() {
                tips.push(genesis.id.clone());
            }
            tips.sort();
            tips.dedup();
            tips
        };

        // 2) Payload None
        let payload: Option<PayloadEnvelope> = None;

        // 3) WireBlock “prépare tout sauf signature”
        let wb = forge_signed_wire_block_for_test(
            parents.clone(),
            &meta,
            &wallet,
            i as u64,
            payload
        );

        // 7) Persistance via l’adapter (qui met à jour DAG + finalité + store)
        let res = adapter.persist_block(&wb).await?;
        assert!(
            matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
            "persist_block doit insérer ou être idempotent, got={res:?}"
        );

        if i % 500 == 0 {
            eprintln!("it={i}, id={}, parents={:?}", wb.id, wb.parents);
        }

        created_ids.push(wb.id.clone());
    }

    // ---- Invariants mémoire (DAG local) ----
    {
        let d = dag.lock().await;
        assert_eq!(
            d.blocks.len(),
            1 + n,
            "taille du DAG en mémoire incorrecte"
        );
    }

    // ---- Vérifications store (échantillon) ----
    for sample in created_ids.iter().step_by(137).take(20) {
        let got = store.get_block(sample).await?;
        assert!(got.is_some(), "block manquant en store: {sample}");
        let sb = got.unwrap();
        assert_eq!(sb.id, *sample, "id store != id attendu");

        if let Some(js) = sb.payload_json.as_ref() {
            let _ok: Option<PayloadEnvelope> = serde_json::from_str(js).unwrap();
        }
    }

    // 2) tips plafonnés (respect de tip_limit initial du testkit = 128)
    let tips = store.top_tips(256).await?;
    assert!(
        tips.len() <= 128,
        "tips dépassent tip_limit ({} > 128)",
        tips.len()
    );

    // 3) enfants store ~= enfants mémoire (échantillon)
    let mut rng = rand::rng();
    {
        let d = dag.lock().await;
        for _ in 0..20 {
            let idx = rng.random_range(0..created_ids.len());
            let bid = &created_ids[idx];
            let b = d.blocks.get(bid).unwrap();
            if b.parents.is_empty() {
                continue;
            }
            let p = &b.parents[0];

            let mem = *d.children.get(p).unwrap_or(&0);
            let store_cnt = store.children_count(p).await?;
            assert_eq!(
                store_cnt, mem,
                "children_count mismatch pour parent {p}: store={store_cnt} mem={mem}"
            );
        }
    }

    // ---- ✅ Vérification des checkpoints / finalité ----
    {
        let d = dag.lock().await;

        // Il doit y avoir au moins un bloc marqué final
        assert!(
            !d.finality.finalized.is_empty(),
            "aucun bloc finalisé après insertion de {n} blocs avec depth_k={}",
            d.finality.depth_k
        );

        // On vérifie qu'au moins un id final est bien reconnu par is_final()
        let some_final = d.finality.finalized.iter().next().unwrap().clone();
        assert!(
            d.is_final(&some_final),
            "le bloc marqué final dans finality.finalized doit être is_final()"
        );
    }

    // Et on vérifie la cohérence avec les checkpoints persistés en store
    let finals_store = store.load_final().await.unwrap_or_default();
    {
        let d = dag.lock().await;
        let finals_ram: Vec<String> = d.finality.finalized.iter().cloned().collect();

        // Même nombre de checkpoints
        assert_eq!(
            finals_store.len(),
            finals_ram.len(),
            "nombre de checkpoints finalisés en store != RAM"
        );

        // Tous les checkpoints du store doivent exister en RAM
        for fid in &finals_store {
            assert!(
                finals_ram.contains(fid),
                "checkpoint {fid} présent en store mais pas dans finality RAM"
            );
        }
    }

    Ok(())
}

#[tokio::test]
async fn atomic_persist_rejects_duplicate_rocks() -> Result<()> {
    let tr = test_rocks_store("dupl").await?;
    let store = tr.store.clone();

    // On récupère network_id / protocol_version depuis les settings
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let sb = StoredBlock {
        id: "DUPL-TEST".to_string(),
        parents: vec!["GEN".into()],
        payload_json: Some("null".into()),
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
    };

    let ok1 = store.append_block_atomic(&sb).await?;
    let ok2 = store.append_block_atomic(&sb).await?;

    assert!(ok1, "1er insert doit réussir");
    assert!(!ok2, "2e insert doit être refusé atomiquement");

    Ok(())
}