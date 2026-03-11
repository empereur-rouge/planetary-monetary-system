use anyhow::Result;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::rocks_store::utxo::UtxoApply;
use std::sync::Arc;
use std::sync::mpsc;
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
    let store = Arc::new(RocksStore::new(&path_str, tip_limit, prefix, None).await?);
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
        .put_cf(&cf_utxo, b"coinbase1#0", br#"{"addr":"A","amt":"1.0"}"#)?;

    // 2) t1 consomme coinbase1:0 -> OK (retour true)
    let t1 = UtxoApply {
        txid: "t1".into(),
        inputs: vec![("coinbase1".into(), 0)],
        outputs: vec![("A".into(), "1.0".into(), None)],
    };
    let ok1 = ts.store.utxo_apply_tx_atomic(&t1).await?;
    assert!(ok1, "t1 doit passer");

    // 3) t2 re-consomme coinbase1:0 -> conflit (retour false)
    let t2 = UtxoApply {
        txid: "t2".into(),
        inputs: vec![("coinbase1".into(), 0)],
        outputs: vec![("B".into(), "1.0".into(), None)],
    };
    let ok2 = ts.store.utxo_apply_tx_atomic(&t2).await?;
    assert!(!ok2, "t2 doit être rejetée (double-spend)");

    // 4) idempotence: rejouer t1 -> false (déjà appliquée)
    let again = ts.store.utxo_apply_tx_atomic(&t1).await?;
    assert!(!again, "rejeu t1 doit être ignoré (déjà appliquée)");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// stream_all_utxos tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn stream_all_utxos_matches_iter_all() -> Result<()> {
    let ts = mk_store("stream:match", 64).await?;

    // Seed 100 UTXOs
    let cf_utxo = ts.store.cf("utxo");
    for i in 0..100u32 {
        let key = format!("tx_{:04}#{}", i, i % 3);
        let val = format!(
            r#"{{"addr":"addr_{}","amt":"{}.0"}}"#,
            i % 10,
            i
        );
        ts.store
            .db
            .put_cf(&cf_utxo, key.as_bytes(), val.as_bytes())?;
    }

    // Collect via iter_all_utxos
    let vec_result = ts.store.iter_all_utxos()?;
    println!("  iter_all_utxos returned {} entries", vec_result.len());

    // Collect via stream_all_utxos
    let (tx, rx) = mpsc::sync_channel(16);
    let store_clone = ts.store.clone();
    let producer = std::thread::spawn(move || store_clone.stream_all_utxos(tx));

    let mut stream_result = Vec::new();
    while let Ok(item) = rx.recv() {
        stream_result.push(item);
    }
    let producer_count = producer.join().unwrap()?;
    println!(
        "  stream_all_utxos sent {}, received {}",
        producer_count,
        stream_result.len()
    );

    // Both must return same count
    assert_eq!(vec_result.len(), stream_result.len());
    assert_eq!(producer_count, stream_result.len());

    // Compare item-by-item (RocksDB key order is deterministic)
    for (i, ((v_txid, v_idx, v_uv), (s_txid, s_idx, s_uv))) in
        vec_result.iter().zip(stream_result.iter()).enumerate()
    {
        println!(
            "  [{i}] vec=({v_txid}#{v_idx}, addr={}, amt={}) stream=({s_txid}#{s_idx}, addr={}, amt={})",
            v_uv.address, v_uv.amount, s_uv.address, s_uv.amount
        );
        assert_eq!(v_txid, s_txid, "txid mismatch at {i}");
        assert_eq!(v_idx, s_idx, "index mismatch at {i}");
        assert_eq!(v_uv.address, s_uv.address, "address mismatch at {i}");
        assert_eq!(v_uv.amount, s_uv.amount, "amount mismatch at {i}");
        assert_eq!(v_uv.asset_id, s_uv.asset_id, "asset_id mismatch at {i}");
    }

    println!("  PASS: stream_all_utxos matches iter_all_utxos");
    Ok(())
}

#[tokio::test]
async fn stream_all_utxos_empty_db() -> Result<()> {
    let ts = mk_store("stream:empty", 64).await?;

    let (tx, rx) = mpsc::sync_channel(16);
    let store_clone = ts.store.clone();
    let producer = std::thread::spawn(move || store_clone.stream_all_utxos(tx));

    let mut count = 0usize;
    while let Ok(_item) = rx.recv() {
        count += 1;
    }
    let producer_count = producer.join().unwrap()?;

    println!("  empty DB: producer_count={producer_count}, consumer_count={count}");
    assert_eq!(count, 0);
    assert_eq!(producer_count, 0);
    println!("  PASS: stream_all_utxos on empty DB returns 0");
    Ok(())
}

#[tokio::test]
async fn stream_all_utxos_receiver_dropped_early() -> Result<()> {
    let ts = mk_store("stream:drop", 64).await?;

    // Seed 1000 UTXOs
    let cf_utxo = ts.store.cf("utxo");
    for i in 0..1000u32 {
        let key = format!("tx_{:06}#0", i);
        let val = format!(r#"{{"addr":"addr","amt":"{i}.0"}}"#);
        ts.store
            .db
            .put_cf(&cf_utxo, key.as_bytes(), val.as_bytes())?;
    }

    // Tiny buffer, drop receiver after 5 items
    let (tx, rx) = mpsc::sync_channel(2);
    let store_clone = ts.store.clone();
    let producer = std::thread::spawn(move || store_clone.stream_all_utxos(tx));

    let mut received = 0;
    for _ in 0..5 {
        if rx.recv().is_ok() {
            received += 1;
        }
    }
    drop(rx);
    println!("  received {received} items before dropping receiver");

    // Producer should complete without panic
    let result = producer.join().unwrap();
    assert!(result.is_ok(), "producer should not error on disconnected channel");
    let sent = result.unwrap();
    println!("  producer sent {sent} items before channel closed");
    assert!(sent >= received, "producer should have sent at least what we consumed");
    assert!(sent < 1000, "producer should have stopped before iterating all 1000");
    println!("  PASS: producer stops gracefully on receiver drop");
    Ok(())
}
