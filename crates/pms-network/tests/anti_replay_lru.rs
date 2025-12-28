use anyhow::Result;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration, Instant};
use pms_config::load_config;
use pms_network::messages::NetMsg;
use pms_storage::{DagStorage, PutResult};
use pms_storage::rocks_store::store::RocksStore;
use pms_testkit::{ephemeral_addr, spawn_node_generic_rocks, wait_for_listen};
use pms_types::{Block, PayloadEnvelope};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};

// Attendre que le nombre de blocs n’évolue plus pendant quiet_ms (best effort)
async fn wait_store_stable(store: &Arc<RocksStore>, quiet_ms: u64, timeout_ms: u64) -> anyhow::Result<usize> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut last = store.all_block_ids().await?.len();
    let mut last_change = Instant::now();

    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let cur = store.all_block_ids().await?.len();
        if cur != last {
            last = cur;
            last_change = Instant::now();
        }
        if Instant::now().duration_since(last_change).as_millis() as u64 >= quiet_ms {
            return Ok(cur);
        }
        if Instant::now() >= deadline {
            return Ok(cur); // on sort quand même (best effort)
        }
    }
}
#[tokio::test]
async fn duplicate_block_is_deduped_by_lru_cache_rocks() -> Result<()> {
    // --- DBs éphémères pour A et B ---
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    let db_a = dir_a
        .path()
        .join("rocks-lru-a")
        .to_string_lossy()
        .to_string();
    let db_b = dir_b
        .path()
        .join("rocks-lru-b")
        .to_string_lossy()
        .to_string();

    // --- Adresses P2P (ports différents) ---
    let api_a = ephemeral_addr();
    let bind_a = ephemeral_addr();

    let api_b = ephemeral_addr();
    let bind_b = ephemeral_addr();

    // --- Spawn nodes Rocks ---
    let tip_limit = 256usize;
    let (a_store, a_dag, a_adapter, a_srv, _jh_a) =
        spawn_node_generic_rocks(&db_a, "it:lrutest:a", bind_a.as_str(), api_a.as_str(), tip_limit, None).await?;
    let (b_store, b_dag, _b_adapter, b_srv, _jh_b) =
        spawn_node_generic_rocks(&db_b, "it:lrutest:b", bind_b.as_str(), api_b.as_str(), tip_limit, None).await?;

    // --- Forcer un genesis en RAM dans les deux DAG pour éviter "genesis manquant" ---

    // Crée un genesis canonique
    let genesis = Block::genesis(compute_block_id);
    let genesis_id = genesis.id.clone();

    {
        let mut dag_a = a_dag.lock().await;
        if dag_a.blocks.is_empty() {
            dag_a.blocks.insert(genesis_id.clone(), genesis.clone());
        }
    }

    {
        let mut dag_b = b_dag.lock().await;
        if dag_b.blocks.is_empty() {
            dag_b.blocks.insert(genesis_id.clone(), genesis.clone());
        }
    }

    // --- Attendre que les deux serveurs écoutent vraiment ---
    wait_for_listen(bind_a.as_str(), 1_000).await?;
    wait_for_listen(bind_b.as_str(), 1_000).await?;

    // B se connecte à A
    b_srv.connect(bind_a.as_str()).await?;

    // Attendre que B se stabilise (rattrapage initial / tips)
    let initial_b = wait_store_stable(&b_store, /*quiet_ms*/ 200, /*timeout_ms*/ 2_000).await?;

    // --- Prépare meta & wallet pour signer le bloc de test ---
    let settings = load_config().expect("settings load failed");
    let mut meta = WireMeta::from(&settings);

    if meta.network_id.is_empty() {
        meta.network_id = "pms-test".into();
    }
    if meta.protocol_version == 0 {
        meta.protocol_version = 1;
    }

    let wallet = Wallet::from_seed(&[1u8; 32], None)
        .expect("Wallet::from_seed ne doit pas fail en test");
    let signer_pk = wallet.encoded_public_key();

    // --- A "mine" un bloc et le persiste via l'adapter ---

    // Parents = notre genesis injecté
    let parents = vec![genesis_id.clone()];

    // 1) Pas de payload pour le test (payload=None)
    let payload: Option<PayloadEnvelope> = None;
    let payload_json = payload
        .as_ref()
        .and_then(|p| serde_json::to_string(p).ok());

    // 2) WireBlock "unsigned" (id calculé juste après)
    let mut wb = WireBlock {
        id: String::new(), // rempli après
        parents: parents.clone(),
        payload_json,
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: signer_pk.clone(),
        signature_hex: String::new(),
    };

    // 3) Calcul de l’ID comme en prod
    wb.id = compute_block_id(
        &wb.parents,
        &wb.payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        wb.nonce,
    );

    // 4) Canonical message + signature (avec id déjà fixé)
    let msg = canonical_wireblock_message(&wb);
    let sig_b64 = wallet
        .sign(&msg)
        .map_err(|e| anyhow::anyhow!("sign error: {e:?}"))?;
    wb.signature_hex = sig_b64;

    // 5) Persistance via l'adapter (chemin complet : validation + store + DAG interne)
    let res = a_adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "persist_block doit insérer ou idempoter, got={res:?}"
    );

    // --- A diffuse deux fois le même bloc ---
    let net = NetMsg::Block {
        id: wb.id.clone(),
        parents: wb.parents.clone(),
        payload_json: wb.payload_json.clone(),
        nonce: wb.nonce,
        network_id: wb.network_id.clone(),
        protocol_version: wb.protocol_version,
        signer_pk_hex: wb.signer_pk_hex.clone(),
        signature_hex: wb.signature_hex.clone(),
    };

    a_srv.broadcast(&net).await?;
    a_srv.broadcast(&net).await?;

    // --- Attendre que B voie (au plus) UNE insertion ---
    let want = initial_b + 1;
    let deadline = Instant::now() + Duration::from_secs(3);

    loop {
        let now = b_store.all_block_ids().await?.len();
        if now >= want {
            break;
        }
        if Instant::now() >= deadline {
            panic!("B n'a pas vu le bloc: got={}, expected>={}", now, want);
        }
        sleep(Duration::from_millis(50)).await;
    }

    // Vérifier qu’il n’y a eu **qu’une** insertion (doublon absorbé)
    let final_b = b_store.all_block_ids().await?.len();
    assert_eq!(
        final_b, want,
        "le doublon ne doit pas être persisté (LRU + idempotence actives)"
    );

    Ok(())
}