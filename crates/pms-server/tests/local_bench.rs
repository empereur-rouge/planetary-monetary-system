//! Local In-Process TPS Benchmark — No Docker, No TLS, One Command.
//!
//! Spawns an in-process Axum HTTP server with a fresh RocksDB, runs 10 parallel
//! workers sending 1,000 transactions each, and reports TPS.
//!
//! ## Usage
//!
//! ```bash
//! cargo test --release -p pms-server local_bench -- --ignored --nocapture
//! ```
//!
//! The test auto-generates `etc/config/config.bench.toml` from `config.local.toml`
//! with Dev mode enabled (no coordinator key enforcement).
//!
//! ## Architecture
//!
//! ```text
//!   [Test Process]
//!   ├─ In-process Axum server (HTTP, random port)
//!   │  ├─ Fresh RocksDB (tempdir)
//!   │  ├─ ConcurrentDag (RAM)
//!   │  └─ ShardedUtxoSet (RAM)
//!   │
//!   ├─ reqwest::Client (connection pooling)
//!   │  ├─ Worker 0: tx0 → tx1 → tx2 ... (independent UTXO chain)
//!   │  ├─ Worker 1: tx0 → tx1 → tx2 ...
//!   │  └─ Worker N: tx0 → tx1 → tx2 ...
//!   │
//!   └─ Progress display + final TPS report
//! ```

use anyhow::{Context, Result};
use pms_config::{ServerConfig, TreasuryWallets, load_config};
use pms_core::ConcurrentDag;
use pms_interface::NetDagAdapter;
use pms_server::api::{AppState, build_api_router};
use pms_server::stats::Stats;
use pms_server::{Server, resolve_admin_token};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_types::{
    Block, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock,
};
use pms_utils::compute_block_id;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireBlock;
use reqwest::Client;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

// ============================================================================
// CONFIGURATION
// ============================================================================

/// Total transactions to send during the benchmark.
const TX_COUNT: usize = 10_000;

/// Number of parallel workers. Each manages its own UTXO chain.
const WORKER_COUNT: usize = 10;

/// Transactions per worker = TX_COUNT / WORKER_COUNT.
const TX_PER_WORKER: usize = TX_COUNT / WORKER_COUNT;

/// Initial mint amount. Must cover all transactions + fees.
const INITIAL_MINT: &str = "500000.0";

/// Amount sent in each test transaction.
const PAYMENT_AMOUNT: &str = "1.0";

/// Fee per transaction (base fee + dynamic overhead).
const FEE_AMOUNT: &str = "0.036";

// ============================================================================
// DATA STRUCTURES
// ============================================================================

/// Reference to an unspent transaction output.
#[derive(Debug, Clone)]
struct OutputRef {
    txid: String,
    index: u32,
    amount: rust_decimal::Decimal,
}

// ============================================================================
// WORKSPACE ROOT HELPER
// ============================================================================

/// Find workspace root via CARGO_MANIFEST_DIR (two levels up from crate dir).
fn get_workspace_root() -> PathBuf {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let crate_dir = std::path::Path::new(&manifest_dir);
    // crates/pms-server → workspace root is ../../
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to find workspace root")
        .to_path_buf()
}

// ============================================================================
// MAIN BENCHMARK TEST
// ============================================================================

/// VPS simulation: 6 vCores → 6 tokio worker threads.
/// This is the primary TPS constraint (CPU-bound block signing + validation).
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn local_bench() -> Result<()> {
    let root = get_workspace_root();

    // ── 1. Set CWD to workspace root so relative config paths work ──────
    std::env::set_current_dir(&root).expect("Failed to set CWD to workspace root");

    // ── 2. Generate a benchmark config in Dev mode ─────────────────────
    //
    // CoreAdapter::new() calls load_config() internally, so we must
    // point PMS_CONFIG to a file with the right settings.
    // Dev mode disables:
    //   - Hardcoded coordinator key fallback (no single-writer enforcement)
    //   - Platform address signature requirement
    let config_src = std::fs::read_to_string(root.join("etc/config/config.local.toml"))
        .expect("Failed to read config.local.toml");
    // Strip coordinator keys + switch to dev mode + disable single-writer
    let config_bench: String = config_src
        .lines()
        .map(|l| {
            let trimmed = l.trim_start();
            // Remove coordinator keys entirely
            if trimmed.starts_with("coordinator_public_key")
                || trimmed.starts_with("coordinator_x25519_public_key")
            {
                return String::new();
            }
            // Switch network mode from testnet to dev
            if trimmed.starts_with("mode") && trimmed.contains("testnet") {
                return l.replace("testnet", "dev");
            }
            // Fix prefix: dev mode requires "pms:dev"
            if trimmed.starts_with("prefix") && trimmed.contains("pms:test") {
                return l.replace("pms:test", "pms:dev");
            }
            // Disable single-writer enforcement
            if trimmed.starts_with("enforce_single_writer") {
                return l.replace("true", "false");
            }
            l.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Ensure enforce_single_writer is set even if not in the original config
    let config_bench = if !config_bench.contains("enforce_single_writer") {
        config_bench.replace(
            "[validation]",
            "[validation]\nenforce_single_writer = false",
        )
    } else {
        config_bench
    };
    let bench_config_path = root.join("etc/config/config.bench.toml");
    std::fs::write(&bench_config_path, &config_bench)
        .expect("Failed to write bench config");

    unsafe {
        std::env::set_var("PMS_CONFIG", bench_config_path.to_string_lossy().as_ref());
    }
    if std::env::var("PMS_ADMIN_TOKEN").is_err() {
        unsafe { std::env::set_var("PMS_ADMIN_TOKEN", "bench") };
    }

    // ── 3. Load config + admin wallet ────────────────────────────────────
    let mut settings = load_config()?;
    let network_id = settings.network.network_id.clone();
    let protocol_version = settings.network.protocol_version as u16;

    let admin_wallet_path = root.join("etc/pms/admin-wallet.json");
    assert!(
        admin_wallet_path.exists(),
        "Admin wallet not found: {:?}",
        admin_wallet_path
    );
    let admin_wallet =
        Wallet::load_from_file(admin_wallet_path.to_string_lossy().as_ref())
            .expect("Failed to load admin wallet");
    let admin_addr = admin_wallet.get_address("8e");
    let admin_pk = admin_wallet.encoded_public_key();

    // Register admin as authorized minter
    settings.admin.signer_pubkeys = vec![admin_pk.clone()];
    settings.admin.wallet_addresses = vec![admin_addr.clone()];

    // ── 3. Create RocksDB in tempdir (kept alive for entire test) ────────
    let _tmp = tempfile::tempdir()?;
    let db_path = _tmp.path().join("bench-rocks");
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            settings.rocks.tip_limit as usize,
            &settings.rocks.prefix,
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    store.ensure_schema().await?;
    store.bootstrap_once_for_production()?;

    // ── 4. Genesis block ─────────────────────────────────────────────────
    if store.all_block_ids().await?.is_empty() {
        let g = Block::genesis(compute_block_id);
        let meta = pms_wire::WireMeta::from(&settings);
        store.persist_genesis(&g, &meta).await?;
    }

    // ── 5. DAG + Adapter ─────────────────────────────────────────────────
    let dag = Arc::new(ConcurrentDag::bootstrap_from_store(&*store).await?);
    let adapter: Arc<dyn NetDagAdapter> =
        pms_core::CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    // ── 6. Node wallet (used by /wallet/tx/send for block signing) ───────
    let node_wallet = Arc::new(
        Wallet::from_seed(&[7u8; 32], None).expect("test wallet must not fail"),
    );

    // ── 7. Server + AppState ─────────────────────────────────────────────
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
        None,
    );

    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: settings.tls.clone(),
        api_tls_enabled: false,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    let admin_token = settings
        .auth
        .admin_api_token
        .as_deref()
        .and_then(resolve_admin_token);

    let tps_tracker = Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60));

    let state = AppState {
        srv,
        _cfg: cfg,
        _ready: Arc::new(AtomicBool::new(true)),
        stats: Arc::new(Stats::new()),
        store: store.clone(),
        admin_token,
        node_wallet: node_wallet.clone(),
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![],
        treasury_wallets: TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: pms_server::fee_pool::create_fee_pool(),
        fee_pool_registry: Arc::new(pms_server::fee_pool::FeePoolRegistry::new()),
        api_key_store: pms_server::api_keys::create_api_key_store(None)
            .expect("empty api key store must work"),
        ledger_mgr: None,
        ledger_id: "main".into(),
        effective_fees: Arc::new(pms_server::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: tps_tracker.clone(),
        contract_event_bus: None,
        contract_store: store.clone(),
    };

    // ── 8. Build router and bind to random port ──────────────────────────
    let app = build_api_router(state, &settings);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let base_url = format!("http://{}", addr);

    // CRITICAL: must use into_make_service_with_connect_info so that
    // SmartIpKeyExtractor (rate limiter) can read the client IP.
    let server_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
            eprintln!("   SERVER ERROR: {}", e);
        }
    });

    // Give the server a moment to start
    sleep(Duration::from_millis(100)).await;

    // Check if server task has already failed
    if server_handle.is_finished() {
        panic!("Server task exited immediately — check for panics");
    }

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!(
        "║  PMS Local Benchmark - {} workers x {} tx/worker          ║",
        WORKER_COUNT, TX_PER_WORKER
    );
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("   Server:   {}", base_url);
    println!("   Network:  {}", network_id);
    println!("   Protocol: {}", protocol_version);

    // ── 9. HTTP client (no TLS) ──────────────────────────────────────────
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(WORKER_COUNT * 2)
        .pool_idle_timeout(Duration::from_secs(30))
        .build()?;

    // Wait for API readiness
    println!("\n   Waiting for API...");
    let mut api_ready = false;
    for i in 0..60 {
        match client.get(format!("{}/livez", base_url)).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    println!("   API is ready!");
                    api_ready = true;
                    break;
                } else {
                    println!("   /livez returned {}", resp.status());
                }
            }
            Err(e) => {
                if i % 10 == 0 {
                    println!("   Waiting... ({}/60): {}", i, e);
                }
            }
        }
        // Check if server crashed
        if server_handle.is_finished() {
            panic!("Server task exited while waiting for readiness");
        }
        sleep(Duration::from_millis(200)).await;
    }
    if !api_ready {
        panic!("Server not ready after 12s");
    }

    // ── 10. Mint initial supply ──────────────────────────────────────────
    println!("\n   Minting {} PMS...", INITIAL_MINT);
    let genesis_id = Block::genesis(compute_block_id).id;
    let parents = vec![genesis_id.clone()];
    let mint_id = mine_mint(
        &client,
        &base_url,
        &admin_wallet,
        &admin_addr,
        INITIAL_MINT,
        parents,
        &network_id,
        protocol_version,
    )
    .await?;
    println!("   Mint block: {}...", &mint_id[..16]);

    sleep(Duration::from_secs(1)).await;

    // ── 11. Split funds to N workers ─────────────────────────────────────
    println!("\n   Splitting funds into {} workers...", WORKER_COUNT);
    let mut workers: Vec<(Wallet, OutputRef)> = Vec::with_capacity(WORKER_COUNT);
    let amount_per_worker = rust_decimal::Decimal::from_str(INITIAL_MINT)?
        / rust_decimal::Decimal::from(WORKER_COUNT as u64);

    let mut current_split_utxo = OutputRef {
        txid: mint_id.clone(),
        index: 0,
        amount: rust_decimal::Decimal::from_str(INITIAL_MINT)?,
    };

    for i in 0..WORKER_COUNT {
        let worker_wallet = Wallet::generate();
        let worker_addr = worker_wallet.get_address("8e");
        let remaining = current_split_utxo.amount - amount_per_worker;

        let genesis_for_parent = Block::genesis(compute_block_id).id;
        let split_parents = vec![current_split_utxo.txid.clone(), genesis_for_parent];

        let split_id = send_tx(
            &client,
            &base_url,
            &admin_wallet,
            &current_split_utxo.txid,
            current_split_utxo.index,
            &worker_addr,
            &amount_per_worker.to_string(),
            &admin_addr,
            &remaining.to_string(),
            split_parents,
            &network_id,
            protocol_version,
        )
        .await
        .context(format!("Failed to fund worker {}", i))?;

        workers.push((
            worker_wallet,
            OutputRef {
                txid: split_id.clone(),
                index: 0,
                amount: amount_per_worker,
            },
        ));

        current_split_utxo = OutputRef {
            txid: split_id,
            index: 1,
            amount: remaining,
        };

        println!("   Worker {} funded with {} PMS", i, amount_per_worker);
    }

    sleep(Duration::from_secs(1)).await;

    // ── 12. Run parallel benchmark ───────────────────────────────────────
    println!("\n═══════════════════════════════════════════════════════════════");
    println!(
        "   STARTING PARALLEL BENCHMARK: {} workers x {} tx = {} total",
        WORKER_COUNT, TX_PER_WORKER, TX_COUNT
    );
    println!("═══════════════════════════════════════════════════════════════\n");

    let benchmark_start = Instant::now();
    let progress_counter = Arc::new(AtomicUsize::new(0));
    let progress_failed = Arc::new(AtomicUsize::new(0));

    // Progress display task
    let pc = progress_counter.clone();
    let pf = progress_failed.clone();
    let progress_task = tokio::spawn(async move {
        let mut last_count = 0usize;
        let mut tick = 0u32;
        loop {
            sleep(Duration::from_secs(1)).await;
            tick += 1;
            let current = pc.load(Ordering::Relaxed);
            let failed = pf.load(Ordering::Relaxed);
            let tps = current - last_count;
            let overall_tps = current as f64 / tick as f64;
            last_count = current;
            eprint!(
                "\r   {:>5}/{} tx | {:>4} tx/s (avg: {:.1}) | {} failed   ",
                current, TX_COUNT, tps, overall_tps, failed
            );
            if current + failed >= TX_COUNT {
                eprintln!();
                break;
            }
        }
    });

    // Spawn workers
    let handles: Vec<_> = workers
        .into_iter()
        .enumerate()
        .map(|(id, (wallet, utxo))| {
            let client = client.clone();
            let base = base_url.clone();
            let recipient = admin_addr.clone(); // Send to admin (we don't care about destination)
            let fee_addr = admin_addr.clone();
            let counter = progress_counter.clone();
            let failed_counter = progress_failed.clone();

            tokio::spawn(async move {
                run_worker(
                    client,
                    &base,
                    wallet,
                    recipient,
                    fee_addr,
                    utxo,
                    id,
                    counter,
                    failed_counter,
                )
                .await
            })
        })
        .collect();

    let results = futures::future::join_all(handles).await;

    let mut successful = 0usize;
    let mut failed = 0usize;
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Ok((s, f)) => {
                println!("   Worker {}: {} success, {} failed", i, s, f);
                successful += s;
                failed += f;
            }
            Err(e) => {
                eprintln!("   Worker {} panicked: {}", i, e);
                failed += TX_PER_WORKER;
            }
        }
    }

    let total_duration = benchmark_start.elapsed();
    let final_tps = successful as f64 / total_duration.as_secs_f64();

    progress_task.abort();

    // ── 13. Server-side TPS ──────────────────────────────────────────────
    let server_tps = tps_tracker.current_tps();

    // ── 14. Results ──────────────────────────────────────────────────────
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("   BENCHMARK RESULTS");
    println!("═══════════════════════════════════════════════════════════════");
    println!("   Duration:         {:.2}s", total_duration.as_secs_f64());
    println!("   Workers:          {}", WORKER_COUNT);
    println!("   Tx per worker:    {}", TX_PER_WORKER);
    println!("   Total tx:         {} submitted", TX_COUNT);
    println!("   Successful:       {}", successful);
    println!("   Failed:           {}", failed);
    println!("   ─────────────────────────────────────────");
    println!("   Client TPS:       {:.2} tx/sec", final_tps);
    println!("   Server TPS (60s): {:.2} blk/sec", server_tps);
    println!("═══════════════════════════════════════════════════════════════\n");

    // ── 15. Save results ─────────────────────────────────────────────────
    let target_dir = root.join("benchmark_results");
    std::fs::create_dir_all(&target_dir).ok();

    let now = chrono::Local::now();
    let timestamp_str = now.format("%Y%m%d_%H%M%S").to_string();
    let filename = format!("bench_local_{}.json", timestamp_str);
    let metrics_path = target_dir.join(&filename);

    let metrics = serde_json::json!({
        "type": "local_bench",
        "timestamp": now.to_rfc3339(),
        "workers": WORKER_COUNT,
        "tx_per_worker": TX_PER_WORKER,
        "tx_count": TX_COUNT,
        "successful": successful,
        "failed": failed,
        "duration_secs": total_duration.as_secs_f64(),
        "client_tps": final_tps,
        "server_tps_60s": server_tps,
    });

    if let Err(e) = std::fs::write(&metrics_path, serde_json::to_string_pretty(&metrics)?) {
        eprintln!("   Failed to write metrics: {}", e);
    } else {
        println!("   Metrics saved to {:?}", metrics_path);
    }

    // Update SUMMARY.md
    let summary_path = target_dir.join("SUMMARY.md");
    let summary_line = format!(
        "| {} | **{:.2}** | {:.2} | {} | {} | {:.2}s | {} |\n",
        now.format("%Y-%m-%d %H:%M:%S"),
        final_tps,
        server_tps,
        WORKER_COUNT,
        TX_COUNT,
        total_duration.as_secs_f64(),
        if failed == 0 { "PASS" } else { "FAIL" }
    );

    let header = "# Benchmark History\n\n| Date | Client TPS | Server TPS | Workers | Total Tx | Duration | Result |\n|---|---|---|---|---|---|---|\n";
    let current = std::fs::read_to_string(&summary_path).unwrap_or_default();
    let final_content = if current.trim().is_empty() {
        format!("{}{}", header, summary_line)
    } else if let Some(pos) = current.find("|---|---|") {
        if let Some(nl) = current[pos..].find('\n') {
            let insert = pos + nl + 1;
            let (before, after) = current.split_at(insert);
            format!("{}{}{}", before, summary_line, after)
        } else {
            format!("{}\n{}", current, summary_line)
        }
    } else {
        format!("{}{}", header, summary_line)
    };
    std::fs::write(&summary_path, final_content).ok();
    println!("   Summary updated in {:?}", summary_path);

    // ── 16. Assert minimum success rate ──────────────────────────────────
    assert!(
        successful >= TX_COUNT * 90 / 100,
        "Less than 90% success rate: {} / {}",
        successful,
        TX_COUNT
    );

    Ok(())
}

// ============================================================================
// WORKER
// ============================================================================

/// Each worker sends TX_PER_WORKER transactions sequentially, using the change
/// output of each tx as the input for the next.
async fn run_worker(
    client: Client,
    base_url: &str,
    sender: Wallet,
    recipient_addr: String,
    fee_addr: String,
    mut current_utxo: OutputRef,
    worker_id: usize,
    progress_counter: Arc<AtomicUsize>,
    progress_failed: Arc<AtomicUsize>,
) -> (usize, usize) {
    let payment = rust_decimal::Decimal::from_str(PAYMENT_AMOUNT).unwrap();
    let fee = rust_decimal::Decimal::from_str(FEE_AMOUNT).unwrap();
    let sender_addr = sender.get_address("8e");

    let mut successful = 0usize;
    let mut failed = 0usize;

    for i in 0..TX_PER_WORKER {
        let change = current_utxo.amount - payment - fee;

        match send_tx_fast(
            &client,
            base_url,
            &sender,
            &current_utxo.txid,
            current_utxo.index,
            &recipient_addr,
            PAYMENT_AMOUNT,
            &fee_addr,
            FEE_AMOUNT,
            &sender_addr,
            &change.to_string(),
        )
        .await
        {
            Ok((txid, change_index)) => {
                current_utxo = OutputRef {
                    txid,
                    index: change_index,
                    amount: change,
                };
                successful += 1;
                progress_counter.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                failed += 1;
                progress_failed.fetch_add(1, Ordering::Relaxed);
                if failed <= 3 {
                    eprintln!("   Worker {} tx {} failed: {}", worker_id, i, e);
                }
                // UTXO chain is broken — stop this worker
                break;
            }
        }
    }

    (successful, failed)
}

// ============================================================================
// BLOCK SUBMISSION HELPERS
// ============================================================================

/// Create a Mint block (initial supply creation).
async fn mine_mint(
    client: &Client,
    base_url: &str,
    miner: &Wallet,
    to_addr: &str,
    amount: &str,
    parents: Vec<String>,
    network_id: &str,
    protocol_version: u16,
) -> Result<String> {
    let output = TxOutput {
        address: to_addr.to_string(),
        amount: amount.to_string(),
        asset_id: None,
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: vec![output],
    }));
    let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();

    // PoW: find nonce where ID starts with "0" (4-bit difficulty)
    loop {
        if block.id.starts_with("0") {
            break;
        }
        block.nonce += 1;
        block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
    }

    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: network_id.to_string(),
        protocol_version,
        signer_pk_hex: miner.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };
    wb.signature_hex = miner.sign(&canonical_wireblock_message(&wb)).unwrap();

    let resp = client
        .post(format!("{}/submit/block", base_url))
        .json(&wb)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Mint failed: {} -> {}", status, body);
    }

    Ok(wb.id)
}

/// Send a transaction via raw block submission (used for setup: splits).
async fn send_tx(
    client: &Client,
    base_url: &str,
    sender: &Wallet,
    utxo_txid: &str,
    utxo_index: u32,
    recipient_addr: &str,
    amount: &str,
    change_addr: &str,
    change_amount: &str,
    parents: Vec<String>,
    network_id: &str,
    protocol_version: u16,
) -> Result<String> {
    let mut outputs = vec![TxOutput {
        address: recipient_addr.to_string(),
        amount: amount.to_string(),
        asset_id: None,
    }];

    if change_amount != "0.0" && change_amount != "0" {
        outputs.push(TxOutput {
            address: change_addr.to_string(),
            amount: change_amount.to_string(),
            asset_id: None,
        });
    }

    let mut tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: utxo_txid.to_string(),
                index: utxo_index,
            },
        }],
        outputs,
        fee: "0.0".to_string(),
        unlocks: vec![],
    };

    let msg = tx.signing_message().unwrap();
    let sig_b64 = sender.sign(&msg).unwrap();
    tx.unlocks = vec![Unlock {
        pubkey_hex: sender.encoded_public_key(),
        signature_b64: sig_b64,
    }];

    let payload = Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)));
    let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();

    // PoW
    loop {
        if block.id.starts_with("0") {
            break;
        }
        block.nonce += 1;
        block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
    }

    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: network_id.to_string(),
        protocol_version,
        signer_pk_hex: sender.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };
    wb.signature_hex = sender.sign(&canonical_wireblock_message(&wb)).unwrap();

    let resp = client
        .post(format!("{}/submit/block", base_url))
        .json(&wb)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("TX failed: {} -> {}", status, body);
    }

    Ok(wb.id)
}

/// Send a transaction via the high-level `/wallet/tx/send` endpoint.
///
/// The server constructs the block, selects parents, does PoW, and signs.
/// This is the fast path used during the actual benchmark.
async fn send_tx_fast(
    client: &Client,
    base_url: &str,
    sender: &Wallet,
    utxo_txid: &str,
    utxo_index: u32,
    recipient_addr: &str,
    amount: &str,
    fee_addr: &str,
    fee_amount: &str,
    change_addr: &str,
    change_amount: &str,
) -> Result<(String, u32)> {
    let mut outputs = vec![TxOutput {
        address: recipient_addr.to_string(),
        amount: amount.to_string(),
        asset_id: None,
    }];

    if fee_amount != "0.0" {
        outputs.push(TxOutput {
            address: fee_addr.to_string(),
            amount: fee_amount.to_string(),
            asset_id: None,
        });
    }

    if change_amount != "0.0" && change_amount != "0" {
        outputs.push(TxOutput {
            address: change_addr.to_string(),
            amount: change_amount.to_string(),
            asset_id: None,
        });
    }

    let mut tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: utxo_txid.to_string(),
                index: utxo_index,
            },
        }],
        outputs: outputs.clone(),
        fee: fee_amount.to_string(),
        unlocks: vec![],
    };

    let msg = tx.signing_message().unwrap();
    let sig_b64 = sender.sign(&msg).unwrap();
    tx.unlocks = vec![Unlock {
        pubkey_hex: sender.encoded_public_key(),
        signature_b64: sig_b64,
    }];

    // Extract X25519 public keys for payload encryption
    let mut recipients_xpk = vec![];
    for out in &tx.outputs {
        if let Ok((_h20, xpk)) = pms_wallet::decode_address(&out.address) {
            recipients_xpk.push(xpk);
        }
    }

    let req_body = serde_json::json!({
        "tx": tx,
        "recipients_xpk": recipients_xpk
    });

    let resp = client
        .post(format!("{}/wallet/tx/send", base_url))
        .json(&req_body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("TX failed: {} -> {}", status, body);
    }

    let body = resp.text().await?;
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let block_id = json["id"]
        .as_str()
        .ok_or(anyhow::anyhow!("No id in response: {}", body))?
        .to_string();

    // change index: [recipient, fee, change] → 2 if fee, else 1
    let change_index = if fee_amount != "0.0" { 2u32 } else { 1u32 };

    Ok((block_id, change_index))
}
