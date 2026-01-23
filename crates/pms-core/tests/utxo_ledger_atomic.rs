// crates/pms-core/tests/utxo_ledger_atomic.rs

use std::sync::Arc;
// use tokio::sync::Mutex; // REMOVED

use anyhow::Result;
use tempfile::tempdir;

use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::{Block, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput};
use pms_utils::compute_block_id;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};
use rust_decimal::Decimal;

#[tokio::test]
async fn mint_then_tx_are_persisted_consistently_in_rocks_and_dag() -> Result<()> {
    // 1) Crée le wallet AVANT la config
    let wallet = Wallet::from_seed(&[1u8; 32], None).expect("Wallet::from_seed ne doit pas fail");

    let admin_pk = wallet.encoded_public_key();

    // 2) Déclare ce wallet comme admin via l’ENV pour ce test
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pk);
    }

    // 3) Maintenant seulement on charge la config
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 4) RocksStore éphémère
    let dir = tempdir()?;
    let db_path = dir.path().join("rocks-utxo-atomic");
    let db_path_str = db_path.to_string_lossy().to_string();

    let store = Arc::new(
        RocksStore::new(
            &db_path_str,
            settings.rocks.tip_limit as usize,
            &settings.rocks.prefix,
            None,
        )
        .await?,
    );
    store.ensure_schema().await?;
    store.bootstrap_once_for_production()?;

    // Genesis si besoin
    let genesis = if store.all_block_ids().await?.is_empty() {
        let g = Block::genesis(compute_block_id);
        store.persist_genesis(&g, &meta).await?;
        g
    } else {
        // In this test we start empty so this branch not strictly needed but good practice
        Block::genesis(compute_block_id)
    };

    // 5) Bootstrap DAG + adapter
    // Since we know we just started (or added genesis), we can init with genesis
    let dag_loaded = ConcurrentDag::new_with_genesis(genesis.clone());
    let dag = Arc::new(dag_loaded);

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // 5) Bloc Mint : 1 output de 10 PMS
    let mint_outputs = vec![TxOutput {
        address: "addr-mint-test".to_string(),
        amount: "10".to_string(),
    }];

    let mint_payload = PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: mint_outputs.clone(),
    });

    // Parents = tips actuels (typiquement genesis)
    let mut parents = store.top_tips(2).await?;
    if parents.is_empty() {
        parents.push(genesis.id.clone());
    }

    let wb_mint =
        forge_signed_wire_block_for_test(parents.clone(), &meta, &wallet, 1, Some(mint_payload));

    let before_mint_store = store.all_block_ids().await?.len();
    let before_mint_dag = dag.blocks.len();

    let res_mint = adapter.persist_block(&wb_mint).await?;
    assert!(
        matches!(res_mint, PutResult::Inserted | PutResult::AlreadyExists),
        "Mint doit être accepté, obtenu: {res_mint:?}"
    );

    // Wait for background persist
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let after_mint_store = store.all_block_ids().await?.len();
    let after_mint_dag = dag.blocks.len();

    assert_eq!(
        after_mint_store,
        before_mint_store + 1,
        "Mint doit ajouter un block dans Rocks"
    );
    assert_eq!(
        after_mint_dag,
        before_mint_dag + 1,
        "Mint doit ajouter un block dans le DAG"
    );

    // 6) Bloc TxUtxo qui dépense la sortie [mint, index=0] vers un nouvel output

    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: wb_mint.id.clone(),
                index: 0,
            },
        }],
        outputs: vec![TxOutput {
            address: "addr-dest-test".to_string(),
            amount: "9".to_string(),
        }],
        fee: "1".to_string(),
        unlocks: Vec::new(),
    };

    let tx_payload = PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx));

    let mut parents_tx = store.top_tips(2).await?;
    if parents_tx.is_empty() {
        parents_tx.push(wb_mint.id.clone());
    }

    let wb_tx = forge_signed_wire_block_for_test(parents_tx, &meta, &wallet, 2, Some(tx_payload));

    let before_tx_store = store.all_block_ids().await?.len();
    let before_tx_dag = dag.blocks.len();

    let res_tx = adapter.persist_block(&wb_tx).await?;
    assert!(
        matches!(res_tx, PutResult::Inserted | PutResult::AlreadyExists),
        "TxUtxo valide doit être acceptée, obtenu: {res_tx:?}"
    );

    // Wait for background persist
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let after_tx_store = store.all_block_ids().await?.len();
    let after_tx_dag = dag.blocks.len();

    assert_eq!(
        after_tx_store,
        before_tx_store + 1,
        "TxUtxo doit ajouter un block dans Rocks (aucun demi-état)"
    );
    assert_eq!(
        after_tx_dag,
        before_tx_dag + 1,
        "TxUtxo doit ajouter un block dans le DAG (cohérent avec Rocks)"
    );

    Ok(())
}

#[tokio::test]
async fn invalid_tx_does_not_mutate_rocks_nor_dag() -> anyhow::Result<()> {
    // 0) Wallet admin + déclaration dans la config
    let admin_wallet =
        Wallet::from_seed(&[1u8; 32], None).expect("Wallet::from_seed ne doit pas échouer");
    let admin_pk = admin_wallet.encoded_public_key();

    // On déclare ce pubkey comme admin dans la config pour ce test
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pk);
    }

    // 1) Config + meta
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 2) Store éphémère
    let dir = tempdir()?;
    let db_path = dir.path().join("rocks-utxo-atomic-invalid-tx");
    let db_path_str = db_path.to_string_lossy().to_string();

    let store = Arc::new(
        RocksStore::new(
            &db_path_str,
            settings.rocks.tip_limit as usize,
            &settings.rocks.prefix,
            None,
        )
        .await?,
    );
    store.ensure_schema().await?;
    store.bootstrap_once_for_production()?;

    // 3) Genesis si besoin
    let genesis = if store.all_block_ids().await?.is_empty() {
        let g = Block::genesis(pms_utils::compute_block_id);
        store.persist_genesis(&g, &meta).await?;
        g
    } else {
        Block::genesis(pms_utils::compute_block_id)
    };

    // 4) Bootstrap DAG + adapter
    let dag_loaded = ConcurrentDag::new_with_genesis(genesis);
    let dag = Arc::new(dag_loaded);
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // ============================================================
    // 1) MINT VALIDE (admin) → crée un vrai UTXO
    // ============================================================

    // Parents = tips ou fallback premier bloc
    let mut parents = store.top_tips(2).await?;
    if parents.is_empty() {
        let all = store.all_block_ids().await?;
        if let Some(first) = all.first() {
            parents.push(first.clone());
        }
    }
    parents.sort();
    parents.dedup();

    // Un seul output, 100 PMS
    let mint_outputs = vec![TxOutput {
        address: "admin-address-for-test".to_string(),
        amount: "100".to_string(),
    }];

    let mint_payload = PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: mint_outputs.clone(),
    });
    let mint_payload_json = Some(serde_json::to_string(&mint_payload)?);

    // WireBlock pour MINT
    let mut wb_mint = WireBlock {
        id: String::new(),
        parents: parents.clone(),
        payload_json: mint_payload_json,
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: admin_pk.clone(),
        signature_hex: String::new(),
        metadata: None,
    };

    // ID + signature MINT
    wb_mint.id = pms_utils::compute_block_id(
        &wb_mint.parents,
        &wb_mint
            .payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        wb_mint.nonce,
    );
    let msg_mint = canonical_wireblock_message(&wb_mint);
    let sig_mint = admin_wallet
        .sign(&msg_mint)
        .map_err(|e| anyhow::anyhow!("sign error mint: {e:?}"))?;
    wb_mint.signature_hex = sig_mint;

    // Persistance MINT
    let res_mint = adapter.persist_block(&wb_mint).await?;
    assert!(
        matches!(res_mint, PutResult::Inserted | PutResult::AlreadyExists),
        "Mint doit être accepté, obtenu: {res_mint:?}"
    );

    // Snapshot Rocks + DAG après MINT (état de référence)
    let blocks_before = store.all_block_ids().await?;
    let dag_before_len = dag.blocks.len();

    // ============================================================
    // 2) TX UTXO AVEC FEE TROP ÉLEVÉE
    //    → DOIT ÊTRE REJETÉE SANS MUTATION ROCKS/DAG
    // ============================================================

    // On dépense la sortie 0 du mint
    let input = TxInput {
        out: OutputId {
            txid: wb_mint.id.clone(),
            index: 0,
        },
    };

    // On envoie 50 PMS et on met une fee volontairement énorme
    let tx = Transaction {
        inputs: vec![input],
        outputs: vec![TxOutput {
            address: "some-recipient".into(),
            amount: "50".into(),
        }],
        fee: "1000000000000".into(),
        unlocks: vec![],
    };

    let tx_payload = PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx));
    let tx_payload_json = Some(serde_json::to_string(&tx_payload)?);

    // Parents = tips actuels
    let mut parents_tx = store.top_tips(2).await?;
    if parents_tx.is_empty() {
        parents_tx.push(wb_mint.id.clone());
    }
    parents_tx.sort();
    parents_tx.dedup();

    let mut wb_tx = WireBlock {
        id: String::new(),
        parents: parents_tx,
        payload_json: tx_payload_json,
        nonce: 2,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: admin_pk.clone(),
        signature_hex: String::new(),
        metadata: None,
    };

    // ID + signature TX
    wb_tx.id = pms_utils::compute_block_id(
        &wb_tx.parents,
        &wb_tx
            .payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        wb_tx.nonce,
    );
    let msg_tx = canonical_wireblock_message(&wb_tx);
    let sig_tx = admin_wallet
        .sign(&msg_tx)
        .map_err(|e| anyhow::anyhow!("sign error tx: {e:?}"))?;
    wb_tx.signature_hex = sig_tx;

    // Persistance TX → doit être rejetée (plusieurs raisons possibles: fee trop haute, fonds insuffisants, etc.)
    let res_tx = adapter.persist_block(&wb_tx).await?;
    match res_tx {
        PutResult::Rejected(reason) => {
            // Accept any rejection - the important thing is that the TX was rejected
            assert!(
                reason.contains("FeeTooHigh")
                    || reason.contains("fee")
                    || reason.contains("insuffisants")
                    || reason.contains("insufficient"),
                "on attend un rejet (fee ou fonds), reason='{reason}'"
            );
        }
        other => {
            panic!("la TX invalide ne doit pas être acceptée, obtenu: {other:?}");
        }
    }

    // ============================================================
    // 3) Vérifier que Rocks + DAG n'ont pas bougé
    // ============================================================

    let blocks_after = store.all_block_ids().await?;
    let dag_after_len = dag.blocks.len();

    assert_eq!(
        blocks_after.len(),
        blocks_before.len(),
        "le nombre de blocs en Rocks ne doit pas changer après TX invalide"
    );
    assert_eq!(
        dag_after_len, dag_before_len,
        "la taille du DAG en RAM ne doit pas changer après TX invalide"
    );

    Ok(())
}
