// tests/rocks_integration.rs

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;
use tokio::task::JoinSet;

use pms_storage::{DagStorage, StoredBlock, PutResult};
use pms_storage::rocks_store::store::RocksStore;
use pms_testkit::{mk_block, test_meta_and_wallet};
use pms_types_block::BlockId;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_types_transaction::TxOutput;
use pms_wire::WireMeta;
// ==== helpers ==== //

fn temp_db_path() -> PathBuf {
    // dossier unique genre target/tmp-rocks-<rand>
    let rand = nanoid::nanoid!();
    let mut p = std::env::temp_dir();
    p.push(format!("pms_rocks_test_{rand}"));
    p
}

#[tokio::test]
async fn can_open_and_basic_put() -> Result<()> {
    let path = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store = RocksStore::new(path.to_str().unwrap(), 64, prefix.clone()).await?;

    let (meta, wallet) = test_meta_and_wallet();

    // on fabrique un bloc simple
    let b = mk_block("B1", vec![], &meta);

    // put_block doit dire Inserted la 1ère fois
    let r = store.put_block(&b).await?;
    assert!(matches!(r, PutResult::Inserted));

    // relire
    let got = store.get_block("B1").await?
        .expect("block should exist");
    assert_eq!(got.id, "B1");

    Ok(())
}

#[tokio::test]
async fn migrations_apply_and_version_is_current() -> Result<()> {
    use pms_storage::migrations::CURRENT_VER;

    let path = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store = RocksStore::new(path.to_str().unwrap(), 64, prefix.clone()).await?;

    // si tu as une fonction équivalente à ensure_schema()
    store.ensure_schema().await?;

    // et si tu as une méthode style store.get_version() -> Result<i64>
    let v = store.get_version().await?;
    assert_eq!(v, CURRENT_VER);

    Ok(())
}

#[tokio::test]
async fn put_get_block_and_index() -> Result<()> {
    let path   = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store  = RocksStore::new(path.to_str().unwrap(), 64, prefix.clone()).await?;

    let (meta, wallet) = test_meta_and_wallet();

    let b = mk_block("B1", vec![], &meta);
    store.put_block(&b).await?;

    // lecture directe
    let got = store.get_block("B1").await?
        .expect("block present");
    assert_eq!(got.id, "B1");

    // index global
    let all = store.all_block_ids().await?;
    assert!(all.contains(&"B1".to_string()));

    Ok(())
}

#[tokio::test]
async fn children_counter_and_tips_basic() -> Result<()> {
    let path   = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store  = RocksStore::new(path.to_str().unwrap(), 64, prefix.clone()).await?;

    let (meta, wallet) = test_meta_and_wallet();

    let p  = mk_block("P", vec![], &meta);
    let c  = mk_block("C", vec!["P".into()], &meta);

    // stocker blocs
    store.put_block(&p).await?;
    store.add_tip(&p.id).await?; // P est tip au départ

    store.put_block(&c).await?;
    store.add_child_edge("P", "C").await?;

    store.add_tip(&c.id).await?;
    store.remove_tip("P").await?;

    // Vérif compteur enfants
    let cc = store.children_count("P").await?;
    assert_eq!(cc, 1);

    // Vérif tips
    let tips = store.top_tips(10).await?;
    assert_eq!(tips, vec!["C".to_string()]);

    Ok(())
}

#[tokio::test]
async fn tips_respect_limit_with_trim() -> Result<()> {
    let path   = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    // tip_limit = 4
    let store  = RocksStore::new(path.to_str().unwrap(), 4, prefix.clone()).await?;

    let (meta, wallet) = test_meta_and_wallet();

    // On ajoute 6 blocs via append_block_atomic -> ça doit tronquer
    for i in 0..6 {
        let id = format!("T{i}");
        let b  = mk_block(&id, vec![], &meta);
        assert!(store.append_block_atomic(&b).await?);
        sleep(Duration::from_millis(2)).await; // sépare un peu les timestamps
    }

    // récupère les tips
    let tips = store.top_tips(10).await?;
    // On s'attend aux 4 plus récents : T5,T4,T3,T2 (ordre du plus récent au plus vieux)
    assert_eq!(tips.len(), 4);
    assert_eq!(
        tips,
        vec!["T5","T4","T3","T2"].into_iter().map(|s| s.to_string()).collect::<Vec<_>>()
    );

    Ok(())
}

#[tokio::test]
async fn export_then_import_roundtrip() -> Result<()> {
    let path1   = temp_db_path();
    let prefix1 = format!("pms:test:{}", nanoid::nanoid!());
    let store1  = RocksStore::new(path1.to_str().unwrap(), 64, prefix1.clone()).await?;

    let (meta, wallet) = test_meta_and_wallet();

    // P -> C1, P -> C2
    let p  = mk_block("P",  vec![], &meta);
    let c1 = mk_block("C1", vec!["P".into()], &meta);
    let c2 = mk_block("C2", vec!["P".into()], &meta);

    // Insère P, puis C1 et C2, et mets à jour tips/enfants
    store1.put_block(&p).await?;
    store1.add_tip("P").await?;

    store1.put_block(&c1).await?;
    store1.add_child_edge("P","C1").await?;
    store1.add_tip("C1").await?;
    store1.remove_tip("P").await?;

    store1.put_block(&c2).await?;
    store1.add_child_edge("P","C2").await?;
    store1.add_tip("C2").await?;
    store1.remove_tip("P").await?;

    // export
    let dump = store1.export_namespace().await?;
    // simple sanity: doit contenir les 3 ids
    #[derive(serde::Deserialize)]
    struct SB { id: String }
    let v: Vec<SB> = serde_json::from_str(&dump)?;
    let ids: std::collections::HashSet<_> =
        v.into_iter().map(|b| b.id).collect();
    assert!(ids.contains("P"));
    assert!(ids.contains("C1"));
    assert!(ids.contains("C2"));

    // Import dans un nouveau RocksStore vierge
    let path2   = temp_db_path();
    let prefix2 = format!("pms:import:{}", nanoid::nanoid!());
    let store2  = RocksStore::new(path2.to_str().unwrap(), 64, prefix2.clone()).await?;

    store2.import_json(&dump).await?;

    // Vérifie présence des blocs
    for id in ["P","C1","C2"] {
        let b = store2.get_block(id).await?
            .expect("block present after import");
        assert_eq!(b.id, id);
    }

    // Tips attendues: C1 et C2
    let tips2 = store2.top_tips(10).await?;
    assert!(tips2.contains(&"C1".to_string()));
    assert!(tips2.contains(&"C2".to_string()));

    Ok(())
}

#[tokio::test]
async fn append_is_atomic_and_idempotent() -> Result<()> {
    let path   = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store  = std::sync::Arc::new(
        RocksStore::new(path.to_str().unwrap(), 64, prefix.clone()).await?
    );

    // Charger meta réseau (test/prod cohérent)
    let (meta, wallet) = test_meta_and_wallet();

    // bloc racine b0
    let b0 = StoredBlock {
        id: "b0".into(),
        parents: vec![],
        nonce: 1,
        payload_json: None,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),
        signature_hex:    String::new(),
    };
    assert!(store.append_block_atomic(&b0).await?);

    // candidat enfant b1
    let b1 = StoredBlock {
        id: "b1".into(),
        parents: vec!["b0".to_string()],
        nonce: 1,
        payload_json: None,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),
        signature_hex:    String::new(),
    };

    // lance 10 fois en parallèle
    let mut js = JoinSet::new();
    for _ in 0..10 {
        let st = store.clone();
        let b  = b1.clone();
        js.spawn(async move {
            st.append_block_atomic(&b).await
        });
    }

    while let Some(res) = js.join_next().await {
        res??;
    }

    // Vérifie que b0 n’a qu’UN enfant
    let cnt = store.children_count("b0").await?;
    assert_eq!(cnt, 1, "b0 should have exactly one counted child");

    // Vérifie tips
    let tips = store.top_tips(10).await?;
    assert!(tips.contains(&"b1".to_string()));
    assert!(!tips.contains(&"b0".to_string()));

    Ok(())
}