// crates/pms-server/tests/submit_block_auth.rs

use std::sync::Arc;

use anyhow::{Error, Result};
use pms_config::{ServerConfig, load_config};
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_storage::{PutResult, StoredBlock};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::{Block, TxOutput};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};
type DagRef = Arc<ConcurrentDag>;

fn to_wire(
    forged: &Block,
    meta: &WireMeta,
    signer_pk_hex: String,
    signature_hex: String,
) -> WireBlock {
    WireBlock {
        id: forged.id.clone(),
        parents: forged.parents.clone(),
        payload_json: serde_json::to_string(&forged.payload).ok(),
        nonce: forged.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex,
        signature_hex,
        metadata: None,
    }
}

fn mk_test_cfg(api_addr: &str) -> ServerConfig {
    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: api_addr.to_string(),
        tls: None,
        api_tls_enabled: false,
        network: load_config().unwrap().network,
        auth: load_config().unwrap().auth,
    };
    cfg
}

/// Forge un bloc minimal en RAM (Mint/Payload vide) sans passer par CLI.
async fn forge_test_block(dag: &DagRef) -> Block {
    dag.forge_block(None, 0, compute_block_id)
        .expect("forge test block")
}

fn block_to_unsigned_wire(b: &Block, network_id: &str, protocol_version: u16) -> WireBlock {
    WireBlock {
        id: b.id.clone(),
        parents: b.parents.clone(),
        payload_json: serde_json::to_string(&b.payload).ok(),
        nonce: b.nonce,
        network_id: network_id.to_string(),
        protocol_version,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    }
}

/// Test 1: un bloc NON signé doit être rejeté par l'adapter.
#[tokio::test]
async fn unsigned_block_is_rejected() -> anyhow::Result<()> {
    // 1) Store temporaire + DAG + adapter réel
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-signature-test");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256, // tip_limit test
            "pms:test",
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag_loaded = ConcurrentDag::new_with_genesis(genesis);
    let dag: DagRef = Arc::new(dag_loaded);

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    // 2) Forge un bloc simple (payload None) avec les fonctions existantes
    let block = dag.forge_block(
        None,
        0, // difficulté test
        compute_block_id,
    )?;

    // 3) Meta cohérente avec la config actuelle
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // 4) On a besoin d'un wallet juste pour le champ signer_pk_hex
    let wallet =
        Wallet::from_seed(&[1u8; 32], None).expect("Wallet::from_seed ne doit pas fail en test");

    // 5) Construction du WireBlock **NON signé**
    let wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: serde_json::to_string(&block.payload).ok(),
        nonce: block.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: wallet.encoded_public_key(),
        signature_hex: String::new(), // <- PAS DE SIGNATURE
        metadata: None,
    };

    // 6) Persist via l'adapter (chemin réel DAG + Rocks)
    let res = adapter.persist_block(&wb).await?;

    match res {
        PutResult::Rejected(reason) => {
            // On s'assure que le rejet vient bien de la signature / auth
            let r = reason.to_lowercase();
            assert!(
                r.contains("signature") || r.contains("sign") || r.contains("auth"),
                "rejet pour mauvaise raison: {reason}"
            );
        }
        other => panic!("Un bloc non signé doit être rejeté, obtenu: {:?}", other),
    }

    Ok(())
}

#[tokio::test]
async fn signed_plain_block_is_accepted() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-signed-plain");

    let store =
        Arc::new(RocksStore::new(db_path.to_string_lossy().as_ref(), 256, "pms:test", None, &RocksMemoryConfig::default()).await?);

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag_loaded = ConcurrentDag::new_with_genesis(genesis);
    let dag: DagRef = Arc::new(dag_loaded);

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    // 1) Forge bloc (payload None) via Dag
    let block = dag.forge_block(None, 0, compute_block_id)?;

    // 2) Meta + wallet de test
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let wallet =
        Wallet::from_seed(&[2u8; 32], None).expect("Wallet::from_seed ne doit pas fail en test");

    // 3) WireBlock "unsigned" avec la *clé publique* en hex
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet,
        block.nonce,
        block.payload,
    );

    // 5) Persist via l’adapter (chemin réel : vérif réseau + vérif signature + store + DAG)
    let res = adapter.persist_block(&wb).await?;

    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "bloc signé devrait être accepté, obtenu: {:?}",
        res
    );

    Ok(())
}

#[tokio::test]
async fn signed_encrypted_mint_is_accepted() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-signed-mint");

    let store =
        Arc::new(RocksStore::new(db_path.to_string_lossy().as_ref(), 256, "pms:test", None, &RocksMemoryConfig::default()).await?);

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag_loaded = ConcurrentDag::new_with_genesis(genesis);
    let dag: DagRef = Arc::new(dag_loaded);

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let wallet = Wallet::from_seed(&[3u8; 32], None).map_err(|e| anyhow::anyhow!("{}", e))?;

    // 1) Payload Mint clair
    let addr = wallet.get_address(&settings.address.hrp);
    let plain = PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: addr,
            amount: "1000".to_string(),
            asset_id: None,
        }],
    };

    // 2) Chiffrement (tes fonctions existantes)
    let recipients = vec![wallet.x25519_pub_hex.clone()];
    let enc =
        EncryptedPayload::encrypt_for_plain(&plain, &recipients).map_err(|e| anyhow::anyhow!(e))?;

    let payload = Some(PayloadEnvelope::Encrypted(enc));

    // 3) Forge un bloc avec ce payload
    let block = dag.forge_block(payload, 0, compute_block_id)?;

    // 4) WireBlock + signature
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet,
        block.nonce,
        block.payload,
    );

    // 5) Persist
    let res = adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "mint chiffré signé devrait être accepté, obtenu: {:?}",
        res
    );

    Ok(())
}
