//! Integration test for Network Batching (no Docker required)
//!
//! Vérifie le VRAI worker d'agrégation (`spawn_broadcast_worker` /
//! `flush_broadcast_buffer` dans `server/broadcast.rs`) en branchant un faux peer
//! INBOUND via un duplex en mémoire et en lisant les messages `Inv` réellement
//! émis sur le fil. Les Inv ne passent PAS par le `NetDagAdapter` (ils vont vers
//! `self.broadcast(NetMsg::Inv)` → channel du peer), donc observer l'adapter ne
//! prouve rien — il faut lire le socket du peer.
//!
//! Ce que les tests prouvent maintenant (avant v0.9.3 : 0 assertion, théâtre) :
//! 1. **Complétude** : chaque id enqueue apparaît exactement une fois dans un Inv.
//! 2. **Flush par taille** : 1000 ids → ≥ 10 batches (max_batch = 100), aucun > 100.
//! 3. **Flush temporel + survie aux ticks vides** : aucun flush à vide, puis un id
//!    isolé est bien flushé après plusieurs ticks oisifs.
//! 4. **Concurrence** : 500 ids depuis 10 tâches arrivent tous, sans perte ni doublon.

use anyhow::Result;
use pms_interface::NetDagAdapter;
use pms_network::messages::NetMsg;
use pms_server::Server;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_storage::{DagStorage, PutResult, StoredBlock};
use pms_wallet::Wallet;
use pms_wire::WireBlock;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::{Duration, sleep};

/// Mock adapter minimal — les Inv ne passent pas par lui, il sert juste à
/// satisfaire `Server::new`.
struct MockAdapter {
    store: Arc<RocksStore>,
}

#[async_trait::async_trait]
impl NetDagAdapter for MockAdapter {
    async fn have_block(&self, id: &str) -> bool {
        self.store.get_block(id).await.ok().flatten().is_some()
    }

    async fn top_tips(&self, limit: usize) -> Result<Vec<String>> {
        self.store.top_tips(limit).await
    }

    async fn persist_block(&self, wb: &WireBlock) -> Result<PutResult> {
        let sb = StoredBlock::from(wb.clone());
        self.store.put_block(&sb).await
    }

    async fn get_block(&self, id: &str) -> Result<Option<WireBlock>> {
        Ok(self.store.get_block(id).await?.map(WireBlock::from))
    }

    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>> {
        self.store.get_blocks_by_ids(ids).await
    }

    async fn broadcast_block(&self, _wb: &WireBlock) -> Result<()> {
        Ok(())
    }

    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>> {
        self.store
            .all_block_ids()
            .await
            .map(|ids| ids.into_iter().take(limit).collect())
    }

    fn min_pow_leading_zero_bits(&self) -> u8 {
        0 // No PoW required for tests
    }

    async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
        (rust_decimal::Decimal::ZERO, 0)
    }

    async fn circulating_supply_by_asset(
        &self,
        _asset_id: Option<&str>,
    ) -> (rust_decimal::Decimal, u64) {
        (rust_decimal::Decimal::ZERO, 0)
    }

    async fn balance_by_address(&self, _address: &str) -> rust_decimal::Decimal {
        rust_decimal::Decimal::ZERO
    }

    async fn balance_by_address_and_asset(
        &self,
        _address: &str,
        _asset_id: Option<&str>,
    ) -> rust_decimal::Decimal {
        rust_decimal::Decimal::ZERO
    }

    async fn utxos_by_address(
        &self,
        _address: &str,
    ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
        Vec::new()
    }

    async fn add_utxo(
        &self,
        _txid: String,
        _index: u32,
        _address: String,
        _amount: String,
        _asset_id: Option<String>,
    ) {
    }

    async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
        false
    }

    async fn get_utxo(&self, _output_id: &pms_types::OutputId) -> Option<pms_types::TxOutput> {
        None
    }
}

/// Construit un serveur P2P avec son worker de broadcast (Server::new le spawn).
async fn make_server() -> Result<Arc<Server>> {
    let temp_dir = TempDir::new()?;
    let store = Arc::new(
        RocksStore::new(
            temp_dir.path().to_str().unwrap(),
            100,
            "test",
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    // On laisse vivre le TempDir le temps du test (leak volontaire pour le test).
    std::mem::forget(temp_dir);
    let adapter: Arc<dyn NetDagAdapter> = Arc::new(MockAdapter { store });
    // NB: `P2pConfig::default()` (Default dérivé) met `per_peer_queue_cap = 0`,
    // ce qui fait paniquer `mpsc::channel(0)` à la connexion d'un peer. En prod
    // la valeur vient du défaut serde (2000) — ici on la fixe explicitement.
    let p2p = pms_config::P2pConfig {
        per_peer_queue_cap: 2_000,
        max_connections: 256,
        ..Default::default()
    };
    Ok(Server::new(
        adapter,
        "test-network",
        1,
        Arc::new(Wallet::generate()),
        &p2p,
        None,
    ))
}

/// Branche un faux peer INBOUND via un duplex en mémoire, complète le handshake
/// (le serveur envoie `Hello` → on répond `HelloAck{ok:true}`), et collecte les
/// batches `Inv` reçus. Chaque entrée du Vec retourné = les `ids` d'un Inv.
async fn attach_inv_collector(srv: &Arc<Server>, peer_port: u16) -> Arc<Mutex<Vec<Vec<String>>>> {
    let (client, server_side) = tokio::io::duplex(4 * 1024 * 1024);
    let (sr, sw) = tokio::io::split(server_side);
    let fake_addr: SocketAddr = format!("127.0.0.1:{peer_port}").parse().unwrap();
    srv.handle_new_peer_from_io(sr, sw, fake_addr, true)
        .await
        .expect("attach inbound peer");

    let batches: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let batches_w = batches.clone();
    tokio::spawn(async move {
        let (cr, mut cw) = tokio::io::split(client);
        let mut lines = BufReader::new(cr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<NetMsg>(&line) {
                Ok(NetMsg::Hello { .. }) => {
                    // Complète le handshake pour rester enregistré au-delà du
                    // HANDSHAKE_TIMEOUT (sinon le serveur nous retire des peers).
                    let ack = serde_json::to_string(&NetMsg::HelloAck {
                        ok: true,
                        reason: None,
                    })
                    .unwrap();
                    if cw.write_all(ack.as_bytes()).await.is_err() {
                        break;
                    }
                    if cw.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
                Ok(NetMsg::Inv { ids }) => {
                    batches_w.lock().unwrap().push(ids);
                }
                _ => { /* GetTips / Pong / etc. — non pertinent ici */ }
            }
        }
    });

    // Laisse le handshake se compléter avant que l'appelant n'enqueue.
    sleep(Duration::from_millis(40)).await;
    batches
}

/// Aplatit tous les ids reçus à travers tous les batches Inv.
fn all_ids(batches: &Arc<Mutex<Vec<Vec<String>>>>) -> Vec<String> {
    batches
        .lock()
        .unwrap()
        .iter()
        .flat_map(|b| b.iter().cloned())
        .collect()
}

/// Test 1 — Complétude : 10 ids enqueue rapidement → tous reçus exactement une fois.
#[tokio::test]
async fn batching_delivers_all_enqueued_ids_exactly_once() -> Result<()> {
    let srv = make_server().await?;
    let invs = attach_inv_collector(&srv, 40001).await;

    for i in 0..10 {
        srv.enqueue_broadcast(format!("block_{i}")).await;
    }
    sleep(Duration::from_millis(80)).await;

    let received = all_ids(&invs);
    let batch_sizes: Vec<usize> = invs.lock().unwrap().iter().map(|b| b.len()).collect();
    println!(
        "✅ enqueued 10 ids → {} batch(es), sizes={:?}, total received={}",
        batch_sizes.len(),
        batch_sizes,
        received.len()
    );

    let expected: HashSet<String> = (0..10).map(|i| format!("block_{i}")).collect();
    let got: HashSet<String> = received.iter().cloned().collect();
    assert_eq!(
        received.len(),
        10,
        "each id must be flushed exactly once (no loss/dup)"
    );
    assert_eq!(got, expected, "all 10 enqueued ids must arrive in Inv batches");
    assert!(
        invs.lock().unwrap().iter().all(|b| b.len() <= 100),
        "no batch may exceed max_batch=100"
    );
    Ok(())
}

/// Test 2 — Flush par taille : 1000 ids → ≥ 10 batches, aucun > 100.
#[tokio::test]
async fn batching_flushes_on_size_threshold() -> Result<()> {
    let srv = make_server().await?;
    let invs = attach_inv_collector(&srv, 40002).await;

    for i in 0..1000 {
        srv.enqueue_broadcast(format!("hl_{i}")).await;
    }
    sleep(Duration::from_millis(250)).await;

    let received = all_ids(&invs);
    let batches = invs.lock().unwrap();
    let sizes: Vec<usize> = batches.iter().map(|b| b.len()).collect();
    println!(
        "✅ enqueued 1000 ids → {} batches, total received={}, max batch={:?}",
        batches.len(),
        received.len(),
        sizes.iter().max()
    );

    let got: HashSet<String> = received.iter().cloned().collect();
    assert_eq!(received.len(), 1000, "all 1000 ids delivered exactly once");
    assert_eq!(got.len(), 1000, "no duplicate ids");
    assert!(
        batches.iter().all(|b| b.len() <= 100),
        "no batch may exceed max_batch=100, got sizes {sizes:?}"
    );
    // 1000 ids répartis en batches de ≤ 100 ⇒ au moins 10 batches : prouve que le
    // flush par TAILLE s'est déclenché (sinon un seul gros batch).
    assert!(
        batches.len() >= 10,
        "1000 ids with max 100/batch require >= 10 batches (size-based flush), got {}",
        batches.len()
    );
    Ok(())
}

/// Test 3 — Flush temporel + survie aux ticks vides : aucun flush à vide, puis un
/// id isolé est bien flushé après plusieurs ticks oisifs.
#[tokio::test]
async fn batching_survives_idle_ticks_and_flushes_single_id() -> Result<()> {
    let srv = make_server().await?;
    let invs = attach_inv_collector(&srv, 40003).await;

    // Plusieurs ticks de 10ms sans rien enqueue : le buffer vide ne doit PAS flush.
    sleep(Duration::from_millis(60)).await;
    assert!(
        invs.lock().unwrap().is_empty(),
        "empty buffer must never emit an Inv"
    );

    srv.enqueue_broadcast("after_empty_tick".to_string()).await;
    sleep(Duration::from_millis(40)).await;

    let received = all_ids(&invs);
    println!("✅ after idle ticks, single enqueue → received={received:?}");
    assert_eq!(
        received,
        vec!["after_empty_tick".to_string()],
        "worker must survive idle ticks and still flush the single id (time-based flush)"
    );
    Ok(())
}

/// Test 4 — Concurrence : 500 ids depuis 10 tâches arrivent tous, sans perte ni doublon.
#[tokio::test]
async fn batching_no_loss_under_concurrent_enqueue() -> Result<()> {
    let srv = make_server().await?;
    let invs = attach_inv_collector(&srv, 40004).await;

    let mut handles = vec![];
    for task_id in 0..10 {
        let srv_clone = srv.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..50 {
                srv_clone
                    .enqueue_broadcast(format!("task{task_id}_block_{i}"))
                    .await;
            }
        }));
    }
    for h in handles {
        h.await?;
    }
    sleep(Duration::from_millis(200)).await;

    let received = all_ids(&invs);
    let got: HashSet<String> = received.iter().cloned().collect();
    let mut expected: HashSet<String> = HashSet::new();
    for task_id in 0..10 {
        for i in 0..50 {
            expected.insert(format!("task{task_id}_block_{i}"));
        }
    }
    println!(
        "✅ 10 tasks × 50 enqueue → received={} (unique={}), batches={}",
        received.len(),
        got.len(),
        invs.lock().unwrap().len()
    );

    assert_eq!(received.len(), 500, "exactly 500 ids delivered (no loss)");
    assert_eq!(got.len(), 500, "no duplicate ids under concurrency");
    assert_eq!(got, expected, "every enqueued id from every task must arrive");
    assert!(
        invs.lock().unwrap().iter().all(|b| b.len() <= 100),
        "no batch may exceed max_batch=100"
    );
    Ok(())
}
