use anyhow::Result;
use pms_config::load_config;
use pms_core::Dag;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::{ephemeral_addr, spawn_node_generic_rocks_with_seed};
use pms_types::PayloadEnvelope;
use pms_utils::compute_block_id;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
use tempfile::tempdir;
use tokio::time::{Duration, Instant, sleep};

#[tokio::test]
async fn bootstrap_tips_then_fetch_chain_rocks() -> Result<()> {
    // 1) chemins DB éphémères + adresses
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    let db_a = dir_a.path().join("rocks-A");
    let db_b = dir_b.path().join("rocks-B");

    let api_a = ephemeral_addr();
    let bind_a = ephemeral_addr();

    let api_b = ephemeral_addr();
    let bind_b = ephemeral_addr();
    let tip_limit = 1000;

    // Use unique seeds to avoid loopback detection
    let seed_a = [1u8; 32];
    let seed_b = [2u8; 32];

    println!("[TEST] spawn A @ {bind_a}");

    // Derive coordinator PK from A's seed
    let a_wallet = Wallet::from_seed(&seed_a, None).unwrap();
    let coord_pk = a_wallet.encoded_public_key();

    let (a_store, _a_dag, a_adapter, _a_srv, _a_jh) = spawn_node_generic_rocks_with_seed(
        db_a.to_string_lossy().as_ref(),
        "pms:test:A",
        bind_a.as_str(),
        api_a.as_str(),
        tip_limit,
        None,
        Some(seed_a),
        Some(coord_pk.clone()),
        false, // A (miner) doesn't need to enforce parents on its own blocks technically
    )
    .await?;

    println!("[TEST] spawn B @ {bind_b}");
    let (b_store, b_dag, _b_adapter, b_srv, _b_jh) = spawn_node_generic_rocks_with_seed(
        db_b.to_string_lossy().as_ref(),
        "pms:test:B",
        bind_b.as_str(),
        api_b.as_str(),
        tip_limit,
        None,
        Some(seed_b),
        Some(coord_pk),
        true, // B MUST enforce parents to trigger sync!
    )
    .await?;

    // Petit délai: listeners prêts
    println!("[TEST] sleep 150ms (startup listeners)");
    sleep(Duration::from_millis(150)).await;

    // 2) A “mine” une chaîne locale (payload=None) via l’ADAPTER (pas via dag.lock())
    let want = 12usize;
    println!("[TEST] A va miner {want} blocs…");

    let settings = load_config().expect("settings");
    let meta = WireMeta::from(&settings);

    let wallet = Wallet::from_seed(&[1u8; 32], None).unwrap();
    let signer_pk = wallet.encoded_public_key();

    // 🔧 FIX: Track last block to create linear chain (Single Writer mode)
    let genesis_id = "5b4540e1509aed5f49e56689e10849385f5ce1c115c4ca354890ed952552e2e5";
    let mut last_block_id = genesis_id.to_string();

    for i in 0..want {
        // 🔧 FIX: Use last block as parent (linear chain)
        let parents = vec![last_block_id.clone()];
        println!("[TEST] Mining #{} with parent {:?}", i, parents);

        // 2.2) Pas de payload pour ce test
        let payload: Option<PayloadEnvelope> = None;
        let payload_json = payload.as_ref().and_then(|p| serde_json::to_string(p).ok());

        // 2.3) WireBlock unsigned (id vide pour l'instant)
        let mut wb = pms_wire::WireBlock {
            id: String::new(), // on va le remplir juste après
            parents: parents.clone(),
            payload_json,
            nonce: i as u64,
            network_id: meta.network_id.clone(),
            protocol_version: meta.protocol_version as u16,
            signer_pk_hex: signer_pk.clone(),
            signature_hex: String::new(),
            metadata: None,
        };

        // 2.4) Calcul ID comme en prod (il fait partie du message signé)
        wb.id = compute_block_id(
            &wb.parents,
            &wb.payload_json
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok()),
            wb.nonce,
        );

        // 2.5) Message canonique + signature ECDSA
        //
        // IMPORTANT : on signe APRES que tous les champs stables du bloc
        // (id, parents, payload_json, nonce, network_id, protocol_version,
        //  signer_pk_hex) soient fixés. C’est exactement ce que
        // verify_block_signature() va re-hasher côté nœud.
        let msg = canonical_wireblock_message(&wb);
        let sig_b64 = wallet
            .sign(&msg)
            .map_err(|e| anyhow::anyhow!("sign error: {e:?}"))?;
        wb.signature_hex = sig_b64;

        // 2.6) Persist via l’adapter (validation + store + DAG interne)
        let res = a_adapter.persist_block(&wb).await?;
        assert!(
            matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
            "persist_block doit insérer ou idempoter, got={res:?}"
        );

        println!("[TEST] [A] mined #{i} id={}", wb.id);

        // 🔧 FIX: Update last block for next iteration
        last_block_id = wb.id.clone();
    }

    // Wait for background persist task to flush all blocks to RocksDB
    // The persist_block uses fire-and-forget async persistence
    println!("[TEST] waiting for background persist to complete...");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Vérif côté A
    let ids = a_store.all_block_ids().await?;
    let a_count = ids.len();
    println!(
        "[TEST] après minage: A_count={a_count} (incl. genesis). IDs: {:?}",
        ids
    );

    // 3) Connecte B -> A : handshake → GetTips → GetBlock
    println!("[TEST] B.connect({bind_a})");
    b_srv.connect(bind_a.as_str()).await?;
    println!("[TEST] B connecté à A, rattrapage en cours…");

    // 4) Poll B jusqu'au rattrapage
    // 🔧 FIX: Count from RAM DAG (updated immediately) not RocksDB (updated async)
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut tick = 0usize;
    loop {
        let got = b_dag.len(); // Count from RAM DAG (includes blocks not yet persisted)
        if got >= want + 1
        /* +genesis */
        {
            println!("[TEST] B a rattrapé: got={got} (>= {})", want + 1);
            break;
        }
        tick += 1;
        if tick % 10 == 0 {
            let a_now = a_store.all_block_ids().await?.len();
            println!(
                "[TEST] [poll #{tick}] A_count={a_now}, B_DAG_count={got} (attend {}+genesis)",
                want
            );
        }
        if Instant::now() >= deadline {
            let got_final = b_dag.len();
            println!(
                "[TEST][TIMEOUT] état final: B_DAG_count={got_final}, want>={}",
                want + 1
            );
            panic!("B n'a pas rattrapé");
        }
        sleep(Duration::from_millis(50)).await;
    }

    // Laisse finir les fetchs en vol
    sleep(Duration::from_millis(100)).await;

    Ok(())
}
