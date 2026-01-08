use anyhow::Result;
use pms_config::{Auth, Network, NetworkMode, ServerConfig};
use pms_core::{CoreAdapter, concurrent_dag::ConcurrentDag};
use pms_interface::NetDagAdapter;
use pms_server::Server;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::sleep;

use pms_types::Block;
use std::str::FromStr;

// Import shared stress test utilities
mod stress_common;
use stress_common::{get_balance, mine_mint, send_tx, spam_transactions};

async fn spawn_node(port_p2p: u16, port_api: u16) -> (Arc<Server>, TempDir, String, String) {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().to_str().unwrap().to_string();

    // Init Store
    // Fix: Remove .into() to let &str generic work or implicit coercion
    let store = Arc::new(RocksStore::new(&db_path, 1000, "test").await.unwrap());
    store.ensure_schema().await.unwrap();

    // Genesis
    let ids = store.all_block_ids().await.unwrap();
    if ids.is_empty() {
        let g = Block::genesis(compute_block_id);
        let meta = WireMeta {
            network_id: "pms-test".into(),
            protocol_version: 1,
        };
        store.persist_genesis(&g, &meta).await.unwrap();
    }

    // DAG & Adapter
    let dag = Arc::new(ConcurrentDag::bootstrap_from_store(&*store).await.unwrap());
    // Bootstrap UTXOs
    let core_adapter = CoreAdapter::new(dag.clone(), store.clone());
    core_adapter.bootstrap_utxos().await.unwrap();
    // Helper coercion via explicit typing - remove Arc::new wrapping
    let adapter: Arc<dyn NetDagAdapter> = core_adapter;

    // Wallet
    let wallet = Arc::new(Wallet::generate());

    // Server
    let srv = Server::new(adapter, "pms-test", 1, wallet);

    // Config
    let bind_addr = format!("127.0.0.1:{}", port_p2p);
    let api_addr = format!("127.0.0.1:{}", port_api);

    let cfg = Arc::new(ServerConfig {
        bind_addr: bind_addr.clone(),
        api_addr: api_addr.clone(),
        tls: None, // No TLS for local test
        network: Network {
            mode: NetworkMode::Dev,
            network_id: "pms-test".into(),
            protocol_version: 1,
        },
        auth: Auth {
            require_signed_submit: false,
            admin_api_token: None,
            allowed_ips: vec![],
        },
    });

    // Spawn Run
    let srv_clone = srv.clone();
    let cfg_clone = cfg.clone();
    let store_clone = store.clone();
    tokio::spawn(async move {
        srv_clone.run(cfg_clone, store_clone).await.unwrap();
    });

    // Wait for API to be ready
    let client = Client::new();
    let url = format!("http://{}", api_addr);
    let mut ready = false;
    for _ in 0..150 {
        if let Ok(resp) = client.get(format!("{}/ready", url)).send().await {
            if resp.status().is_success() {
                ready = true;
                break;
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "Node on port {} failed to start", port_api);

    (srv, tmp_dir, format!("http://{}", api_addr), bind_addr)
}

#[tokio::test]
async fn local_stress_test() -> Result<()> {
    // 0. Setup Faucet & Env
    let faucet = Wallet::generate();
    let faucet_addr = faucet.get_address("8e");
    let faucet_pk = faucet.encoded_public_key();

    // 0b. Setup Admin Wallet for Fees (BEFORE unsafe block)
    let admin = Wallet::generate();
    let admin_addr = admin.get_address("8e");

    // Override config defaults to match test constants
    unsafe {
        std::env::set_var("PMS__NETWORK__NETWORK_ID", "pms-test");
        std::env::set_var("PMS__NETWORK__PROTOCOL_VERSION", "1");
        std::env::set_var("PMS__NETWORK__MODE", "dev");
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &faucet_pk);
        // Increase rate limits for stress test
        std::env::set_var("PMS__LIMITS__RATE_LIMIT_RPS", "10000");
        std::env::set_var("PMS__LIMITS__BURST", "20000");

        // Fee Split Config
        std::env::set_var("PMS__FEES__PLATFORM_ADDRESS", &admin_addr);
        std::env::set_var("PMS__FEES__PLATFORM_FEE_RATIO", "0.45");
    }

    // Re-create the wallet struct or just use the keys?
    // Wallet::generate() creates a random one. We want the SAME one.
    // So we should move the generation OUTSIDE the unsafe block but BEFORE spawn_node.

    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    // 0. Setup 3 nodes
    println!("🚀 Starting Local Stress Test (3 Nodes)");
    let (n1, _d1, url1, _p2p1) = spawn_node(40000, 40003).await;
    let (n2, _d2, url2, p2p2) = spawn_node(40001, 40004).await;
    let (n3, _d3, url3, p2p3) = spawn_node(40002, 40005).await;

    // Unused vars prefixed with _

    let nodes = vec![url1.clone(), url2.clone(), url3.clone()];
    let client = Client::builder().build()?;

    // 1. Connect Peers (Mesh)
    println!("🔗 Connecting peers...");
    n1.connect(&p2p2).await?;
    n1.connect(&p2p3).await?;
    n2.connect(&p2p3).await?; // Full mesh for robustness
    n3.connect(&p2p2).await?; // Explicit full mesh (N3->N2)
    // Wait for mesh
    sleep(Duration::from_secs(2)).await;

    // 2. Wallets
    // 2. Wallets
    // faucet already created at start
    println!("💰 Faucet: {}", faucet_addr);
    let alice = Wallet::generate();
    let bob = Wallet::generate();
    let carol = Wallet::generate();

    let users = vec![&alice, &bob, &carol];
    let user_addrs: Vec<String> = users.iter().map(|w| w.get_address("8e")).collect();

    let dest1 = Wallet::generate();
    let dest1_addr = dest1.get_address("8e");
    let dest2 = Wallet::generate();
    let dest2_addr = dest2.get_address("8e");
    let dest3 = Wallet::generate();
    let dest3_addr = dest3.get_address("8e");

    // Fee recipient (admin wallet) - Already generated at start
    println!("💰 Faucet: {}", &faucet_addr[..10]);

    // 3. Mint Initial Supply
    println!("\n📦 [Setup] Minting 20,000 PMS on Node 1...");
    let parents = vec![Block::genesis(compute_block_id).id];
    let mint_id = mine_mint(&client, &url1, &faucet, &faucet_addr, "20000.0", parents).await?;
    println!("   Mint Block: {}", mint_id);

    // Wait for propagation
    sleep(Duration::from_secs(3)).await;

    // 4. Distribute Funds
    println!("💸 [Setup] Distributing funds...");
    let mut last_parent = mint_id.clone();
    let mut utxo_txid = mint_id.clone();

    // TX1: Faucet -> Alice (5000), Change -> Faucet (15000)
    let (tx1_id, _) = send_tx(
        &client,
        &url1,
        &faucet,
        &utxo_txid,
        0,
        &user_addrs[0],
        "5000.0",
        &faucet_addr,
        "15000.0",
        vec![last_parent.clone()],
    )
    .await?;
    last_parent = tx1_id.clone();
    utxo_txid = tx1_id.clone();

    // TX2: Faucet -> Bob (5000), Change -> Faucet (10000)
    let (tx2_id, _) = send_tx(
        &client,
        &url1,
        &faucet,
        &utxo_txid,
        1,
        &user_addrs[1],
        "5000.0",
        &faucet_addr,
        "10000.0",
        vec![last_parent.clone()],
    )
    .await?;
    last_parent = tx2_id.clone();
    utxo_txid = tx2_id.clone();

    // TX3: Faucet -> Carol (5000), Change -> Faucet (5000)
    let (tx3_id, _) = send_tx(
        &client,
        &url1,
        &faucet,
        &utxo_txid,
        1,
        &user_addrs[2],
        "5000.0",
        &faucet_addr,
        "5000.0",
        vec![last_parent.clone()],
    )
    .await?;

    println!("✅ Distribution complete. Waiting 5s for sync...");
    sleep(Duration::from_secs(5)).await;

    // Verify Sync Success
    let alice_balance = get_balance(&client, &url1, &alice).await;
    println!("Alice Balance on Node 1: {}", alice_balance);
    if alice_balance == rust_decimal::Decimal::ZERO {
        panic!("❌ Node 1 failed to sync distribution transactions!");
    }
    let bob_balance = get_balance(&client, &url2, &bob).await;
    println!("Bob Balance on Node 2: {}", bob_balance);
    if bob_balance == rust_decimal::Decimal::ZERO {
        panic!("❌ Node 2 failed to sync distribution transactions!");
    }

    let carol_balance = get_balance(&client, &url3, &carol).await;
    println!("Carol Balance on Node 3: {}", carol_balance);
    if carol_balance == rust_decimal::Decimal::ZERO {
        panic!("❌ Node 3 failed to sync distribution transactions!");
    }

    // 5. Stress Test logic
    println!("\n🔥 [Stress] Starting 1000 Tx Traffic (333 per node)...");
    let mut handles = vec![];
    let payment_str = "1.0";
    let admin_address_ref = admin_addr.clone();

    // Helper closure to avoid lifetime issues
    let spawn_worker = |client: Client,
                        url: String,
                        user: Wallet,
                        start_tx: String,
                        start_idx: u32,
                        count: usize,
                        dest: String| {
        let payment = payment_str.to_string();
        let admin = admin_address_ref.to_string();
        tokio::spawn(async move {
            spam_transactions(
                client, url, user, start_tx, start_idx, count, &payment, &admin, dest,
            )
            .await
        })
    };

    handles.push(spawn_worker(
        client.clone(),
        url1.clone(),
        alice.clone(),
        tx1_id,
        0,
        333,
        dest1_addr.clone(),
    ));
    handles.push(spawn_worker(
        client.clone(),
        url2.clone(),
        bob.clone(),
        tx2_id,
        0,
        333,
        dest2_addr.clone(),
    ));
    handles.push(spawn_worker(
        client.clone(),
        url3.clone(),
        carol.clone(),
        tx3_id,
        0,
        334,
        dest3_addr.clone(),
    ));

    for h in handles {
        h.await??;
    }

    println!("✅ All 1000 transactions submitted.");
    println!("⏳ Waiting 10s for full network convergence...");
    sleep(Duration::from_secs(10)).await;

    // 6. Audit
    println!("\n🕵️  [Audit] Verifying Synchronization...");
    let mut attempts = 0;
    loop {
        attempts += 1;
        let mut all_synced = true;

        // Check consistency on 3 nodes for user wallets
        for wallet in [&alice, &bob, &carol, &dest1, &dest2, &dest3] {
            let b1 = get_balance(&client, &url1, wallet).await;
            let b2 = get_balance(&client, &url2, wallet).await;
            let b3 = get_balance(&client, &url3, wallet).await;

            if b1 != b2 || b2 != b3 {
                all_synced = false;
                println!(
                    "   ⚠️  Attempt {}: Syncing... Balances: {} {} {}",
                    attempts, b1, b2, b3
                );
                // Force break on mismatch to retry
                break;
            }
        }

        if all_synced {
            println!("✅ Network Fully Synchronized after {} attempts.", attempts);
            break;
        }

        if attempts >= 100 {
            println!("❌ Timeout waiting for sync.");
            break; // Validation below will fail
        }
        sleep(Duration::from_millis(500)).await;
    }

    // Checking final balances
    for (name, wallet) in [("Alice", &alice), ("Dest1", &dest1)] {
        let b1 = get_balance(&client, &url1, wallet).await;
        let b2 = get_balance(&client, &url2, wallet).await;
        let b3 = get_balance(&client, &url3, wallet).await;
        println!("   👤 {}: N1={}, N2={}, N3={}", name, b1, b2, b3);
        assert_eq!(b1, b2);
        assert_eq!(b2, b3);
    }

    // Verify admin received all fees
    let admin_b1 = get_balance(&client, &url1, &admin).await;
    let admin_b2 = get_balance(&client, &url2, &admin).await;
    let admin_b3 = get_balance(&client, &url3, &admin).await;
    println!(
        "   💰 Admin Fees: N1={}, N2={}, N3={}",
        admin_b1, admin_b2, admin_b3
    );
    // Expected: 1000 tx * 1.0 payment * 2.4% total fee = 24.0 Total Fees
    // Platform Share = 24.0 * 0.45 = 10.8
    let expected_fees = rust_decimal::Decimal::from_str("10.8").unwrap();
    assert_eq!(
        admin_b1, expected_fees,
        "Admin should have received 45% of fees (1000 tx * 0.024 * 0.45 = 10.8)"
    );
    assert_eq!(admin_b1, admin_b2);
    assert_eq!(admin_b2, admin_b3);

    Ok(())
}
