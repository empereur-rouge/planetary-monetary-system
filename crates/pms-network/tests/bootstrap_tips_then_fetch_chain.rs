use anyhow::Result;
use tokio::{time::{sleep, Duration, Instant}};
use tempfile::tempdir;
use pms_config::load_config;
use pms_core::Dag;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::{ephemeral_addr, spawn_node_generic_rocks};
use pms_types::PayloadEnvelope;
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireMeta;

#[tokio::test]
async fn bootstrap_tips_then_fetch_chain_rocks() -> Result<()> {
    // 1) chemins DB éphémères + adresses
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    let db_a = dir_a.path().join("rocks-A");
    let db_b = dir_b.path().join("rocks-B");

    let api_a  = ephemeral_addr();
    let bind_a = ephemeral_addr();

    let api_b  = ephemeral_addr();
    let bind_b = ephemeral_addr();
    let tip_limit = 256usize;

    println!("[TEST] spawn A @ {bind_a}");
    let (a_store, _a_dag, a_adapter, _a_srv, _a_jh) =
        spawn_node_generic_rocks(
            db_a.to_string_lossy().as_ref(),
            "pms:test:A",
            bind_a.as_str(),
            api_a.as_str(),
            tip_limit,
            None,
        ).await?;

    println!("[TEST] spawn B @ {bind_b}");
    let (b_store, _b_dag, _b_adapter, b_srv, _b_jh) =
        spawn_node_generic_rocks(
            db_b.to_string_lossy().as_ref(),
            "pms:test:B",
            bind_b.as_str(),
            api_b.as_str(),
            tip_limit,
            None,
        ).await?;

    // Petit délai: listeners prêts
    println!("[TEST] sleep 150ms (startup listeners)");
    sleep(Duration::from_millis(150)).await;

    // 2) A “mine” une chaîne locale (payload=None) via l’ADAPTER (pas via dag.lock())
    let want = 12usize;
    println!("[TEST] A va miner {want} blocs…");

    let settings = load_config().expect("settings");
    let meta     = WireMeta::from(&settings);

    let wallet   = Wallet::from_seed(&[1u8; 32], None).unwrap();
    let signer_pk = wallet.encoded_public_key();

    for i in 0..want {
        // 2.1) Parents = tips du store, sinon fallback sur un id existant
        let mut parents = a_store.top_tips(2).await?;
        if parents.is_empty() {
            let all = a_store.all_block_ids().await?;
            if let Some(first) = all.first() {
                parents.push(first.clone());
            }
        }
        parents.sort();
        parents.dedup();

        // 2.2) Pas de payload pour ce test
        let payload: Option<PayloadEnvelope> = None;
        let payload_json = payload
            .as_ref()
            .and_then(|p| serde_json::to_string(p).ok());

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

        // 2.6) Persist via l’adapter (validation + store + DAG interne)
        let res = a_adapter.persist_block(&wb).await?;
        assert!(
            matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
            "persist_block doit insérer ou idempoter, got={res:?}"
        );

        println!("[TEST] [A] mined #{i} id={}", wb.id);
    }

    // Vérif côté A
    let a_count = a_store.all_block_ids().await?.len();
    println!("[TEST] après minage: A_count={a_count} (incl. genesis)");

    // 3) Connecte B -> A : handshake → GetTips → GetBlock
    println!("[TEST] B.connect({bind_a})");
    b_srv.connect(bind_a.as_str()).await?;
    println!("[TEST] B connecté à A, rattrapage en cours…");

    // 4) Poll B jusqu’au rattrapage
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut tick = 0usize;
    loop {
        let got = b_store.all_block_ids().await?.len();
        if got >= want + 1 /* +genesis */ {
            println!("[TEST] B a rattrapé: got={got} (>= {})", want + 1);
            break;
        }
        tick += 1;
        if tick % 10 == 0 {
            let a_now = a_store.all_block_ids().await?.len();
            println!(
                "[TEST] [poll #{tick}] A_count={a_now}, B_count={got} (attend {}+genesis)",
                want
            );
        }
        if Instant::now() >= deadline {
            let got_final = b_store.all_block_ids().await?.len();
            println!(
                "[TEST][TIMEOUT] état final: B_count={got_final}, want>={}",
                want + 1
            );
            panic!("B n’a pas rattrapé");
        }
        sleep(Duration::from_millis(50)).await;
    }

    // Laisse finir les fetchs en vol
    sleep(Duration::from_millis(100)).await;

    Ok(())
}