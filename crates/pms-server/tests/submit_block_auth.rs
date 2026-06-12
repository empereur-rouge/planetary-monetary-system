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

/// Helper : store Rocks éphémère + DAG genesis + adapter réel (policy globale).
async fn fresh_adapter(tag: &str) -> Result<(DagRef, Arc<dyn NetDagAdapter>, WireMeta)> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join(format!("rocks-{tag}"));
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:test",
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    std::mem::forget(dir); // garde le tempdir vivant pour la durée du test
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store, 0, None);
    Ok((dag, adapter, meta))
}

/// Test : une signature PRÉSENTE mais INVALIDE (bien formée, mais sur un autre
/// message) doit atteindre le vérificateur crypto et être rejetée.
///
/// NB (v0.9.3) : tous les tests précédents envoyaient une signature VIDE, qui
/// court-circuite `verify_block_signature` (rejet "missing signature" avant la
/// crypto). Le vrai vérificateur n'était jamais exercé sur une signature
/// mal-mais-présente — exactement ce qu'un attaquant enverrait.
#[tokio::test]
async fn tampered_block_signature_is_rejected() -> Result<()> {
    let (dag, adapter, meta) = fresh_adapter("tampered-sig").await?;
    let wallet = Wallet::from_seed(&[4u8; 32], None).map_err(|e| anyhow::anyhow!("{e}"))?;

    let block = dag.forge_block(None, 0, compute_block_id)?;
    let mut wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet,
        block.nonce,
        block.payload,
    );
    // Remplace par une signature BIEN FORMÉE mais portant sur un autre message :
    // elle ne vérifiera pas contre le message canonique du bloc.
    wb.signature_hex = wallet
        .sign("an entirely different message")
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let res = adapter.persist_block(&wb).await?;
    println!("tampered-signature persist → {res:?}");
    match res {
        PutResult::Rejected(reason) => {
            let r = reason.to_lowercase();
            assert!(
                r.contains("signature") || r.contains("sign") || r.contains("auth"),
                "must be rejected by the signature verifier, got: {reason}"
            );
        }
        other => panic!("present-but-invalid signature must be rejected, got: {other:?}"),
    }
    Ok(())
}

/// Test : une signature valide mais attribuée à une AUTRE clé publique
/// (signer_pk_hex usurpé) doit être rejetée — le vérificateur lie la signature
/// à la clé annoncée (le message canonique inclut `signer_pk_hex`).
#[tokio::test]
async fn signature_with_swapped_pubkey_is_rejected() -> Result<()> {
    let (dag, adapter, meta) = fresh_adapter("swapped-pk").await?;
    let real_signer = Wallet::from_seed(&[5u8; 32], None).map_err(|e| anyhow::anyhow!("{e}"))?;
    let imposter = Wallet::from_seed(&[6u8; 32], None).map_err(|e| anyhow::anyhow!("{e}"))?;

    let block = dag.forge_block(None, 0, compute_block_id)?;
    let mut wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &real_signer,
        block.nonce,
        block.payload,
    );
    // Usurpation : on remplace la clé publique annoncée par celle de l'imposteur.
    // La signature (faite par real_signer) ne vérifiera pas contre imposter_pk.
    wb.signer_pk_hex = imposter.encoded_public_key();

    let res = adapter.persist_block(&wb).await?;
    println!("swapped-pubkey persist → {res:?}");
    match res {
        PutResult::Rejected(reason) => {
            let r = reason.to_lowercase();
            assert!(
                r.contains("signature") || r.contains("sign") || r.contains("auth"),
                "swapped-pubkey block must be rejected by the verifier, got: {reason}"
            );
        }
        other => panic!("signature/pubkey mismatch must be rejected, got: {other:?}"),
    }
    Ok(())
}

/// Test : un Mint PLAIN signé par une clé NON autorisée (pas dans
/// `admin.signer_pubkeys` de la config dev) est rejeté au niveau persist_block.
///
/// NB (v0.9.3) : couvre le gate d'autorité de mint via le VRAI chemin persist
/// (`validate_mint_policy` → `validate_mint_security` dans persist.rs:238-257),
/// pas seulement les fonctions unitaires. On assert la RAISON pour prouver que
/// le rejet vient bien du contrôle d'autorité (et non d'un montant/parent/etc.).
#[tokio::test]
async fn non_authorized_mint_is_rejected_by_persist() -> Result<()> {
    let (dag, adapter, meta) = fresh_adapter("unauth-mint").await?;

    // Wallet aléatoire — N'EST PAS dans admin.signer_pubkeys de config.dev.toml.
    let attacker = Wallet::from_seed(&[42u8; 32], None).map_err(|e| anyhow::anyhow!("{e}"))?;
    let addr = attacker.get_address("8e");

    let plain = PlainPayload::Mint {
        outputs: vec![TxOutput::new(addr, "1000".to_string(), None)],
    };
    let block = dag.forge_block(Some(PayloadEnvelope::Plain(plain)), 0, compute_block_id)?;
    // Signature VALIDE de l'attaquant (donc le rejet n'est PAS dû à la crypto,
    // mais bien au contrôle d'autorité de mint).
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &attacker,
        block.nonce,
        block.payload,
    );

    let res = adapter.persist_block(&wb).await?;
    println!("non-authorized mint persist → {res:?}");
    match res {
        PutResult::Rejected(reason) => {
            let r = reason.to_lowercase();
            assert!(
                r.contains("mint") && (r.contains("policy") || r.contains("unauthor") || r.contains("security")),
                "rejection must come from the mint-authority gate, got: {reason}"
            );
        }
        other => panic!("unauthorized mint must be rejected by persist, got: {other:?}"),
    }
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
        outputs: vec![TxOutput::new(addr, "1000".to_string(), None)],
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
