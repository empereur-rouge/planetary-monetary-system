use anyhow::Result;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::rocks_store::utxo::UtxoApply;
use std::sync::Arc;
use tempfile::{TempDir, tempdir};

//
// petit helper: DB éphémère + store
//
struct TestStore {
    _dir: TempDir,
    pub path: String,
    pub store: Arc<RocksStore>,
}

async fn mk_store(prefix: &str, tip_limit: usize) -> Result<TestStore> {
    let dir = tempdir()?;
    let path = dir
        .path()
        .join(format!("rocks-utxo-{}", nanoid::nanoid!(5)));
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_string_lossy().to_string();
    let store = Arc::new(RocksStore::new(&path_str, tip_limit, prefix).await?);
    Ok(TestStore {
        _dir: dir,
        path: path_str,
        store,
    })
}

#[tokio::test]
async fn apply_tx_atomic_ok_then_conflict_rocks() -> Result<()> {
    // 1) store éphémère
    let ts = mk_store("it:utxo", 64).await?;

    // ⚠️ On suppose que tu as bien créé les CF "it:utxo:utxo" et "it:utxo:tx_applied"
    // dans RocksStore::new(...) (cf. ColumnFamilyDescriptor).
    // On seed un UTXO coinbase "coinbase1:0" -> {"addr":"A","amt":"1.0"}

    let cf_utxo = ts.store.cf("utxo"); // cf("<prefix>:utxo")
    ts.store
        .db
        .put_cf(cf_utxo, b"coinbase1:0", br#"{"addr":"A","amt":"1.0"}"#)?;

    // 2) t1 consomme coinbase1:0 -> OK (retour true)
    let t1 = UtxoApply {
        txid: "t1".into(),
        inputs: vec![("coinbase1".into(), 0)],
        outputs: vec![("A".into(), "1.0".into())],
    };
    let ok1 = ts.store.utxo_apply_tx_atomic(&t1).await?;
    assert!(ok1, "t1 doit passer");

    // 3) t2 re-consomme coinbase1:0 -> conflit (retour false)
    let t2 = UtxoApply {
        txid: "t2".into(),
        inputs: vec![("coinbase1".into(), 0)],
        outputs: vec![("B".into(), "1.0".into())],
    };
    let ok2 = ts.store.utxo_apply_tx_atomic(&t2).await?;
    assert!(!ok2, "t2 doit être rejetée (double-spend)");

    // 4) idempotence: rejouer t1 -> false (déjà appliquée)
    let again = ts.store.utxo_apply_tx_atomic(&t1).await?;
    assert!(!again, "rejeu t1 doit être ignoré (déjà appliquée)");

    Ok(())
}
