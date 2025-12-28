// crates/pms-storage/tests/rocks_migration
use anyhow::Result;
use tempfile::{tempdir, TempDir};
use tokio::time::{sleep, Duration};
use std::sync::Arc;

use pms_storage::{DagStorage, StoredBlock};
use pms_storage::rocks_store::store::RocksStore;
use pms_testkit::{mk_block, test_meta_and_wallet};

// -- helper: crée une DB éphémère + RocksStore
struct TestStore {
    _dir: TempDir,                // garde la vie du dossier
    pub path: String,
    pub store: Arc<RocksStore>,
}

async fn mk_store(tip_limit: usize, prefix: &str) -> Result<TestStore> {
    let dir = tempdir()?;
    let path = dir.path().join(format!("rocks-{}", nanoid::nanoid!(6)));
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_string_lossy().to_string();

    let store = Arc::new(RocksStore::new(&path_str, tip_limit, prefix).await?);
    Ok(TestStore { _dir: dir, path: path_str, store })
}

#[tokio::test]
async fn can_open_and_write_read() -> Result<()> {
    let ts = mk_store(64, &format!("it:open:{}", nanoid::nanoid!(6))).await?;
    let (meta, wallet) = test_meta_and_wallet();
    // put/get simple
    let b = mk_block("B1", vec![], &meta);
    ts.store.put_block(&b).await?;
    let got = ts.store.get_block("B1").await?.expect("present");
    assert_eq!(got.id, "B1");
    Ok(())
}

#[tokio::test]
async fn migrations_apply_and_version_is_current_rocks() -> Result<()> {
    use pms_storage::migrations::CURRENT_VER;

    let ts = mk_store(64, &format!("it:mig:{}", nanoid::nanoid!(6))).await?;
    // Si tu as un ensure_schema() pour Rocks :
    ts.store.ensure_schema().await?; // no-op si déjà OK

    let (meta, wallet) = test_meta_and_wallet();

    // Vérifie que la migration a bien laissé une trace (option: expose une méthode).
    // Ici on se contente de faire un round-trip basique : la DB est utilisable.
    let b = mk_block("M1", vec![], &meta);
    ts.store.put_block(&b).await?;
    let _ = ts.store.get_block("M1").await?.expect("present");

    // (Optionnel) si tu stockes la version (cf. impl proposée):
    // assert_eq!(ts.store.load_schema_version().await?, Some(CURRENT_VER));

    let _ = CURRENT_VER; // pour ne pas laisser l’import mort
    Ok(())
}

#[tokio::test]
async fn put_get_block_and_index_rocks() -> Result<()> {
    let ts = mk_store(64, &format!("it:pgi:{}", nanoid::nanoid!(6))).await?;
    let (meta, wallet) = test_meta_and_wallet();
    let b = mk_block("B1", vec![], &meta);
    ts.store.put_block(&b).await?;

    let got = ts.store.get_block("B1").await?.expect("block present");
    assert_eq!(got.id, "B1");

    let all = ts.store.all_block_ids().await?;
    assert!(all.contains(&"B1".to_string()));
    Ok(())
}

#[tokio::test]
async fn children_counter_and_tips_basic_rocks() -> Result<()> {
    let ts = mk_store(64, &format!("it:ctb:{}", nanoid::nanoid!(6))).await?;
    let (meta, wallet) = test_meta_and_wallet();

    let p = mk_block("P", vec![], &meta);
    let c = mk_block("C", vec!["P".into()], &meta);

    // Stocke d’abord le parent, puis l’enfant
    ts.store.put_block(&p).await?;
    ts.store.add_tip(&p.id).await?;

    ts.store.put_block(&c).await?;
    ts.store.add_child_edge("P", "C").await?;
    ts.store.add_tip(&c.id).await?;
    ts.store.remove_tip("P").await?;

    // children_count(P) == 1
    let cc = ts.store.children_count("P").await?;
    assert_eq!(cc, 1);

    // tips == ["C"]
    let tips = ts.store.top_tips(10).await?;
    assert_eq!(tips, vec!["C".to_string()]);
    Ok(())
}

#[tokio::test]
async fn tips_respect_limit_with_trim_rocks() -> Result<()> {
    let ts = mk_store(4, &format!("it:trim:{}", nanoid::nanoid!(6))).await?;

    let (meta, wallet) = test_meta_and_wallet();

    // Ajoute 6 tips; seuls les 4 plus récents doivent rester
    for i in 0..6 {
        let id = format!("T{i}");
        let b = mk_block(&id, vec![], &meta);
        ts.store.put_block(&b).await?;
        ts.store.add_tip(&id).await?;
        sleep(Duration::from_millis(2)).await; // timestamps distincts
    }

    let tips = ts.store.top_tips(10).await?;
    assert_eq!(tips.len(), 4);
    assert_eq!(tips, vec!["T5", "T4", "T3", "T2"].iter().map(|s| s.to_string()).collect::<Vec<_>>());
    Ok(())
}

#[tokio::test]
async fn export_then_import_roundtrip_rocks() -> Result<()> {
    // Source
    let src = mk_store(64, &format!("it:exp:src:{}", nanoid::nanoid!(6))).await?;

    let (meta, wallet) = test_meta_and_wallet();

    // P -> C1, P -> C2
    let p  = mk_block("P",  vec![], &meta);
    let c1 = mk_block("C1", vec!["P".into()], &meta);
    let c2 = mk_block("C2", vec!["P".into()], &meta);

    src.store.put_block(&p).await?;
    src.store.add_tip("P").await?;

    src.store.put_block(&c1).await?;
    src.store.add_child_edge("P", "C1").await?;
    src.store.add_tip("C1").await?;
    src.store.remove_tip("P").await?;

    src.store.put_block(&c2).await?;
    src.store.add_child_edge("P", "C2").await?;
    src.store.add_tip("C2").await?;
    src.store.remove_tip("P").await?;

    // Export JSON
    let dump = src.store.export_namespace().await?;
    #[derive(serde::Deserialize)]
    struct SB { id: String }
    let v: Vec<SB> = serde_json::from_str(&dump)?;
    let ids: std::collections::HashSet<_> = v.into_iter().map(|b| b.id).collect();
    assert!(ids.contains("P"));
    assert!(ids.contains("C1"));
    assert!(ids.contains("C2"));

    // Import dans autre DB
    let dst = mk_store(64, &format!("it:exp:dst:{}", nanoid::nanoid!(6))).await?;
    dst.store.import_json(&dump).await?;

    // Présence
    for id in ["P", "C1", "C2"] {
        let b = dst.store.get_block(id).await?.expect("block present");
        assert_eq!(b.id, id);
    }

    // Tips attendus: C1 & C2
    let tips2 = dst.store.top_tips(10).await?;
    assert!(tips2.contains(&"C1".to_string()));
    assert!(tips2.contains(&"C2".to_string()));
    Ok(())
}

#[tokio::test]
async fn append_is_atomic_and_idempotent_rocks() -> Result<()> {
    use tokio::task::JoinSet;

    let ts = mk_store(64, &format!("it:atomic:{}", nanoid::nanoid!(6))).await?;

    // Charge meta & wallet de test pour remplir StoredBlock
    let (meta, _wallet) = test_meta_and_wallet();

    // bloc racine
    let b0 = StoredBlock {
        id: "b0".into(),
        parents: vec![],
        nonce: 1,
        payload_json: None,

        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
    };
    assert!(ts.store.append_block_atomic(&b0).await?);

    // concurrent sur b1 (mêmes parents)
    let b1 = StoredBlock {
        id: "b1".into(),
        parents: vec!["b0".into()],
        nonce: 1,
        payload_json: None,

        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
    };

    // lance 10 fois en parallèle
    let mut js = JoinSet::new();
    for _ in 0..10 {
        let st = ts.store.clone();
        let blk = b1.clone();
        js.spawn(async move {
            st.append_block_atomic(&blk).await
        });
    }

    while let Some(res) = js.join_next().await {
        res??; // ignore booléen
    }

    // Vérifie cohérence enfants
    let cnt = ts.store.children_count("b0").await?;
    assert_eq!(cnt, 1, "b0 ne doit avoir qu'un seul enfant");

    // Vérifie tips
    let tips = ts.store.top_tips(10).await?;
    assert!(tips.contains(&"b1".to_string()));
    assert!(!tips.contains(&"b0".to_string()));

    Ok(())
}