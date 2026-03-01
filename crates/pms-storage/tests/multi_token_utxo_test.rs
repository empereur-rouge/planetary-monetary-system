// crates/pms-storage/tests/multi_token_utxo_test.rs
//
// Tests pour le stockage UTXO multi-token (RocksDB) :
// - Mint de tokens custom (UtxoApply avec asset_id)
// - Transfer de tokens entre addresses
// - Rétrocompatibilité des UTXOs PMS natif
// - get_utxo avec asset_id

use anyhow::Result;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::rocks_store::utxo::UtxoApply;
use std::sync::Arc;
use tempfile::{TempDir, tempdir};

struct TestStore {
    _dir: TempDir,
    pub store: Arc<RocksStore>,
}

async fn mk_store(prefix: &str) -> Result<TestStore> {
    let dir = tempdir()?;
    let path = dir.path().join(format!("rocks-mt-{}", nanoid::nanoid!(5)));
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_string_lossy().to_string();
    let store = Arc::new(RocksStore::new(&path_str, 64, prefix, None).await?);
    Ok(TestStore { _dir: dir, store })
}

// ═══════════════════════════════════════════════════════════════════════════════
// Mint de tokens custom via UtxoApply
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mint_edenite_creates_utxo_with_asset_id() -> Result<()> {
    let ts = mk_store("mint:eden").await?;

    // Simuler un bloc Mint qui crée 1000 EDEN pour Alice
    let mint = UtxoApply {
        txid: "mint_eden_001".into(),
        inputs: vec![], // Mint n'a pas d'inputs
        outputs: vec![(
            "Alice".into(),
            "1000.00000000".into(),
            Some("edenite".into()),
        )],
    };

    let ok = ts.store.utxo_apply_tx_atomic(&mint).await?;
    assert!(ok, "mint should succeed");

    // Vérifier que l'UTXO est bien stocké avec l'asset_id
    let utxo = ts.store.get_utxo("mint_eden_001", 0)?;
    assert!(utxo.is_some(), "UTXO should exist");

    let utxo = utxo.unwrap();
    assert_eq!(utxo.address, "Alice");
    assert_eq!(utxo.amount, "1000.00000000");
    assert_eq!(utxo.asset_id, Some("edenite".into()));

    Ok(())
}

#[tokio::test]
async fn mint_pms_native_has_no_asset_id() -> Result<()> {
    let ts = mk_store("mint:pms").await?;

    let mint = UtxoApply {
        txid: "mint_pms_001".into(),
        inputs: vec![],
        outputs: vec![("Bob".into(), "50.00000000".into(), None)],
    };

    let ok = ts.store.utxo_apply_tx_atomic(&mint).await?;
    assert!(ok);

    let utxo = ts.store.get_utxo("mint_pms_001", 0)?.unwrap();
    assert_eq!(utxo.asset_id, None, "PMS UTXO should have no asset_id");

    Ok(())
}

#[tokio::test]
async fn mint_multiple_tokens_in_one_tx() -> Result<()> {
    let ts = mk_store("mint:multi").await?;

    // Un bloc Mint qui crée du PMS + EDEN en même temps
    let mint = UtxoApply {
        txid: "mint_multi_001".into(),
        inputs: vec![],
        outputs: vec![
            ("Alice".into(), "10.00000000".into(), None), // PMS
            (
                "Alice".into(),
                "500.00000000".into(),
                Some("edenite".into()),
            ), // EDEN
            ("Bob".into(), "200.0000".into(), Some("gold".into())), // GOLD
        ],
    };

    let ok = ts.store.utxo_apply_tx_atomic(&mint).await?;
    assert!(ok);

    let u0 = ts.store.get_utxo("mint_multi_001", 0)?.unwrap();
    assert_eq!(u0.asset_id, None);
    assert_eq!(u0.amount, "10.00000000");

    let u1 = ts.store.get_utxo("mint_multi_001", 1)?.unwrap();
    assert_eq!(u1.asset_id, Some("edenite".into()));
    assert_eq!(u1.amount, "500.00000000");

    let u2 = ts.store.get_utxo("mint_multi_001", 2)?.unwrap();
    assert_eq!(u2.asset_id, Some("gold".into()));
    assert_eq!(u2.amount, "200.0000");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Transfer de tokens custom
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn transfer_edenite_between_users() -> Result<()> {
    let ts = mk_store("xfer:eden").await?;

    // 1) Mint initial : Alice reçoit 100 EDEN
    let mint = UtxoApply {
        txid: "mint_001".into(),
        inputs: vec![],
        outputs: vec![(
            "Alice".into(),
            "100.00000000".into(),
            Some("edenite".into()),
        )],
    };
    assert!(ts.store.utxo_apply_tx_atomic(&mint).await?);

    // 2) Transfer : Alice envoie 60 EDEN à Bob, 40 EDEN change
    let transfer = UtxoApply {
        txid: "tx_001".into(),
        inputs: vec![("mint_001".into(), 0)],
        outputs: vec![
            ("Bob".into(), "60.00000000".into(), Some("edenite".into())),
            ("Alice".into(), "40.00000000".into(), Some("edenite".into())),
        ],
    };
    let ok = ts.store.utxo_apply_tx_atomic(&transfer).await?;
    assert!(ok, "transfer should succeed");

    // L'ancien UTXO doit être consommé
    assert!(
        ts.store.get_utxo("mint_001", 0)?.is_none(),
        "spent UTXO should be gone"
    );

    // Nouveaux UTXOs
    let bob_utxo = ts.store.get_utxo("tx_001", 0)?.unwrap();
    assert_eq!(bob_utxo.address, "Bob");
    assert_eq!(bob_utxo.amount, "60.00000000");
    assert_eq!(bob_utxo.asset_id, Some("edenite".into()));

    let alice_change = ts.store.get_utxo("tx_001", 1)?.unwrap();
    assert_eq!(alice_change.address, "Alice");
    assert_eq!(alice_change.amount, "40.00000000");
    assert_eq!(alice_change.asset_id, Some("edenite".into()));

    Ok(())
}

#[tokio::test]
async fn transfer_mixed_assets_in_one_tx() -> Result<()> {
    let ts = mk_store("xfer:mix").await?;

    // Seed: Alice a PMS + EDEN
    let mint_pms = UtxoApply {
        txid: "mint_pms".into(),
        inputs: vec![],
        outputs: vec![("Alice".into(), "5.00000000".into(), None)],
    };
    let mint_eden = UtxoApply {
        txid: "mint_eden".into(),
        inputs: vec![],
        outputs: vec![(
            "Alice".into(),
            "100.00000000".into(),
            Some("edenite".into()),
        )],
    };
    assert!(ts.store.utxo_apply_tx_atomic(&mint_pms).await?);
    assert!(ts.store.utxo_apply_tx_atomic(&mint_eden).await?);

    // Transfer: Alice envoie EDEN + paye fee en PMS
    let transfer = UtxoApply {
        txid: "tx_mixed".into(),
        inputs: vec![("mint_pms".into(), 0), ("mint_eden".into(), 0)],
        outputs: vec![
            ("Bob".into(), "80.00000000".into(), Some("edenite".into())), // EDEN to Bob
            ("Alice".into(), "20.00000000".into(), Some("edenite".into())), // EDEN change
            ("FeePool".into(), "0.10000000".into(), None),                // fee PMS
            ("Alice".into(), "4.90000000".into(), None),                  // PMS change
        ],
    };
    let ok = ts.store.utxo_apply_tx_atomic(&transfer).await?;
    assert!(ok);

    // Vérifier les résultats
    let bob_eden = ts.store.get_utxo("tx_mixed", 0)?.unwrap();
    assert_eq!(bob_eden.asset_id, Some("edenite".into()));
    assert_eq!(bob_eden.amount, "80.00000000");

    let fee = ts.store.get_utxo("tx_mixed", 2)?.unwrap();
    assert_eq!(fee.asset_id, None, "fee should be PMS native");
    assert_eq!(fee.address, "FeePool");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Double-spend de tokens custom
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn double_spend_edenite_rejected() -> Result<()> {
    let ts = mk_store("dblspend").await?;

    // Mint 100 EDEN
    let mint = UtxoApply {
        txid: "mint_ds".into(),
        inputs: vec![],
        outputs: vec![(
            "Alice".into(),
            "100.00000000".into(),
            Some("edenite".into()),
        )],
    };
    assert!(ts.store.utxo_apply_tx_atomic(&mint).await?);

    // Premier spend : OK
    let tx1 = UtxoApply {
        txid: "tx1".into(),
        inputs: vec![("mint_ds".into(), 0)],
        outputs: vec![("Bob".into(), "100.00000000".into(), Some("edenite".into()))],
    };
    assert!(ts.store.utxo_apply_tx_atomic(&tx1).await?);

    // Deuxième spend du même UTXO : rejeté
    let tx2 = UtxoApply {
        txid: "tx2".into(),
        inputs: vec![("mint_ds".into(), 0)],
        outputs: vec![(
            "Charlie".into(),
            "100.00000000".into(),
            Some("edenite".into()),
        )],
    };
    let ok = ts.store.utxo_apply_tx_atomic(&tx2).await?;
    assert!(!ok, "double-spend should be rejected");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Rétrocompatibilité : UTXOs stockés sans asset_id
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn legacy_utxo_without_asset_id_field() -> Result<()> {
    let ts = mk_store("legacy").await?;

    // Simuler un UTXO stocké par une ancienne version (sans champ "ast")
    let cf_utxo = ts.store.cf("utxo");
    ts.store
        .db
        .put_cf(&cf_utxo, b"old_tx:0", br#"{"addr":"Alice","amt":"42.0"}"#)?;

    // get_utxo doit fonctionner et retourner asset_id = None
    let utxo = ts.store.get_utxo("old_tx", 0)?.unwrap();
    assert_eq!(utxo.address, "Alice");
    assert_eq!(utxo.amount, "42.0");
    assert_eq!(
        utxo.asset_id, None,
        "legacy UTXO sans 'ast' doit avoir asset_id=None"
    );

    Ok(())
}

#[tokio::test]
async fn utxo_with_asset_id_serialized_correctly() -> Result<()> {
    let ts = mk_store("ser:ast").await?;

    // Stocker via UtxoApply (avec asset_id)
    let mint = UtxoApply {
        txid: "ast_tx".into(),
        inputs: vec![],
        outputs: vec![("Alice".into(), "100.0".into(), Some("edenite".into()))],
    };
    assert!(ts.store.utxo_apply_tx_atomic(&mint).await?);

    // Lire le raw JSON pour vérifier le format
    let cf_utxo = ts.store.cf("utxo");
    let raw = ts.store.db.get_cf(&cf_utxo, b"ast_tx:0")?.unwrap();
    let json_str = String::from_utf8(raw)?;

    assert!(
        json_str.contains("\"ast\":\"edenite\""),
        "JSON should contain ast field: {json_str}"
    );

    // Stocker sans asset_id : le champ "ast" ne doit pas apparaître
    let mint_pms = UtxoApply {
        txid: "pms_tx".into(),
        inputs: vec![],
        outputs: vec![("Bob".into(), "10.0".into(), None)],
    };
    assert!(ts.store.utxo_apply_tx_atomic(&mint_pms).await?);

    let raw_pms = ts.store.db.get_cf(&cf_utxo, b"pms_tx:0")?.unwrap();
    let json_pms = String::from_utf8(raw_pms)?;
    assert!(
        !json_pms.contains("ast"),
        "PMS UTXO should NOT contain 'ast' field: {json_pms}"
    );

    Ok(())
}
