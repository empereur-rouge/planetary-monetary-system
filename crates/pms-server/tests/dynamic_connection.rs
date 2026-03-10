use pms_config::{ServerConfig, load_config};
use pms_core::ConcurrentDag;
use pms_interface::NetDagAdapter;
use pms_server::Server;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::Block;
use pms_wallet::{SignerBackend, Wallet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

async fn spawn_node(port: u16, seed: u8) -> Arc<Server> {
    // Manually build context to control wallet seed and tempdir lifetime
    let settings = load_config().expect("Config");

    // Use tempfile but persist it by converting to path
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.into_path().join(format!("rocks-test-{}", port));

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            settings.rocks.tip_limit as usize,
            &settings.rocks.prefix,
            None,
        )
        .await
        .expect("store"),
    );
    store.ensure_schema().await.expect("schema");
    store.bootstrap_once_for_production().expect("bootstrap");

    if store.all_block_ids().await.unwrap().is_empty() {
        let g = Block::genesis(pms_utils::compute_block_id);
        let meta = pms_wire::WireMeta::from(&settings);
        store.persist_genesis(&g, &meta).await.unwrap();
    }

    let dag = Arc::new(ConcurrentDag::bootstrap_from_store(&*store).await.unwrap());
    let adapter: Arc<dyn NetDagAdapter> = pms_core::CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    // UNIQUE WALLET PER NODE using seed
    // Using explicit seed ensures nodes have different IDs (prevent loopback detection)
    let node_wallet = Arc::new(Wallet::from_seed(&[seed; 32], None).unwrap());

    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
        None,
    );

    let server_cfg = Arc::new(ServerConfig {
        bind_addr: format!("127.0.0.1:{}", port),
        api_addr: format!("127.0.0.1:{}", port + 1000),
        tls: None,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    let srv_clone = srv.clone();
    let store_clone = store.clone();
    tokio::spawn(async move {
        if let Err(e) = srv_clone.run(server_cfg, store_clone).await {
            eprintln!("Server {} exited with error: {}", port, e);
        }
    });

    // Give it a moment to bind
    sleep(Duration::from_secs(1)).await;

    srv
}

#[tokio::test]
async fn test_dynamic_p2p_connection() {
    // Pick high ports to avoid conflicts
    let port1 = 19500;
    let port2 = 19501;

    // Use different seeds => different NodeIDs
    let node1 = spawn_node(port1, 1).await;
    let node2 = spawn_node(port2, 2).await;

    // Initially isolated
    assert!(node1.get_p2p_peers().is_empty());
    assert!(node2.get_p2p_peers().is_empty());

    println!("Creating dynamic connection Node2 -> Node1...");

    // Connect Node 2 to Node 1
    // CRITICAL: Clone node2 because connect_to_peer consumes Arc<Self>
    node2
        .clone()
        .connect_to_peer(format!("127.0.0.1:{}", port1), None)
        .await
        .expect("Failed to initiate connection");

    // Wait for handshake
    sleep(Duration::from_secs(2)).await;

    // Verify connections
    let peers1 = node1.get_p2p_peers();
    let peers2 = node2.get_p2p_peers();

    println!("Node 1 peers: {:?}", peers1);
    println!("Node 2 peers: {:?}", peers2);

    assert!(!peers1.is_empty(), "Node 1 should have incoming peer");
    assert!(!peers2.is_empty(), "Node 2 should have outgoing peer");

    println!("✅ Test Passed!");
}
