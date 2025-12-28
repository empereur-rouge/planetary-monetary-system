use anyhow::Result;
use tempfile::tempdir;
use tokio::time::{sleep, Duration, Instant};

use pms_network::messages::NetMsg;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::{ephemeral_addr, forge_signed_wire_block_for_test, spawn_node_generic_rocks, test_meta_and_wallet};
use pms_types::{Block, PayloadEnvelope};
use pms_utils::compute_block_id;
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;

#[tokio::test]
async fn two_nodes_share_blocks_debug_rocks() -> Result<()> {
    let api_a  = ephemeral_addr();
    let bind_a = ephemeral_addr();

    let api_b  = ephemeral_addr();
    let bind_b = ephemeral_addr();
    let tip_limit = 256usize;

    // 0) Genesis partagé
    let shared_genesis = Block::genesis(compute_block_id);
    println!("[TEST] shared genesis id={}", shared_genesis.id);

    // 1) Crée deux dossiers Rocks éphémères
    let dir_a = tempdir()?;
    let db_a   = dir_a.path().join("rocks-a");
    std::fs::create_dir_all(&db_a)?;
    let db_a_str = db_a.to_string_lossy();

    let dir_b = tempdir()?;
    let db_b   = dir_b.path().join("rocks-b");
    std::fs::create_dir_all(&db_b)?;
    let db_b_str = db_b.to_string_lossy();

    // 2) Meta réseau + wallet de test pour signer les blocs
    let (meta, wallet) = test_meta_and_wallet();

    // 3) Spawn A et B avec le même genesis persisté
    println!("[TEST] spawn A @ {bind_a}");
    let (a_store, _a_dag, a_adapter, a_srv, _ha) =
        spawn_node_generic_rocks(
            &db_a_str,
            "pms:test:a",
            bind_a.as_str(),
            api_a.as_str(),
            tip_limit,
            Some(&shared_genesis),
        ).await?;

    println!("[TEST] spawn B @ {bind_b}");
    let (b_store, _b_dag, _b_adapter, b_srv, _hb) =
        spawn_node_generic_rocks(
            &db_b_str,
            "pms:test:b",
            bind_b.as_str(),
            api_b.as_str(),
            tip_limit,
            Some(&shared_genesis),
        ).await?;

    // 4) Laisse le temps aux listeners de démarrer
    println!("[TEST] sleep 150ms (startup listeners)");
    sleep(Duration::from_millis(150)).await;

    // 5) Connecte B -> A
    println!("[TEST] B.connect({bind_a})");
    b_srv.connect(bind_a.as_str()).await?;
    println!("[TEST] B connected to A");

    // 6) Stats de départ (incl. genesis)
    let a0 = a_store.all_block_ids().await?.len();
    let b0 = b_store.all_block_ids().await?.len();
    println!("[TEST] initial counts: A={a0}, B={b0}");

    // 7) A mine et diffuse via l’adapter (prod-like, sans lock DAG)
    let n = 30usize;
    for i in 0..n {
        // 7.1 Parents depuis les tips de A
        let mut parents = a_store.top_tips(2).await?;
        if parents.is_empty() {
            let all = a_store.all_block_ids().await?;
            if let Some(first) = all.first() {
                parents.push(first.clone());
            }
        }
        parents.sort();
        parents.dedup();

        // 7.2 Pas de payload dans ce test
        let payload: Option<PayloadEnvelope> = None;

        // 7.3 WireBlock unsigned (id vide pour l’instant)
        let mut wb = forge_signed_wire_block_for_test(
            parents.clone(),
            &meta,
            &wallet,
            i as u64 + 1,
            payload
        );

        // 7.4 Calcul de l’ID comme en prod
        // L’ID dépend uniquement de (parents, payload_json, nonce).
        // On le fige AVANT de créer le message signé.
        wb.id = compute_block_id(
            &wb.parents,
            &wb.payload_json
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok()),
            wb.nonce,
        );

        // 7.5 Message canonique + signature
        //
        // IMPORTANT : canonical_wireblock_message(&wb) doit voir exactement
        // les mêmes champs que verify_block_signature utilisera :
        //   - id
        //   - parents
        //   - payload_json
        //   - nonce
        //   - network_id
        //   - protocol_version
        //   - signer_pk_hex
        // (signature_hex est exclu du message signé).
        let msg = canonical_wireblock_message(&wb);
        let sig_b64 = wallet
            .sign(&msg)
            .map_err(|e| anyhow::anyhow!("sign error: {e:?}"))?;
        wb.signature_hex = sig_b64;

        // 7.6 Persistance via l’adapter (valide + store + DAG interne)
        let res = a_adapter.persist_block(&wb).await?;
        assert!(
            matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
            "persist_block doit insérer ou être idempotent, got={res:?}"
        );

        println!("[TEST] [A] mined #{i} id={}", wb.id);

        // 7.7 Diffusion réseau
        a_srv
            .broadcast(&NetMsg::Block {
                id: wb.id.clone(),
                parents: wb.parents.clone(),
                payload_json: wb.payload_json.clone(),
                nonce: wb.nonce,
                network_id: wb.network_id,
                protocol_version: wb.protocol_version,
                signature_hex: wb.signature_hex,
                signer_pk_hex: wb.signer_pk_hex,
            })
            .await?;

        sleep(Duration::from_millis(10)).await;
    }

    // 8) Poll B jusqu’à n+1 (genesis + n blocs)
    println!("[TEST] start polling B for {} blocks (+genesis)", n);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let len = b_store.all_block_ids().await?.len();
        println!("[TEST] [poll] B has {len} blocks");
        if len >= n + 1 {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "B n'a pas persisté assez de blocs: got={len}, expected>={}",
                n + 1
            );
        }
        sleep(Duration::from_millis(100)).await;
    }

    println!("[TEST] ✅ OK: B a bien reçu >= {} blocs (incl. genesis)", n + 1);
    Ok(())
}