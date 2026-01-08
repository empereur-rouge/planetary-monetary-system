// pms-network/tests/inv_roundtrip.rs
use anyhow::Result;
use tempfile::tempdir;
use tokio::time::{Duration, Instant, sleep};

use pms_storage::{DagStorage, PutResult};
use pms_testkit::{
    ephemeral_addr, forge_signed_wire_block_for_test, spawn_node_generic_rocks_with_seed,
    test_meta_and_wallet, wait_for_listen,
};
use pms_types::PayloadEnvelope;
use pms_utils::compute_block_id;
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
// pour get_block()

#[tokio::test]
async fn inv_triggers_getblock_and_blocks_flow_rocks() -> Result<()> {
    // DB éphémères pour A et B
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    let db_a = dir_a.path().join("rocks-inv-A");
    let db_b = dir_b.path().join("rocks-inv-B");

    let api_a = ephemeral_addr();
    let bind_a = ephemeral_addr();

    let api_b = ephemeral_addr();
    let bind_b = ephemeral_addr();
    let tip_limit = 256usize;

    // spawn nœud A (bind=p2p & api identiques pour les tests)
    // Use unique seeds to avoid loopback detection
    let seed_a = [1u8; 32];
    let seed_b = [2u8; 32];

    let (a_store, _a_dag, a_adapter, a_srv, _a_jh) = spawn_node_generic_rocks_with_seed(
        db_a.to_string_lossy().as_ref(),
        "pms:test:inv:A",
        bind_a.as_str(),
        api_a.as_str(),
        tip_limit,
        None,
        Some(seed_a),
    )
    .await?;
    // spawn nœud B
    let (b_store, _b_dag, _b_adapter, b_srv, _b_jh) = spawn_node_generic_rocks_with_seed(
        db_b.to_string_lossy().as_ref(),
        "pms:test:inv:B",
        bind_b.as_str(),
        api_b.as_str(),
        tip_limit,
        None,
        Some(seed_b),
    )
    .await?;

    wait_for_listen(bind_a.as_str(), 1_000).await?;
    wait_for_listen(bind_b.as_str(), 1_000).await?;

    // Connexion B -> A (handshake P2P)
    b_srv.connect(bind_a.as_str()).await?;

    // Meta réseau + wallet de test
    let (meta, wallet) = test_meta_and_wallet();

    // ===== Mine 1 bloc sur A (persist + RAM via adapter, signé) =====

    // 1) Choisir des parents depuis les tips du store A
    let mut parents = a_store.top_tips(2).await?;
    if parents.is_empty() {
        // fallback: n'importe quel bloc existant (typiquement genesis)
        let all = a_store.all_block_ids().await?;
        if let Some(first) = all.first() {
            parents.push(first.clone());
        }
    }
    parents.sort();
    parents.dedup();

    // 2) Pas de payload pour ce test
    let payload: Option<PayloadEnvelope> = None;
    let payload_json = payload.as_ref().and_then(|p| serde_json::to_string(p).ok());

    // 3) WireBlock unsigned (id vide pour l’instant)
    let wb = forge_signed_wire_block_for_test(parents.clone(), &meta, &wallet, 1, payload);

    // 4) Persistance via l’adapter (validation + store + DAG interne)
    let res = a_adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "persist_block doit insérer ou être idempotent, got={res:?}"
    );

    // 5) Persistance via l’adapter (validation + store + DAG interne)
    let res = a_adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "persist_block doit insérer ou être idempotent, got={res:?}"
    );

    // ===== Broadcast du bloc depuis A (le serveur émettra aussi Inv) =====
    a_srv
        .broadcast(&pms_network::messages::NetMsg::Block {
            id: wb.id.clone(),
            parents: wb.parents.clone(),
            payload_json: wb.payload_json.clone(),
            nonce: wb.nonce,
            network_id: wb.network_id,
            protocol_version: wb.protocol_version,
            signature_hex: wb.signature_hex,
            signer_pk_hex: wb.signer_pk_hex,
            metadata: wb.metadata.clone(),
        })
        .await?;

    // ===== Attendre que B récupère ce bloc (via Inv -> GetBlock -> Block) =====
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if b_store.get_block(&wb.id).await?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("B n’a pas fetch le bloc annoncé via Inv");
        }
        sleep(Duration::from_millis(40)).await;
    }

    Ok(())
}
