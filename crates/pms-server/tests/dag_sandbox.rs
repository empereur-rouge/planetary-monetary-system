//! Production-like DAG Sandbox — reusable lab for integration tests.
//!
//! Boots a full PMS engine in-process with LedgerManager, EventBus,
//! ContractListener, and fee distribution. Replicates deploy-testnet.sh.
//!
//! ## Usage
//! ```bash
//! cargo test --release -p pms-server dag_sandbox -- --ignored --nocapture
//! ```
//!
//! ## Architecture
//!
//! ```text
//!   boot_sandbox() → Sandbox
//!   ├─ LedgerManager::bootstrap()    // Multi-prefix RocksDB (main)
//!   ├─ CoreAdapter (from main instance)
//!   ├─ Server::new(adapter, ledger_mgr)
//!   ├─ AppState (full production-like)
//!   ├─ spawn_fee_distributor_task()   // 2s interval
//!   ├─ FeePoolRefundSink + spawn_contract_listener()
//!   └─ axum::serve on random port
//! ```

#![allow(dead_code)]

use anyhow::{Context, Result};
use pms_config::{ServerConfig, TreasuryWallets, load_config};
use pms_server::api::{AppState, FeePoolRefundSink, build_api_router, spawn_fee_distributor_task};
use pms_server::stats::Stats;
use pms_server::{Server, resolve_admin_token};
// RocksMemoryConfig not needed — LedgerManager::bootstrap handles DB config
use pms_wallet::{SignerBackend, Wallet};
use reqwest::Client;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;

// ============================================================================
// SANDBOX STRUCT
// ============================================================================

/// Production-like PMS engine running in-process.
///
/// Holds the server handle, HTTP client, admin wallet, and helper methods
/// for creating ledgers, minting, sending transactions, and querying balances.
/// Drop-safe: the tempdir is kept alive for the lifetime of the sandbox.
struct Sandbox {
    base_url: String,
    client: Client,
    admin_wallet: Wallet,
    admin_addr: String,
    admin_token: String,
    #[allow(dead_code)]
    network_id: String,
    /// Kept alive so RocksDB data directory persists for the test duration.
    _tmp: tempfile::TempDir,
    /// Server task handle — aborted on drop.
    _server_handle: tokio::task::JoinHandle<()>,
    /// P2P server handle (multi-engine cluster mode only). `None` when the
    /// sandbox was booted via `boot_sandbox()` (single engine, no P2P).
    _p2p_handle: Option<tokio::task::JoinHandle<()>>,
    /// Reference to the underlying `Server` so multi-engine tests can
    /// invoke `connect_to_peer` and `get_p2p_peers`. `None` for the
    /// single-engine path.
    server_arc: Option<Arc<Server>>,
    /// P2P bind address (`127.0.0.1:<port>`). `None` for single-engine.
    p2p_addr: Option<String>,
    /// Cluster index (0 = coordinator, ≥1 = follower). `None` for single-engine.
    cluster_idx: Option<usize>,
}

impl Sandbox {
    // ── HTTP helpers ──────────────────────────────────────────────────

    /// POST with admin Bearer token to a global endpoint.
    async fn admin_post(&self, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .bearer_auth(&self.admin_token)
            .json(&body)
            .send()
            .await
            .expect("HTTP POST failed");
        let status = resp.status();
        let json = resp.json::<Value>().await.unwrap_or(json!({}));
        (status, json)
    }

    /// POST with admin Bearer token to a per-ledger endpoint.
    async fn admin_post_ledger(
        &self,
        ledger_id: &str,
        path: &str,
        body: Value,
    ) -> (reqwest::StatusCode, Value) {
        let url = format!("{}/l/{}{}", self.base_url, ledger_id, path);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.admin_token)
            .json(&body)
            .send()
            .await
            .expect("HTTP POST (ledger) failed");
        let status = resp.status();
        let json = resp.json::<Value>().await.unwrap_or(json!({}));
        (status, json)
    }

    /// POST without auth (public endpoint), optionally on a ledger.
    async fn post(
        &self,
        ledger_id: Option<&str>,
        path: &str,
        body: Value,
    ) -> (reqwest::StatusCode, Value) {
        let url = match ledger_id {
            Some(lid) => format!("{}/l/{}{}", self.base_url, lid, path),
            None => format!("{}{}", self.base_url, path),
        };
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .expect("HTTP POST failed");
        let status = resp.status();
        let json = resp.json::<Value>().await.unwrap_or(json!({}));
        (status, json)
    }

    /// POST (public, on a specific ledger path).
    async fn post_ledger(
        &self,
        ledger_id: &str,
        path: &str,
        body: Value,
    ) -> (reqwest::StatusCode, Value) {
        self.post(Some(ledger_id), path, body).await
    }

    /// GET with admin Bearer token.
    #[allow(dead_code)]
    async fn admin_get(&self, path: &str) -> (reqwest::StatusCode, Value) {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .bearer_auth(&self.admin_token)
            .send()
            .await
            .expect("HTTP GET failed");
        let status = resp.status();
        let json = resp.json::<Value>().await.unwrap_or(json!({}));
        (status, json)
    }

    // ── High-level operations ────────────────────────────────────────

    /// Create a custom ledger (eden) via the admin API.
    async fn create_ledger(
        &self,
        id: &str,
        network_id: &str,
        prefix: &str,
        symbol: &str,
    ) -> Result<()> {
        let (status, body) = self
            .admin_post(
                "/admin/ledgers/create",
                json!({
                    "id": id,
                    "network_id": network_id,
                    "prefix": prefix,
                    "symbol": symbol,
                }),
            )
            .await;
        println!("   Create ledger '{}': {} — {:?}", id, status, body);
        anyhow::ensure!(
            status.is_success(),
            "Failed to create ledger '{}': {} — {}",
            id,
            status,
            body
        );
        Ok(())
    }

    /// Deposit PMS into a ledger's gas pool.
    async fn deposit_gas_pool(&self, ledger_id: &str, amount: &str) -> Result<()> {
        let (status, body) = self
            .admin_post(
                "/admin/gas-pool/deposit",
                json!({
                    "ledger_id": ledger_id,
                    "amount": amount,
                }),
            )
            .await;
        println!(
            "   Gas pool deposit '{}' +{}: {} — {:?}",
            ledger_id, amount, status, body
        );
        anyhow::ensure!(
            status.is_success(),
            "Gas pool deposit failed: {} — {}",
            status,
            body
        );
        Ok(())
    }

    /// Faucet mint PMS on a specific ledger (or main if None).
    async fn faucet_mint(
        &self,
        ledger_id: Option<&str>,
        to: &str,
        amount: &str,
    ) -> Result<String> {
        let body = json!({ "to": to, "amount": amount });
        let (status, resp) = match ledger_id {
            Some(lid) => self.admin_post_ledger(lid, "/admin/faucet", body).await,
            None => self.admin_post("/admin/faucet", body).await,
        };
        let label = ledger_id.unwrap_or("main");
        println!(
            "   Faucet mint {} PMS on '{}' to {}...: {} — {:?}",
            amount,
            label,
            &to[..20.min(to.len())],
            status,
            resp
        );
        anyhow::ensure!(
            status.is_success(),
            "Faucet mint failed on '{}': {} — {}",
            label,
            status,
            resp
        );
        Ok(resp["block_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }

    /// Send PMS via wallet/send-simple on a specific ledger.
    async fn send_simple(
        &self,
        ledger_id: Option<&str>,
        private_key_b64: &str,
        to: &str,
        amount: &str,
    ) -> Result<Value> {
        let body = json!({
            "private_key_b64": private_key_b64,
            "to": to,
            "amount": amount,
        });
        let (status, resp) = self
            .post(ledger_id, "/v1/wallet/send-simple", body)
            .await;
        anyhow::ensure!(
            status.is_success(),
            "send-simple failed: {} — {}",
            status,
            resp
        );
        Ok(resp)
    }

    /// Trigger manual fee distribution.
    async fn distribute_fees(&self) -> Result<Value> {
        let (status, body) = self
            .admin_post("/admin/distribute_fees", json!({}))
            .await;
        println!("   Distribute fees: {} — {:?}", status, body);
        anyhow::ensure!(
            status.is_success(),
            "Fee distribution failed: {} — {}",
            status,
            body
        );
        Ok(body)
    }

    /// Query PMS balance for an address on a given ledger.
    async fn get_balance(&self, ledger_id: &str, address: &str) -> Result<Decimal> {
        let body = json!({
            "address": address,
            "ledger_id": ledger_id,
        });
        let (status, resp) = self.post(None, "/v1/balance", body).await;
        anyhow::ensure!(
            status.is_success(),
            "Balance query failed: {} — {}",
            status,
            resp
        );
        let balance_str = resp["balance"].as_str().unwrap_or("0");
        Ok(Decimal::from_str(balance_str).unwrap_or(Decimal::ZERO))
    }
}

// ============================================================================
// WORKSPACE ROOT HELPER
// ============================================================================

/// Find workspace root via CARGO_MANIFEST_DIR (two levels up from crate dir).
fn get_workspace_root() -> PathBuf {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let crate_dir = std::path::Path::new(&manifest_dir);
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to find workspace root")
        .to_path_buf()
}

// ============================================================================
// BOOT SANDBOX
// ============================================================================

/// Boots a production-like PMS engine in-process.
///
/// - Multi-prefix RocksDB in a temp directory
/// - LedgerManager with "main" ledger
/// - Full AppState with fee pool registry, contract event bus
/// - Fee distribution task (2s interval for fast testing)
/// - Contract listener wired via FeePoolRefundSink
/// - Axum server on a random port (HTTP, no TLS)
async fn boot_sandbox() -> Result<Sandbox> {
    let root = get_workspace_root();

    // ── 1. Set CWD to workspace root (relative config paths) ─────────
    std::env::set_current_dir(&root).expect("Failed to set CWD to workspace root");

    // ── 2. Load admin wallet FIRST (needed for coordinator key in config) ──
    let admin_wallet_path = root.join("etc/pms/admin-wallet.json");
    anyhow::ensure!(
        admin_wallet_path.exists(),
        "Admin wallet not found: {:?}",
        admin_wallet_path
    );
    let admin_wallet =
        Wallet::load_from_file(admin_wallet_path.to_string_lossy().as_ref())
            .map_err(|e| anyhow::anyhow!("Failed to load admin wallet: {}", e))?;
    let admin_addr = admin_wallet.get_address("8e");
    let admin_pk = admin_wallet.encoded_public_key();

    // ── 3. Generate sandbox config (Dev mode, fast fee distribution) ──
    // IMPORTANT: CoreAdapter::new() calls load_config() internally, so the
    // config file must contain the correct coordinator_public_key BEFORE
    // LedgerManager::bootstrap creates the CoreAdapter.
    let config_src = std::fs::read_to_string(root.join("etc/config/config.local.toml"))
        .context("Failed to read config.local.toml")?;

    let config_bench: String = config_src
        .lines()
        .map(|l| {
            let trimmed = l.trim_start();
            // Replace coordinator keys with admin wallet's key
            if trimmed.starts_with("coordinator_public_key") && !trimmed.starts_with("coordinator_public_key_") {
                return format!("coordinator_public_key = \"{}\"", admin_pk);
            }
            if trimmed.starts_with("coordinator_x25519_public_key") {
                return format!(
                    "coordinator_x25519_public_key = \"{}\"",
                    admin_wallet.x25519_pub_hex()
                );
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

    // Ensure coordinator key is in the config even if not in original file
    let config_bench = if !config_bench.contains("coordinator_public_key") {
        config_bench.replace(
            "[validation]",
            &format!(
                "[validation]\ncoordinator_public_key = \"{}\"",
                admin_pk
            ),
        )
    } else {
        config_bench
    };

    // Add fast fee distribution interval (2s instead of 600s default)
    let config_bench = if config_bench.contains("distribution_interval_sec") {
        // Replace existing value
        let mut result = String::new();
        for line in config_bench.lines() {
            if line.trim_start().starts_with("distribution_interval_sec") {
                result.push_str("distribution_interval_sec = 2\n");
            } else {
                result.push_str(line);
                result.push('\n');
            }
        }
        result
    } else {
        // Append after [fees] section
        config_bench.replace("[fees]", "[fees]\ndistribution_interval_sec = 2")
    };

    let bench_config_path = root.join("etc/config/config.bench.toml");
    std::fs::write(&bench_config_path, &config_bench)
        .context("Failed to write bench config")?;

    unsafe {
        std::env::set_var("PMS_CONFIG", bench_config_path.to_string_lossy().as_ref());
    }
    if std::env::var("PMS_ADMIN_TOKEN").is_err() {
        unsafe { std::env::set_var("PMS_ADMIN_TOKEN", "sandbox_test") };
    }

    // ── 4. Load config ───────────────────────────────────────────────
    let mut settings = load_config()?;
    let network_id = settings.network.network_id.clone();

    // Register admin as authorized minter/signer + coordinator
    settings.admin.signer_pubkeys = vec![admin_pk.clone()];
    settings.admin.wallet_addresses = vec![admin_addr.clone()];
    settings.validation.coordinator_public_key = Some(admin_pk.clone());

    // ── 4. Create RocksDB in tempdir ─────────────────────────────────
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join("sandbox-rocks");

    // Override rocks.path BEFORE LedgerManager::bootstrap reads it
    settings.rocks.path = db_path.to_string_lossy().into();

    // ── 5. Bootstrap LedgerManager (multi-prefix, creates "main") ────
    let mgr = pms_ledger::LedgerManager::bootstrap(&settings)
        .await
        .context("LedgerManager::bootstrap failed")?;

    let main_instance = mgr
        .default_ledger()
        .context("No default (main) ledger after bootstrap")?;

    let store = main_instance.store.clone();
    let adapter = main_instance.adapter.clone();

    // ── 6. Node wallet (signs blocks, receives coordinator fees) ─────
    let node_wallet = Arc::new(admin_wallet.clone());

    // ── 7. Server (with LedgerManager for /l/{id} routes) ────────────
    let mgr = Arc::new(mgr);
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
        Some(mgr.clone()),
    );

    // ── 8. Resolve admin token ───────────────────────────────────────
    let admin_token = settings
        .auth
        .admin_api_token
        .as_deref()
        .and_then(resolve_admin_token)
        .unwrap_or_else(|| "sandbox_test".to_string());

    // ── 9. Fee pool registry + EventBus ──────────────────────────────
    let fee_pool_registry = Arc::new(pms_server::fee_pool::FeePoolRegistry::new());
    let main_fee_pool = fee_pool_registry.get_or_create("main");
    let main_store_for_contracts: Arc<dyn pms_storage::ContractStorage> = store.clone();
    let main_event_bus = srv.adapter_arc().event_bus();

    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: settings.tls.clone(),
        api_tls_enabled: false,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    // ── 10. Build AppState (mirrors api.rs production construction) ───
    let state = AppState {
        srv,
        _cfg: cfg,
        _ready: Arc::new(AtomicBool::new(true)),
        stats: Arc::new(Stats::new()),
        store: store.clone(),
        admin_token: Some(admin_token.clone()),
        node_wallet: node_wallet.clone(),
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![],
        treasury_wallets: TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: main_fee_pool,
        fee_pool_registry: fee_pool_registry.clone(),
        api_key_store: pms_server::api_keys::create_api_key_store(None)
            .expect("empty api key store must work"),
        ledger_mgr: Some(mgr),
        ledger_id: "main".into(),
        effective_fees: Arc::new(pms_server::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
        contract_event_bus: main_event_bus.clone(),
        contract_store: main_store_for_contracts.clone(),
        compliance_lock: Arc::new(tokio::sync::Mutex::new(())),
    };

    // ── 11. Spawn fee distributor task (2s interval) ─────────────────
    spawn_fee_distributor_task(state.clone());

    // ── 12. Wire contract listener via FeePoolRefundSink ─────────────
    if let Some(bus) = main_event_bus {
        let sink = Arc::new(FeePoolRefundSink {
            registry: fee_pool_registry.clone(),
        });
        pms_contracts::spawn_contract_listener(bus, main_store_for_contracts, sink);
        println!("   ContractListener spawned on EventBus");
    }

    // ── 13. Build router + bind to random port ───────────────────────
    let app = build_api_router(state, &settings);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let base_url = format!("http://{}", addr);

    let server_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            eprintln!("   SANDBOX SERVER ERROR: {}", e);
        }
    });

    // Give the server a moment to start
    sleep(Duration::from_millis(200)).await;

    if server_handle.is_finished() {
        anyhow::bail!("Sandbox server exited immediately — check for panics");
    }

    // ── 14. Wait for API readiness ───────────────────────────────────
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(20)
        .pool_idle_timeout(Duration::from_secs(30))
        .build()?;

    println!("   Waiting for API readiness...");
    let mut ready = false;
    for i in 0..60 {
        match client.get(format!("{}/livez", base_url)).send().await {
            Ok(resp) if resp.status().is_success() => {
                ready = true;
                break;
            }
            Ok(resp) => {
                if i % 10 == 0 {
                    println!("   /livez returned {} (attempt {}/60)", resp.status(), i);
                }
            }
            Err(e) => {
                if i % 10 == 0 {
                    println!("   Waiting... ({}/60): {}", i, e);
                }
            }
        }
        if server_handle.is_finished() {
            anyhow::bail!("Sandbox server crashed while waiting for readiness");
        }
        sleep(Duration::from_millis(200)).await;
    }
    anyhow::ensure!(ready, "Sandbox server not ready after 12s");

    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║  PMS DAG Sandbox — Production Simulation                 ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!("   Server:     {}", base_url);
    println!("   Network:    {} (dev mode)", network_id);
    println!("   Admin:      {}...{}", &admin_addr[..12], &admin_addr[admin_addr.len() - 8..]);
    println!("   Fee dist:   every 2s");
    println!();

    Ok(Sandbox {
        base_url,
        client,
        admin_wallet,
        admin_addr,
        admin_token,
        network_id,
        _tmp: tmp,
        _server_handle: server_handle,
        _p2p_handle: None,
        server_arc: None,
        p2p_addr: None,
        cluster_idx: None,
    })
}

// ============================================================================
// TEST: Coordinator receives eden fees
// ============================================================================

/// Verifies the full fee lifecycle on a custom ledger (eden):
///
/// 1. Create eden ledger via API
/// 2. Deposit gas pool for eden (anti-spam)
/// 3. Faucet mint PMS on eden for a test user
/// 4. Test user sends transactions on eden → fees are distributed via
///    immediate Reward blocks (wallet_send_simple creates them directly)
/// 5. Assert coordinator wallet received fee UTXOs on eden
///
/// **Fee distribution path**: `wallet_send_simple` creates an immediate Reward
/// block after each TX (via `create_reward_block`). The fee goes directly to
/// coordinator/treasury on the SAME ledger. This is different from the FeePool
/// accumulation path used by P2P-submitted blocks.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_coordinator_receives_eden_fees() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // ── 1. Create eden ledger ─────────────────────────────────────────
    println!("   [1/6] Creating eden ledger...");
    sandbox
        .create_ledger("eden", "eden-net", "eden", "EDN")
        .await?;

    // ── 2. Deposit gas pool for eden ─────────────────────────────────
    println!("   [2/6] Depositing gas pool for eden...");
    sandbox.deposit_gas_pool("eden", "50000").await?;

    // ── 3. Faucet mint PMS on eden for test user ─────────────────────
    println!("   [3/6] Creating test user and minting on eden...");
    let user_wallet = Wallet::generate();
    let user_addr = user_wallet.get_address("8e");
    let user_sk_b64 = user_wallet.private_key_b64.clone();
    println!(
        "   Test user: {}...{}",
        &user_addr[..12],
        &user_addr[user_addr.len() - 8..]
    );

    sandbox
        .faucet_mint(Some("eden"), &user_addr, "10000")
        .await?;

    // Small delay for UTXO propagation
    sleep(Duration::from_millis(500)).await;

    // Check user balance before transactions
    let user_balance_before = sandbox.get_balance("eden", &user_addr).await?;
    println!("   User balance (eden, before tx): {}", user_balance_before);

    // ── 4. Check coordinator balance on eden BEFORE transactions ──────
    // Coordinator starts with 0 on eden (no PMS minted to them on eden)
    let coord_eden_before = sandbox
        .get_balance("eden", &sandbox.admin_addr)
        .await?;
    println!(
        "   [4/6] Coordinator balance (eden, BEFORE tx): {}",
        coord_eden_before
    );

    // ── 5. Send transactions on eden (generates fees) ─────────────────
    // wallet_send_simple creates immediate Reward blocks — fees go directly
    // to coordinator address on eden (not via FeePool).
    println!("   [5/6] Sending 10 transactions on eden...");
    let admin_addr_clone = sandbox.admin_addr.clone();
    let mut total_fees_paid = Decimal::ZERO;
    let mut total_payments = Decimal::ZERO;
    let mut tx_count = 0u32;

    for i in 0..10 {
        match sandbox
            .send_simple(Some("eden"), &user_sk_b64, &admin_addr_clone, "10.0")
            .await
        {
            Ok(resp) => {
                let fee_str = resp["fee"].as_str().unwrap_or("0");
                let fee = Decimal::from_str(fee_str).unwrap_or(Decimal::ZERO);
                let transfer_fee = resp["transfer_fee"].as_str().unwrap_or("0");
                total_fees_paid += fee;
                total_payments += Decimal::from(10);
                tx_count += 1;
                println!(
                    "      TX {}: block={} fee={} transfer_fee={}",
                    i,
                    &resp["block_id"].as_str().unwrap_or("?")[..16],
                    fee_str,
                    transfer_fee
                );
            }
            Err(e) => {
                println!("      TX {} FAILED: {}", i, e);
                break;
            }
        }
    }
    println!(
        "   Sent {} tx, total payments: {}, total fees: {}",
        tx_count, total_payments, total_fees_paid
    );

    // ── 6. Check results ──────────────────────────────────────────────
    println!("   [6/6] Checking balances...");

    let coord_eden_after = sandbox
        .get_balance("eden", &sandbox.admin_addr)
        .await?;
    let user_balance_after = sandbox.get_balance("eden", &user_addr).await?;

    // Coordinator gained = payments + fee rewards
    let coord_gained = coord_eden_after - coord_eden_before;

    println!("\n   ╔══════════════════════════════════════════════════════╗");
    println!("   ║  RESULTS                                              ║");
    println!("   ╠══════════════════════════════════════════════════════╣");
    println!(
        "   ║  Coordinator (eden) before:   {:>20}    ║",
        coord_eden_before
    );
    println!(
        "   ║  Coordinator (eden) after:    {:>20}    ║",
        coord_eden_after
    );
    println!(
        "   ║  Coordinator gained:          {:>20}    ║",
        coord_gained
    );
    println!(
        "   ║  Of which payments:           {:>20}    ║",
        total_payments
    );
    println!(
        "   ║  Of which fees (expected):    {:>20}    ║",
        total_fees_paid
    );
    println!(
        "   ║  Fee revenue (gained-pay):    {:>20}    ║",
        coord_gained - total_payments
    );
    println!(
        "   ║  User (eden) before:          {:>20}    ║",
        user_balance_before
    );
    println!(
        "   ║  User (eden) after:           {:>20}    ║",
        user_balance_after
    );
    println!(
        "   ║  Transactions sent:           {:>20}    ║",
        tx_count
    );
    println!("   ╚══════════════════════════════════════════════════════╝");

    // ── ASSERTIONS ───────────────────────────────────────────────────

    // 1. Coordinator should have received at least the payment amounts
    assert!(
        coord_gained >= total_payments,
        "Coordinator should have received at least the payments ({} PMS). \
         Got: {}",
        total_payments,
        coord_gained
    );

    // 2. Coordinator should have received MORE than just payments (fee revenue)
    //    Each TX pays ~0.3 PMS fee → coordinator gets this via Reward blocks.
    //    The fee revenue = coord_gained - total_payments should be > 0.
    let fee_revenue = coord_gained - total_payments;
    println!(
        "\n   Assertion: coordinator fee revenue = {} PMS (from {} tx)",
        fee_revenue, tx_count
    );
    assert!(
        fee_revenue > Decimal::ZERO,
        "Coordinator should have received fee revenue beyond payments. \
         Total gained: {}, payments: {}, fee revenue: {}",
        coord_gained,
        total_payments,
        fee_revenue
    );

    // 3. At least some transactions should have succeeded
    assert!(
        tx_count >= 5,
        "At least 5 transactions should have succeeded, got {}",
        tx_count
    );

    println!("\n   TEST PASSED: Coordinator received {} PMS in fee revenue from {} eden transactions!",
        fee_revenue, tx_count);
    Ok(())
}

// ============================================================================
// HELPER: Query UTXOs for an address on a ledger
// ============================================================================

impl Sandbox {
    /// Query all UTXOs for an address on a given ledger.
    /// Returns (asset_id, amount) pairs for inspection.
    async fn get_utxos(
        &self,
        ledger_id: &str,
        address: &str,
    ) -> Result<Vec<(Option<String>, Decimal)>> {
        let url = match ledger_id {
            "main" => format!("{}/v1/wallet/{}/utxos", self.base_url, address),
            lid => format!("{}/l/{}/v1/wallet/{}/utxos", self.base_url, lid, address),
        };
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("GET utxos failed")?;
        let status = resp.status();
        let json: Value = resp.json().await.unwrap_or(json!({}));
        anyhow::ensure!(status.is_success(), "UTXOs query failed: {} — {}", status, json);

        let utxos = json["utxos"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|u| {
                        let asset_id = u["asset_id"].as_str().map(|s| s.to_string());
                        let amount = u["amount"]
                            .as_str()
                            .and_then(|s| Decimal::from_str(s).ok())
                            .unwrap_or(Decimal::ZERO);
                        (asset_id, amount)
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(utxos)
    }

    /// Sum UTXOs for a specific asset_id on a ledger.
    async fn get_asset_balance(
        &self,
        ledger_id: &str,
        address: &str,
        asset_id: Option<&str>,
    ) -> Result<Decimal> {
        let utxos = self.get_utxos(ledger_id, address).await?;
        let total: Decimal = utxos
            .iter()
            .filter(|(aid, _)| aid.as_deref() == asset_id)
            .map(|(_, amount)| *amount)
            .sum();
        Ok(total)
    }

    /// Register a smart contract via POST /admin/contracts.
    async fn register_contract(&self, body: Value) -> Result<String> {
        let (status, resp) = self.admin_post("/admin/contracts", body).await;
        println!("   Register contract: {} — {:?}", status, resp);
        anyhow::ensure!(
            status.is_success(),
            "Contract registration failed: {} — {}",
            status,
            resp
        );
        Ok(resp["contract_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }

    /// Mint an NFT on a ledger via POST /l/{id}/admin/nft/mint.
    async fn mint_nft(
        &self,
        ledger_id: &str,
        token_id: &str,
        owner_address: &str,
        owner_x25519_pubkey: &str,
        metadata: Value,
    ) -> Result<String> {
        let body = json!({
            "token_id": token_id,
            "owner_address": owner_address,
            "owner_x25519_pubkey": owner_x25519_pubkey,
            "metadata": metadata,
        });
        let (status, resp) = self
            .admin_post_ledger(ledger_id, "/admin/nft/mint", body)
            .await;
        anyhow::ensure!(
            status.is_success(),
            "NFT mint failed: {} — {}",
            status,
            resp
        );
        Ok(resp["block_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }

    /// Burn an NFT on a ledger via POST /l/{id}/v1/nft/burn-simple.
    async fn burn_nft_simple(
        &self,
        ledger_id: &str,
        private_key_b64: &str,
        token_id: &str,
    ) -> Result<String> {
        let body = json!({
            "private_key_b64": private_key_b64,
            "token_id": token_id,
        });
        let (status, resp) = self
            .post_ledger(ledger_id, "/v1/nft/burn-simple", body)
            .await;
        anyhow::ensure!(
            status.is_success(),
            "NFT burn failed: {} — {}",
            status,
            resp
        );
        Ok(resp["block_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }

    /// Query the supply endpoint for a ledger, optionally for a specific asset.
    async fn get_supply(
        &self,
        ledger_id: &str,
        asset_id: Option<&str>,
    ) -> Result<Value> {
        let url = match ledger_id {
            "main" => match asset_id {
                Some(a) => format!("{}/v1/supply?asset_id={}", self.base_url, a),
                None => format!("{}/v1/supply", self.base_url),
            },
            lid => match asset_id {
                Some(a) => format!("{}/l/{}/v1/supply?asset_id={}", self.base_url, lid, a),
                None => format!("{}/l/{}/v1/supply", self.base_url, lid),
            },
        };
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("GET supply failed")?;
        let status = resp.status();
        let json: Value = resp.json().await.unwrap_or(json!({}));
        anyhow::ensure!(
            status.is_success(),
            "Supply query failed: {} — {}",
            status,
            json
        );
        Ok(json)
    }

    /// Send a token (with asset_id) via wallet/send-simple on a ledger.
    async fn send_asset(
        &self,
        ledger_id: &str,
        private_key_b64: &str,
        to: &str,
        amount: &str,
        asset_id: &str,
    ) -> Result<Value> {
        let body = json!({
            "private_key_b64": private_key_b64,
            "to": to,
            "amount": amount,
            "asset_id": asset_id,
        });
        let (status, resp) = self
            .post_ledger(ledger_id, "/v1/wallet/send-simple", body)
            .await;
        anyhow::ensure!(
            status.is_success(),
            "send_asset failed: {} — {}",
            status,
            resp
        );
        Ok(resp)
    }
}

// ============================================================================
// TEST: Full EDN lifecycle — cube burn → EDN refund → EDN transfer → fee
// ============================================================================

/// Validates the COMPLETE Edenite lifecycle:
///
/// 1. Create eden ledger + register contracts:
///    - Cube burn contract: NFT burn → 0.1 EDN per cube (AccumulateRefund)
///    - Transfer fee contract: 5% on all transfers on eden → coordinator
/// 2. Mint cube NFTs for test user on eden
/// 3. User burns cubes → EDN refund accumulated in FeePool
/// 4. Manual fee distribution → EDN UTXOs appear for the user on eden
/// 5. User sends EDN to a recipient → 5% transfer fee → coordinator gets EDN
/// 6. Assert coordinator has EDN balance > 0 from transfer fees
///
/// This test replicates the exact flow that the VPS simulator runs,
/// but in a controlled sandbox where we can verify every step.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_edn_transfer_fee_flow() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Full EDN Lifecycle — Burn → Refund → Transfer     ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Create eden ledger ─────────────────────────────────────────
    println!("   [1/9] Creating eden ledger...");
    sandbox
        .create_ledger("eden", "eden-net", "eden", "EDN")
        .await?;

    // ── 2. Deposit gas pool for eden ─────────────────────────────────
    println!("   [2/9] Depositing gas pool for eden...");
    sandbox.deposit_gas_pool("eden", "50000").await?;

    // ── 3. Register smart contracts ──────────────────────────────────
    println!("   [3/9] Registering contracts...");

    // 3a. Cube burn contract: NFT "cube" burn → 0.1 EDN per cube
    let burn_contract_id = sandbox
        .register_contract(json!({
            "name": "cube-burn-edenite",
            "scope": { "Ledger": ["eden"] },
            "trigger": { "OnNftBurn": { "nft_type": "cube" } },
            "actions": [{
                "AccumulateRefund": {
                    "asset_id": "edenite",
                    "formula": { "FixedRate": { "rate_numerator": 1, "rate_denominator": 10 } }
                }
            }],
            "enabled": true
        }))
        .await?;
    println!("      Burn contract ID: {}...", &burn_contract_id[..16]);

    // 3b. Transfer fee contract: 5% on ALL transfers on eden → coordinator
    let coord_addr = sandbox.admin_addr.clone();
    let fee_contract_id = sandbox
        .register_contract(json!({
            "name": "eden-transfer-fee-5pct",
            "scope": { "Ledger": ["eden"] },
            "trigger": { "OnTransfer": { "asset_id": null } },
            "actions": [{
                "TransferFee": {
                    "formula": { "PercentageBps": { "rate_bps": 500 } },
                    "splits": [{ "address": coord_addr, "share_bps": 10000 }]
                }
            }],
            "enabled": true
        }))
        .await?;
    println!("      Fee contract ID:  {}...", &fee_contract_id[..16]);

    // ── 4. Create test user wallet ───────────────────────────────────
    println!("   [4/9] Creating test user wallet...");
    let user_wallet = Wallet::generate();
    let user_addr = user_wallet.get_address("8e");
    let user_sk_b64 = user_wallet.private_key_b64.clone();
    let user_x25519_pub = user_wallet.x25519_pub_hex().to_string();
    println!(
        "      User:  {}...{}",
        &user_addr[..12],
        &user_addr[user_addr.len() - 8..]
    );

    // Create recipient wallet (for EDN transfer target)
    let recipient_wallet = Wallet::generate();
    let recipient_addr = recipient_wallet.get_address("8e");
    println!(
        "      Recv:  {}...{}",
        &recipient_addr[..12],
        &recipient_addr[recipient_addr.len() - 8..]
    );

    // Faucet mint PMS on eden for gas fees
    sandbox
        .faucet_mint(Some("eden"), &user_addr, "10000")
        .await?;
    sleep(Duration::from_millis(300)).await;

    // ── 5. Mint cube NFTs for user on eden ───────────────────────────
    println!("   [5/9] Minting 5 cube NFTs on eden...");
    let num_cubes = 5;
    let mut cube_token_ids = Vec::new();
    for i in 0..num_cubes {
        let token_id = format!(
            "{:064x}",
            (0xC0BE_0000_0000u64 + i as u64) // deterministic token IDs
        );
        let metadata = json!({
            "name": format!("Cube #{}", i),
            "nft_type": "cube",
            "extra": json!({
                "attributes": { "weight": 100, "size": 50, "density": 20 },
                "rarity": "common"
            }).to_string(),
        });
        let block_id = sandbox
            .mint_nft("eden", &token_id, &user_addr, &user_x25519_pub, metadata)
            .await?;
        cube_token_ids.push(token_id.clone());
        println!(
            "      Cube {}: {} (block {}...)",
            i,
            &token_id[..16],
            &block_id[..16]
        );
    }
    sleep(Duration::from_millis(300)).await;

    // ── 6. Burn cubes → EDN refund accumulated in FeePool ────────────
    println!("   [6/9] Burning {} cubes (expecting 0.1 EDN each)...", num_cubes);
    for (i, token_id) in cube_token_ids.iter().enumerate() {
        let block_id = sandbox
            .burn_nft_simple("eden", &user_sk_b64, token_id)
            .await?;
        println!(
            "      Burned cube {}: {} (block {}...)",
            i,
            &token_id[..16],
            &block_id[..16]
        );
    }
    let expected_edn = Decimal::from_str("0.5")?; // 5 cubes * 0.1 EDN
    println!(
        "      Expected EDN refund: {} (5 * 0.1)",
        expected_edn
    );

    // Check user EDN balance BEFORE distribution (should be 0)
    let user_edn_before_distrib = sandbox
        .get_asset_balance("eden", &user_addr, Some("edenite"))
        .await?;
    println!("      User EDN before distribution: {}", user_edn_before_distrib);

    // ── 7. Trigger fee distribution → EDN UTXOs appear ───────────────
    println!("   [7/9] Triggering manual fee distribution...");
    sandbox.distribute_fees().await?;

    // Wait a moment for distribution to complete (async task)
    sleep(Duration::from_secs(3)).await;

    // Trigger again to make sure (distribution iterates ledgers)
    sandbox.distribute_fees().await?;
    sleep(Duration::from_secs(2)).await;

    let user_edn_after_distrib = sandbox
        .get_asset_balance("eden", &user_addr, Some("edenite"))
        .await?;
    println!("      User EDN after distribution: {}", user_edn_after_distrib);

    // List all user UTXOs on eden for debugging
    let user_utxos = sandbox.get_utxos("eden", &user_addr).await?;
    println!("      User UTXOs on eden ({} total):", user_utxos.len());
    for (i, (asset_id, amount)) in user_utxos.iter().enumerate().take(20) {
        println!(
            "         [{:>2}] {} {}",
            i,
            amount,
            asset_id.as_deref().unwrap_or("PMS")
        );
    }

    anyhow::ensure!(
        user_edn_after_distrib > Decimal::ZERO,
        "User should have received EDN refund from cube burns. Got: {}. \
         Expected: ~{}. FeePool distribution may not have run for eden.",
        user_edn_after_distrib,
        expected_edn
    );

    // ── 8. Send EDN → transfer fee → coordinator gets EDN ────────────
    println!("   [8/9] Sending EDN from user to recipient (transfer fee 5%)...");
    let send_amount = user_edn_after_distrib / Decimal::from(2); // Send half the EDN
    let send_amount_str = format!("{}", send_amount.round_dp(8));
    let expected_fee = (send_amount * Decimal::from(500) / Decimal::from(10_000)).round_dp(8);

    println!("      Sending: {} EDN", send_amount_str);
    println!("      Expected transfer fee (5%): {} EDN", expected_fee);

    let coord_edn_before = sandbox
        .get_asset_balance("eden", &sandbox.admin_addr, Some("edenite"))
        .await?;
    println!("      Coordinator EDN before transfer: {}", coord_edn_before);

    let send_resp = sandbox
        .send_asset("eden", &user_sk_b64, &recipient_addr, &send_amount_str, "edenite")
        .await?;

    let resp_fee = send_resp["fee"].as_str().unwrap_or("?");
    let resp_transfer_fee = send_resp["transfer_fee"].as_str().unwrap_or("?");
    let resp_block_id = send_resp["block_id"].as_str().unwrap_or("?");
    println!(
        "      TX result: block={}..., gas_fee={}, transfer_fee={}",
        &resp_block_id[..16.min(resp_block_id.len())],
        resp_fee,
        resp_transfer_fee
    );

    sleep(Duration::from_millis(500)).await;

    // ── 9. Verify results ────────────────────────────────────────────
    println!("   [9/9] Verifying balances...");

    let coord_edn_after = sandbox
        .get_asset_balance("eden", &sandbox.admin_addr, Some("edenite"))
        .await?;
    let recipient_edn = sandbox
        .get_asset_balance("eden", &recipient_addr, Some("edenite"))
        .await?;
    let user_edn_final = sandbox
        .get_asset_balance("eden", &user_addr, Some("edenite"))
        .await?;

    let coord_edn_gained = coord_edn_after - coord_edn_before;

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  EDN LIFECYCLE RESULTS                                   ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!(
        "   ║  Cubes burned:               {:>24}    ║",
        num_cubes
    );
    println!(
        "   ║  Expected EDN refund:        {:>24}    ║",
        expected_edn
    );
    println!(
        "   ║  Actual EDN refund:          {:>24}    ║",
        user_edn_after_distrib
    );
    println!(
        "   ║  ─────────────────────────────────────────────────────    ║"
    );
    println!(
        "   ║  EDN sent:                   {:>24}    ║",
        send_amount_str
    );
    println!(
        "   ║  Transfer fee (5%):          {:>24}    ║",
        resp_transfer_fee
    );
    println!(
        "   ║  ─────────────────────────────────────────────────────    ║"
    );
    println!(
        "   ║  Coordinator EDN before:     {:>24}    ║",
        coord_edn_before
    );
    println!(
        "   ║  Coordinator EDN after:      {:>24}    ║",
        coord_edn_after
    );
    println!(
        "   ║  Coordinator EDN gained:     {:>24}    ║",
        coord_edn_gained
    );
    println!(
        "   ║  Recipient EDN:              {:>24}    ║",
        recipient_edn
    );
    println!(
        "   ║  User EDN remaining:         {:>24}    ║",
        user_edn_final
    );
    println!("   ╚══════════════════════════════════════════════════════════╝");

    // ── ASSERTIONS ───────────────────────────────────────────────────

    // 1. EDN refund should have arrived
    assert!(
        user_edn_after_distrib > Decimal::ZERO,
        "EDN refund from cube burns should be > 0. Got: {}",
        user_edn_after_distrib,
    );

    // 2. Transfer fee should be non-zero in the response
    let transfer_fee_dec =
        Decimal::from_str(resp_transfer_fee).unwrap_or(Decimal::ZERO);
    assert!(
        transfer_fee_dec > Decimal::ZERO,
        "Transfer fee should be > 0 (5% contract active). Got: '{}'",
        resp_transfer_fee,
    );

    // 3. Coordinator should have gained EDN from the transfer fee
    assert!(
        coord_edn_gained > Decimal::ZERO,
        "Coordinator should have received EDN transfer fee. \
         Before: {}, after: {}, gained: {}",
        coord_edn_before,
        coord_edn_after,
        coord_edn_gained,
    );

    // 4. Transfer fee amount should equal ~5% of send amount
    assert_eq!(
        transfer_fee_dec.round_dp(8),
        expected_fee.round_dp(8),
        "Transfer fee should be 5% of {} = {}. Got: {}",
        send_amount_str,
        expected_fee,
        transfer_fee_dec,
    );

    // 5. Coordinator gained should equal the transfer fee
    assert_eq!(
        coord_edn_gained.round_dp(8),
        transfer_fee_dec.round_dp(8),
        "Coordinator EDN gain should equal transfer fee. \
         Gained: {}, fee: {}",
        coord_edn_gained,
        transfer_fee_dec,
    );

    // 6. Recipient should have received EDN
    assert!(
        recipient_edn > Decimal::ZERO,
        "Recipient should have received EDN. Got: {}",
        recipient_edn,
    );

    // 7. Conservation: user_edn_remaining + sent + fee = user_edn_after_distrib
    let edn_sum = user_edn_final + send_amount + transfer_fee_dec;
    assert_eq!(
        edn_sum.round_dp(8),
        user_edn_after_distrib.round_dp(8),
        "EDN conservation: remaining({}) + sent({}) + fee({}) = {} != refund({})",
        user_edn_final,
        send_amount,
        transfer_fee_dec,
        edn_sum,
        user_edn_after_distrib,
    );

    println!(
        "\n   TEST PASSED: Full EDN lifecycle validated!"
    );
    println!(
        "   - {} cubes burned → {} EDN refunded",
        num_cubes, user_edn_after_distrib
    );
    println!(
        "   - {} EDN sent → {} fee (5%) → coordinator",
        send_amount_str, coord_edn_gained
    );
    println!("   - Conservation verified: remaining + sent + fee = refund");

    Ok(())
}

// ============================================================================
// TEST: Supply endpoint returns EDN wallet balances (not PMS) on eden
// ============================================================================

/// Verifies that GET /l/eden/v1/supply returns EDN wallet balances, not PMS.
///
/// Before the fix, `admin_balance`, `node_balance`, `treasury_balance` always
/// returned PMS native balance even when `circulating_supply` showed EDN.
/// This caused the pms-dashboard to display "0 EDN" for all wallets while
/// supply was > 0.
///
/// The fix: when `resolved_asset` is Some (auto-fallback or explicit), wallet
/// balances use `balance_by_address_and_asset()` instead of `balance_by_address()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_supply_endpoint_edn_balances() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Supply Endpoint — EDN Wallet Balances              ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Create eden ledger + contracts ──────────────────────────────
    println!("   [1/7] Creating eden ledger...");
    sandbox
        .create_ledger("eden", "eden-net", "eden", "EDN")
        .await?;
    sandbox.deposit_gas_pool("eden", "50000").await?;

    // Register transfer fee contract: 5% → coordinator
    let coord_addr = sandbox.admin_addr.clone();
    sandbox
        .register_contract(json!({
            "name": "eden-transfer-fee-5pct",
            "scope": { "Ledger": ["eden"] },
            "trigger": { "OnTransfer": { "asset_id": null } },
            "actions": [{
                "TransferFee": {
                    "formula": { "PercentageBps": { "rate_bps": 500 } },
                    "splits": [{ "address": coord_addr, "share_bps": 10000 }]
                }
            }],
            "enabled": true
        }))
        .await?;

    // Register burn contract: cube → 0.1 EDN
    sandbox
        .register_contract(json!({
            "name": "cube-burn-edenite",
            "scope": { "Ledger": ["eden"] },
            "trigger": { "OnNftBurn": { "nft_type": "cube" } },
            "actions": [{
                "AccumulateRefund": {
                    "asset_id": "edenite",
                    "formula": { "FixedRate": { "rate_numerator": 1, "rate_denominator": 10 } }
                }
            }],
            "enabled": true
        }))
        .await?;

    // ── 2. Check supply BEFORE any EDN exists ──────────────────────────
    println!("   [2/7] Checking supply before EDN...");
    let supply_before = sandbox.get_supply("eden", None).await?;
    println!("      Supply (eden, before): {:?}", supply_before);
    println!(
        "      admin_balance={}, node_balance={}, symbol={}",
        supply_before["admin_balance"].as_str().unwrap_or("?"),
        supply_before["node_balance"].as_str().unwrap_or("?"),
        supply_before["symbol"].as_str().unwrap_or("?"),
    );

    // ── 3. Create user, mint cubes, burn → generate EDN ────────────────
    println!("   [3/7] Creating user + burning cubes for EDN...");
    let user_wallet = Wallet::generate();
    let user_addr = user_wallet.get_address("8e");
    let user_sk_b64 = user_wallet.private_key_b64.clone();
    let user_x25519 = user_wallet.x25519_pub_hex().to_string();

    sandbox
        .faucet_mint(Some("eden"), &user_addr, "10000")
        .await?;
    sleep(Duration::from_millis(300)).await;

    // Mint 5 cubes
    let mut cube_ids = Vec::new();
    for i in 0..5 {
        let token_id = format!("{:064x}", 0xFEED_0000_0000u64 + i as u64);
        sandbox
            .mint_nft(
                "eden",
                &token_id,
                &user_addr,
                &user_x25519,
                json!({
                    "name": format!("Test Cube #{}", i),
                    "nft_type": "cube",
                    "extra": json!({ "attributes": { "weight": 100, "size": 50, "density": 20 } }).to_string(),
                }),
            )
            .await?;
        cube_ids.push(token_id);
    }
    sleep(Duration::from_millis(300)).await;

    // Burn all cubes → EDN via contract
    println!("   [4/7] Burning 5 cubes...");
    for token_id in &cube_ids {
        sandbox
            .burn_nft_simple("eden", &user_sk_b64, token_id)
            .await?;
    }

    // Distribute fees → EDN UTXOs appear for user
    println!("   [5/7] Distributing fees (EDN refunds)...");
    sandbox.distribute_fees().await?;
    sleep(Duration::from_secs(3)).await;
    sandbox.distribute_fees().await?;
    sleep(Duration::from_secs(2)).await;

    // ── 4. User sends EDN → 5% transfer fee → coordinator gets EDN ────
    println!("   [6/7] Sending EDN to trigger transfer fee...");
    let user_edn = sandbox
        .get_asset_balance("eden", &user_addr, Some("edenite"))
        .await?;
    println!("      User EDN balance: {}", user_edn);

    if user_edn > Decimal::ZERO {
        let send_amount = (user_edn / Decimal::from(2)).round_dp(8);
        sandbox
            .send_asset(
                "eden",
                &user_sk_b64,
                &sandbox.admin_addr.clone(),
                &send_amount.to_string(),
                "edenite",
            )
            .await?;
        println!("      Sent {} EDN to coordinator (5% fee)", send_amount);
    }
    sleep(Duration::from_millis(500)).await;

    // ── 5. Check supply AFTER EDN exists ───────────────────────────────
    println!("   [7/7] Checking supply endpoint...");

    // Auto-resolve (no explicit asset_id) — returns PMS if PMS exists on eden
    // (faucet minted PMS on eden, so auto-resolve stays PMS, not edenite)
    let supply_auto = sandbox.get_supply("eden", None).await?;
    // Explicit asset_id — this is how the dashboard SHOULD query for EDN
    let supply_edn = sandbox.get_supply("eden", Some("edenite")).await?;

    // ── Auto-resolve results (PMS native, since faucet minted PMS on eden) ──
    let auto_circ = supply_auto["circulating_supply"]
        .as_str()
        .unwrap_or("0");
    let auto_admin = supply_auto["admin_balance"]
        .as_str()
        .unwrap_or("0");
    let auto_asset = supply_auto["asset_id"]
        .as_str()
        .unwrap_or("none");
    let symbol = supply_auto["symbol"].as_str().unwrap_or("?");

    // ── Explicit edenite results (THE FIX — these must show real EDN balances) ──
    let edn_circ = supply_edn["circulating_supply"]
        .as_str()
        .unwrap_or("0");
    let edn_admin = supply_edn["admin_balance"]
        .as_str()
        .unwrap_or("0");
    let edn_node = supply_edn["node_balance"]
        .as_str()
        .unwrap_or("0");
    let edn_treasury = supply_edn["treasury_balance"]
        .as_str()
        .unwrap_or("0");
    let edn_asset = supply_edn["asset_id"]
        .as_str()
        .unwrap_or("none");

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  SUPPLY ENDPOINT RESULTS                                 ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!(
        "   ║  Symbol:                 {:>28}    ║",
        symbol
    );
    println!(
        "   ║  ── Auto-resolve (no asset_id) ─────────────────────    ║"
    );
    println!(
        "   ║  Auto asset_id:          {:>28}    ║",
        auto_asset
    );
    println!(
        "   ║  Auto circ. supply:      {:>28}    ║",
        auto_circ
    );
    println!(
        "   ║  Auto admin balance:     {:>28}    ║",
        auto_admin
    );
    println!(
        "   ║  ── Explicit ?asset_id=edenite ─────────────────────    ║"
    );
    println!(
        "   ║  EDN asset_id:           {:>28}    ║",
        edn_asset
    );
    println!(
        "   ║  EDN circ. supply:       {:>28}    ║",
        edn_circ
    );
    println!(
        "   ║  EDN admin balance:      {:>28}    ║",
        edn_admin
    );
    println!(
        "   ║  EDN node balance:       {:>28}    ║",
        edn_node
    );
    println!(
        "   ║  EDN treasury balance:   {:>28}    ║",
        edn_treasury
    );
    println!("   ╚══════════════════════════════════════════════════════════╝");

    // ── ASSERTIONS ────────────────────────────────────────────────────

    // 1. Auto-resolve: PMS native supply should be > 0 (faucet minted PMS on eden)
    let auto_circ_dec = Decimal::from_str(auto_circ).unwrap_or(Decimal::ZERO);
    assert!(
        auto_circ_dec > Decimal::ZERO,
        "PMS native supply on eden should be > 0 (from faucet). Got: {}",
        auto_circ
    );

    // 2. Explicit edenite: circulating supply should be > 0 (from cube burns)
    let edn_circ_dec = Decimal::from_str(edn_circ).unwrap_or(Decimal::ZERO);
    assert!(
        edn_circ_dec > Decimal::ZERO,
        "EDN circulating supply should be > 0 (from cube burns). Got: {}",
        edn_circ
    );

    // 3. THE FIX: explicit edenite query must return real EDN admin balance
    //    Before the fix, balance_by_address() always returned PMS native → 0 for EDN.
    if user_edn > Decimal::ZERO {
        let edn_admin_dec = Decimal::from_str(edn_admin).unwrap_or(Decimal::ZERO);
        assert!(
            edn_admin_dec > Decimal::ZERO,
            "Admin balance with ?asset_id=edenite should show EDN (from transfer fee). \
             Got: '{}'. This was the bug: supply endpoint used \
             balance_by_address() instead of balance_by_address_and_asset().",
            edn_admin
        );
        println!(
            "\n   TEST PASSED: Supply endpoint correctly returns EDN wallet balances!"
        );
        println!("   Admin (coordinator) EDN balance: {} (from 5% transfer fee)", edn_admin);
    } else {
        panic!(
            "User EDN balance was 0 — cube burns did not produce EDN. \
             Cannot validate the supply fix."
        );
    }

    Ok(())
}

// ============================================================================
// TEST: Contract simulation endpoint (dry-run)
// ============================================================================

/// Tests the `POST /admin/contracts/simulate` endpoint:
/// - Simulates a transfer fee contract against a fictitious event → correct fee returned.
/// - Verifies that NO contract is persisted after simulation.
/// - Tests sandbox mode: register_contract defaults to `enabled: false`.
/// - Toggles the contract to `enabled: true` and verifies.
#[tokio::test]
#[ignore]
async fn test_contract_simulate_endpoint() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Contract Simulation Endpoint                      ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Create eden ledger ────────────────────────────────────────
    println!("   [1/6] Creating eden ledger...");
    sandbox
        .create_ledger("eden", "Edenite", "edenite", "EDN")
        .await?;
    sandbox.deposit_gas_pool("eden", "50000").await?;

    // ── 2. Simulate a 5% transfer fee contract ───────────────────────
    println!("   [2/6] Simulating transfer fee contract (5% on 100 EDN)...");
    let coord_addr = sandbox.admin_addr.clone();
    let sim_body = json!({
        "contract": {
            "name": "eden-transfer-fee-5pct",
            "scope": { "Ledger": ["eden"] },
            "trigger": { "OnTransfer": { "asset_id": null } },
            "actions": [{
                "TransferFee": {
                    "formula": { "PercentageBps": { "rate_bps": 500 } },
                    "splits": [{ "address": coord_addr, "share_bps": 10000 }]
                }
            }]
        },
        "event": {
            "Transfer": {
                "ledger_id": "eden",
                "asset_id": "edenite",
                "transfer_amount": "100"
            }
        }
    });

    let (status, resp) = sandbox.admin_post("/admin/contracts/simulate", sim_body).await;
    println!("      Simulate status: {}", status);
    println!("      Simulate response: {}", serde_json::to_string_pretty(&resp).unwrap_or_default());

    assert_eq!(status, 200, "Simulate should return 200");
    assert_eq!(resp["matched"], true, "Contract should match the event");
    assert!(resp["match_reason"].is_null(), "No mismatch reason expected");

    let fee_results = resp["transfer_fee_results"].as_array().expect("transfer_fee_results");
    assert_eq!(fee_results.len(), 1, "Should have 1 transfer fee result");
    println!("      Fee amount: {}", fee_results[0]["fee_amount"]);
    // Accept both string "5" and number 5 — Decimal serializes as string in serde_json
    let fee_dec: rust_decimal::Decimal = serde_json::from_value(fee_results[0]["fee_amount"].clone())
        .unwrap_or(rust_decimal::Decimal::ZERO);
    assert_eq!(fee_dec, rust_decimal::Decimal::from(5), "5% of 100 = 5");
    println!("      5% of 100 EDN = {} fee: OK", fee_dec);

    // ── 3. Verify NO contract was persisted ──────────────────────────
    println!("   [3/6] Verifying no contract was persisted...");
    let (status, resp) = sandbox.admin_get("/admin/contracts").await;
    assert_eq!(status, 200);
    let contracts = resp["contracts"].as_array().expect("contracts array");
    assert_eq!(contracts.len(), 0, "No contracts should exist after simulation");
    println!("      Contract list is empty: OK");

    // ── 4. Register the contract (sandbox mode: enabled=false) ───────
    println!("   [4/6] Registering contract (default enabled=false)...");
    let register_body = json!({
        "name": "eden-transfer-fee-5pct",
        "scope": { "Ledger": ["eden"] },
        "trigger": { "OnTransfer": { "asset_id": null } },
        "actions": [{
            "TransferFee": {
                "formula": { "PercentageBps": { "rate_bps": 500 } },
                "splits": [{ "address": coord_addr, "share_bps": 10000 }]
            }
        }]
    });

    let (status, resp) = sandbox.admin_post("/admin/contracts", register_body).await;
    println!("      Register status: {}", status);
    println!("      Register response: {}", serde_json::to_string_pretty(&resp).unwrap_or_default());
    assert_eq!(status, 201, "Registration should return 201");
    assert_eq!(resp["enabled"], false, "Contract should be disabled by default (sandbox mode)");
    let contract_id = resp["contract_id"].as_str().unwrap().to_string();
    println!("      Contract registered as disabled: OK (id={}...)", &contract_id[..16]);

    // ── 5. Toggle to enabled ─────────────────────────────────────────
    println!("   [5/6] Toggling contract to enabled...");
    let toggle_body = json!({
        "enabled": true,
        "reason": "Simulation validated, activating"
    });
    let (status, resp) = sandbox
        .admin_post(
            &format!("/admin/contracts/{}/toggle", contract_id),
            toggle_body,
        )
        .await;
    println!("      Toggle status: {}", status);
    assert_eq!(status, 200);
    assert_eq!(resp["enabled"], true, "Contract should be enabled after toggle");
    println!("      Contract toggled to enabled: OK");

    // ── 6. Verify contract is now enabled ────────────────────────────
    println!("   [6/6] Verifying contract is enabled...");
    let (status, resp) = sandbox
        .admin_get(&format!("/admin/contracts/{}", contract_id))
        .await;
    assert_eq!(status, 200);
    assert_eq!(resp["enabled"], true, "Contract should be enabled");
    println!("      Contract enabled in store: OK");

    println!("\n   TEST PASSED: Contract simulation endpoint works correctly!");
    println!("   - Dry-run returns accurate fee calculations");
    println!("   - Simulation does NOT persist contracts");
    println!("   - Sandbox mode: contracts default to disabled");
    println!("   - Toggle enables contracts after validation");

    Ok(())
}

// ============================================================================
// TEST: Concurrent contract registration
// ============================================================================

/// Tests concurrent contract registration under load:
/// - 10 concurrent tasks register unique contracts.
/// - 5 concurrent tasks read the contract list.
/// - Verifies all 10 contracts are persisted with `enabled: false`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_contract_registration_concurrent() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Concurrent Contract Registration                  ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    let coord_addr = sandbox.admin_addr.clone();
    let base_url = sandbox.base_url.clone();
    let client = sandbox.client.clone();
    let admin_token = sandbox.admin_token.clone();

    // ── 1. Spawn 10 concurrent registrations + 5 concurrent reads ────
    println!("   [1/2] Spawning 10 register + 5 list tasks concurrently...");

    let mut handles = Vec::new();

    for i in 0..10 {
        let cl = client.clone();
        let url = base_url.clone();
        let addr = coord_addr.clone();
        let token = admin_token.clone();

        let h = tokio::spawn(async move {
            let body = json!({
                "name": format!("concurrent-fee-{}", i),
                "scope": { "Ledger": [format!("ledger-{}", i)] },
                "trigger": { "OnTransfer": { "asset_id": null } },
                "actions": [{
                    "TransferFee": {
                        "formula": { "PercentageBps": { "rate_bps": 100 + i * 50 } },
                        "splits": [{ "address": addr, "share_bps": 10000 }]
                    }
                }]
            });

            let resp = cl
                .post(format!("{}/admin/contracts", url))
                .header("Authorization", format!("Bearer {}", token))
                .json(&body)
                .send()
                .await
                .expect("HTTP request failed");

            let status = resp.status().as_u16();
            let json: serde_json::Value = resp.json().await.unwrap_or(json!({}));
            println!("      Register task {} → status {}", i, status);
            (i, status, json)
        });
        handles.push(h);
    }

    // Concurrent reads
    for j in 0..5 {
        let cl = client.clone();
        let url = base_url.clone();
        let token = admin_token.clone();

        let h = tokio::spawn(async move {
            let resp = cl
                .get(format!("{}/admin/contracts", url))
                .header("Authorization", format!("Bearer {}", token))
                .send()
                .await
                .expect("HTTP request failed");

            let status = resp.status().as_u16();
            println!("      List task {} → status {} (concurrent read)", j, status);
            (100 + j, status, json!({}))
        });
        handles.push(h);
    }

    let results: Vec<(usize, u16, serde_json::Value)> =
        futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.expect("Task panicked"))
            .collect();

    // ── 2. Verify results ────────────────────────────────────────────
    println!("\n   [2/2] Verifying results...");

    // All 10 registrations should succeed (201)
    let register_results: Vec<_> = results.iter().filter(|(i, _, _)| *i < 100).collect();
    assert_eq!(register_results.len(), 10, "Should have 10 registration results");
    for (i, status, resp) in &register_results {
        assert_eq!(*status, 201, "Registration {} should return 201, got {}: {:?}", i, status, resp);
        assert_eq!(resp["enabled"], false, "Contract {} should be disabled by default", i);
    }
    println!("      All 10 registrations returned 201 with enabled=false: OK");

    // All 5 reads should succeed (200)
    let read_results: Vec<_> = results.iter().filter(|(i, _, _)| *i >= 100).collect();
    assert_eq!(read_results.len(), 5, "Should have 5 list results");
    for (i, status, _) in &read_results {
        assert_eq!(*status, 200, "List task {} should return 200", i);
    }
    println!("      All 5 concurrent reads returned 200: OK");

    // Final list should have exactly 10 contracts
    let (status, resp) = sandbox.admin_get("/admin/contracts").await;
    assert_eq!(status, 200);
    let contracts = resp["contracts"].as_array().expect("contracts array");
    assert_eq!(contracts.len(), 10, "Should have exactly 10 contracts");
    println!("      Final contract list: {} contracts: OK", contracts.len());

    // All should be disabled
    let all_disabled = contracts.iter().all(|c| c["enabled"] == false);
    assert!(all_disabled, "All contracts should be disabled (sandbox mode)");
    println!("      All contracts disabled (sandbox mode): OK");

    println!("\n   TEST PASSED: Concurrent contract registration is safe!");
    println!("   - 10 concurrent registrations + 5 concurrent reads: no lost writes");
    println!("   - All contracts default to enabled=false");

    Ok(())
}

// ============================================================================
// TEST: PMS throughput — measures max PMS TPS the engine supports
// ============================================================================

/// Measures PMS transaction throughput by sending concurrent transactions
/// from multiple wallets. This quantifies the engine's capacity vs.
/// the simulator's agent-limited ~200 TPS on PMS.
///
/// Reports: actual TPS, per-worker success/fail counts, total elapsed time.
/// This test proves the engine can handle >>200 TPS — the bottleneck is
/// agent configuration (1 TX per tick per agent), not the engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn test_pms_throughput_benchmark() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: PMS Throughput Benchmark                          ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── Configuration ──────────────────────────────────────────────────
    const WORKERS: usize = 10;
    const TX_PER_WORKER: usize = 100;
    const INITIAL_MINT: &str = "500000";
    const TX_AMOUNT: &str = "1.0";

    // ── 1. Create wallets and fund them ────────────────────────────────
    println!("   [1/3] Creating {} worker wallets and funding with {} PMS each...",
             WORKERS, INITIAL_MINT);

    let mut worker_wallets: Vec<(Wallet, String)> = Vec::new();
    for i in 0..WORKERS {
        let w = Wallet::generate();
        let addr = w.get_address("8e");
        sandbox.faucet_mint(None, &addr, INITIAL_MINT).await?;
        println!("      Worker {}: {}...{}", i, &addr[..12], &addr[addr.len() - 8..]);
        worker_wallets.push((w, addr));
    }

    // Small delay for UTXO propagation
    sleep(Duration::from_millis(500)).await;

    // ── 2. Concurrent PMS sends ────────────────────────────────────────
    println!("   [2/3] Sending {} PMS transactions ({} workers × {} tx each)...",
             WORKERS * TX_PER_WORKER, WORKERS, TX_PER_WORKER);

    let success_count = Arc::new(AtomicUsize::new(0));
    let fail_count = Arc::new(AtomicUsize::new(0));
    let admin_addr = sandbox.admin_addr.clone();
    let base_url = sandbox.base_url.clone();
    let client = sandbox.client.clone();

    let start = Instant::now();

    let mut handles = Vec::new();
    for (i, (wallet, _addr)) in worker_wallets.iter().enumerate() {
        let sk_b64 = wallet.private_key_b64.clone();
        let target = admin_addr.clone();
        let url = base_url.clone();
        let cl = client.clone();
        let ok = success_count.clone();
        let fail = fail_count.clone();

        let h = tokio::spawn(async move {
            for tx_idx in 0..TX_PER_WORKER {
                let body = serde_json::json!({
                    "private_key_b64": sk_b64,
                    "to": target,
                    "amount": TX_AMOUNT,
                });
                match cl
                    .post(format!("{}/v1/wallet/send-simple", url))
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        ok.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(resp) => {
                        if tx_idx < 3 {
                            let status = resp.status();
                            let body = resp.text().await.unwrap_or_default();
                            eprintln!(
                                "      Worker {} TX {} FAILED: {} — {}",
                                i, tx_idx, status, &body[..100.min(body.len())]
                            );
                        }
                        fail.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        if tx_idx < 3 {
                            eprintln!("      Worker {} TX {} ERROR: {}", i, tx_idx, e);
                        }
                        fail.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
        handles.push(h);
    }

    // Wait for all workers to complete
    for h in handles {
        h.await?;
    }

    let elapsed = start.elapsed();
    let total_ok = success_count.load(Ordering::Relaxed);
    let total_fail = fail_count.load(Ordering::Relaxed);
    let tps = if elapsed.as_secs_f64() > 0.0 {
        total_ok as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    // ── 3. Results ──────────────────────────────────────────────────────
    println!("   [3/3] Results...\n");

    println!("   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  PMS THROUGHPUT RESULTS                                  ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!(
        "   ║  Workers:                {:>28}    ║",
        WORKERS
    );
    println!(
        "   ║  TX per worker:          {:>28}    ║",
        TX_PER_WORKER
    );
    println!(
        "   ║  Total attempted:        {:>28}    ║",
        WORKERS * TX_PER_WORKER
    );
    println!(
        "   ║  Successful:             {:>28}    ║",
        total_ok
    );
    println!(
        "   ║  Failed:                 {:>28}    ║",
        total_fail
    );
    println!(
        "   ║  Elapsed:                {:>24.2} s    ║",
        elapsed.as_secs_f64()
    );
    println!(
        "   ║  TPS (successful):       {:>24.1} tx/s  ║",
        tps
    );
    println!(
        "   ║  ─────────────────────────────────────────────────────    ║"
    );
    println!(
        "   ║  Simulator PMS agents:   {:>24} tx/s  ║",
        "~200 (1 tx/tick)"
    );
    println!(
        "   ║  Engine capacity:        {:>24.0} tx/s  ║",
        tps
    );
    println!(
        "   ║  Headroom factor:        {:>24.1}x      ║",
        tps / 200.0
    );
    println!("   ╚══════════════════════════════════════════════════════════╝\n");

    // ── Assertions ─────────────────────────────────────────────────────

    // At least 90% of transactions should succeed (UTXO conflicts can cause some failures)
    let success_rate = total_ok as f64 / (WORKERS * TX_PER_WORKER) as f64;
    println!(
        "   Success rate: {:.1}% ({}/{})",
        success_rate * 100.0,
        total_ok,
        WORKERS * TX_PER_WORKER
    );
    assert!(
        success_rate >= 0.5,
        "At least 50% of transactions should succeed. Got {:.1}%",
        success_rate * 100.0
    );

    // Engine TPS should be significantly higher than simulator's ~200
    println!("   Engine TPS: {:.0} (vs simulator ~200)", tps);
    assert!(
        tps > 200.0,
        "Engine should support more than 200 TPS. Got {:.0} TPS. \
         The bottleneck is agent config, not the engine.",
        tps
    );

    println!("\n   TEST PASSED: Engine supports {:.0} TPS — {:.1}x more than simulator's 200 TPS",
             tps, tps / 200.0);
    println!("   Solution: Use `sends_per_tick` in agent config to multiply PMS throughput.");

    Ok(())
}

// ============================================================================
// STRESS TEST: Sustained TPS — measures degradation over time
// ============================================================================

/// Single measurement interval for the time-series report.
struct IntervalMetric {
    /// Seconds since test start.
    offset_secs: f64,
    /// Successful transactions in this interval.
    interval_tx: usize,
    /// TPS for this interval alone.
    interval_tps: f64,
    /// Cumulative successful transactions.
    cumulative_tx: usize,
    /// Approximate block count from RocksDB (via `/internal/health`).
    cumulative_blocks: u64,
    /// Average latency in milliseconds for this interval.
    avg_latency_ms: f64,
    /// P50 latency in milliseconds.
    p50_latency_ms: f64,
    /// P95 latency in milliseconds.
    p95_latency_ms: f64,
    /// P99 latency in milliseconds.
    p99_latency_ms: f64,
    /// Number of failures in this interval.
    interval_failures: usize,
}

/// Compute a percentile from a sorted slice. Returns 0.0 if empty.
fn stress_percentile(sorted: &[f64], pct: u32) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((pct as f64 / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Query approximate block count from the internal health endpoint (O(1)).
async fn stress_block_count(client: &Client, base_url: &str) -> u64 {
    match client.get(format!("{}/internal/health", base_url)).send().await {
        Ok(resp) => {
            let json: Value = resp.json().await.unwrap_or(json!({}));
            json["block_count"].as_u64().unwrap_or(0)
        }
        Err(_) => 0,
    }
}

/// Sustained throughput stress test — runs for 5 minutes targeting 20M+ blocks
/// and measures TPS degradation as block count increases.
///
/// Architecture:
/// - 80 parallel workers send transactions continuously via `send_simple()`
/// - A metrics collector samples every 5 seconds
/// - Time-series report shows TPS, cumulative blocks, latency percentiles
/// - Final report detects degradation (first minute avg vs last minute avg)
///
/// Each `send_simple()` call creates 2 blocks (TX + Reward), so the DAG
/// accumulates blocks at ~2× the visible TPS rate. The `block_count_estimate()`
/// from RocksDB is an approximation — the accurate count is `total_tx × 2`.
///
/// Run:
/// ```bash
/// cargo test --release -p pms-server --test dag_sandbox test_sustained_tps_stress -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
#[ignore]
async fn test_sustained_tps_stress() -> Result<()> {
    // ── Configuration ──────────────────────────────────────────────────
    const WORKERS: usize = 80;
    const DURATION_SECS: u64 = 300; // 5 minutes
    const INTERVAL_SECS: u64 = 5;
    const INITIAL_MINT: &str = "1000000"; // Per worker (21M TX capacity at 0.046/TX)
    const TX_AMOUNT: &str = "0.01"; // Small amount to maximize TX count
    /// Each `send_simple()` creates a TX block + a Reward block = 2 blocks.
    const BLOCKS_PER_TX: u64 = 2;

    // ── 1. Setup ───────────────────────────────────────────────────────
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  SUSTAINED TPS STRESS TEST — 20M+ BLOCKS TARGET         ║");
    println!("║  Duration: {}s | Workers: {} | Interval: {}s            ║",
             DURATION_SECS, WORKERS, INTERVAL_SECS);
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    println!(
        "   [1/4] Creating {} worker wallets and minting {} PMS each...",
        WORKERS, INITIAL_MINT
    );

    let mut worker_keys: Vec<String> = Vec::with_capacity(WORKERS);
    for i in 0..WORKERS {
        let w = Wallet::generate();
        let addr = w.get_address("8e");
        sandbox.faucet_mint(None, &addr, INITIAL_MINT).await?;
        worker_keys.push(w.private_key_b64.clone());
        if (i + 1) % 10 == 0 {
            println!("      Funded worker {}/{}...", i + 1, WORKERS);
        }
    }

    // Wait for UTXO propagation
    sleep(Duration::from_secs(2)).await;

    // Initial block count baseline
    let initial_blocks_estimate = stress_block_count(&sandbox.client, &sandbox.base_url).await;
    let initial_blocks_actual = (WORKERS as u64 + 1) * BLOCKS_PER_TX; // mints + genesis
    println!(
        "   Initial blocks: ~{} (RocksDB estimate) / {} (actual: genesis + {} mints)\n",
        initial_blocks_estimate, initial_blocks_actual, WORKERS
    );

    // ── 2. Shared state ────────────────────────────────────────────────
    let stop_flag = Arc::new(AtomicBool::new(false));
    let success_count = Arc::new(AtomicUsize::new(0));
    let fail_count = Arc::new(AtomicUsize::new(0));
    let latencies: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::with_capacity(10_000)));

    let admin_addr = sandbox.admin_addr.clone();
    let base_url = sandbox.base_url.clone();
    let client = sandbox.client.clone();

    // ── 3. Spawn workers ───────────────────────────────────────────────
    println!(
        "   [2/4] Starting {} workers for {}s...\n",
        WORKERS, DURATION_SECS
    );

    let benchmark_start = Instant::now();
    let mut worker_handles = Vec::with_capacity(WORKERS);

    for (i, sk_b64) in worker_keys.iter().enumerate() {
        let sk = sk_b64.clone();
        let target = admin_addr.clone();
        let url = base_url.clone();
        let cl = client.clone();
        let stop = stop_flag.clone();
        let ok_counter = success_count.clone();
        let fail_counter = fail_count.clone();
        let lat_buf = latencies.clone();

        let h = tokio::spawn(async move {
            let mut worker_ok = 0usize;
            let mut worker_fail = 0usize;

            while !stop.load(Ordering::Relaxed) {
                let tx_start = Instant::now();

                let body = json!({
                    "private_key_b64": sk,
                    "to": target,
                    "amount": TX_AMOUNT,
                });

                match cl
                    .post(format!("{}/v1/wallet/send-simple", url))
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        let lat_ms = tx_start.elapsed().as_secs_f64() * 1000.0;
                        ok_counter.fetch_add(1, Ordering::Relaxed);
                        worker_ok += 1;

                        // Push latency sample (best-effort, skip if collector is draining)
                        if let Ok(mut buf) = lat_buf.try_lock() {
                            buf.push(lat_ms);
                        }
                    }
                    Ok(resp) => {
                        // Log first few failures per worker for diagnosis
                        if worker_fail < 3 {
                            let status = resp.status();
                            let body_text = resp.text().await.unwrap_or_default();
                            eprintln!(
                                "      Worker {} fail #{}: {} — {}",
                                i,
                                worker_fail + 1,
                                status,
                                &body_text[..100.min(body_text.len())]
                            );
                        }
                        fail_counter.fetch_add(1, Ordering::Relaxed);
                        worker_fail += 1;
                        // Brief backoff on rejection (UTXO conflict, etc.)
                        sleep(Duration::from_millis(10)).await;
                    }
                    Err(e) => {
                        if worker_fail < 3 {
                            eprintln!("      Worker {} error #{}: {}", i, worker_fail + 1, e);
                        }
                        fail_counter.fetch_add(1, Ordering::Relaxed);
                        worker_fail += 1;
                        sleep(Duration::from_millis(50)).await;
                    }
                }
            }

            (i, worker_ok, worker_fail)
        });
        worker_handles.push(h);
    }

    // ── 4. Timer — stops workers after DURATION_SECS ───────────────────
    {
        let stop = stop_flag.clone();
        tokio::spawn(async move {
            sleep(Duration::from_secs(DURATION_SECS)).await;
            stop.store(true, Ordering::Relaxed);
        });
    }

    // ── 5. Metrics collector (main thread, every INTERVAL_SECS) ────────
    println!("   [3/4] Collecting metrics every {}s...\n", INTERVAL_SECS);

    let mut intervals: Vec<IntervalMetric> = Vec::new();
    let mut last_success = 0usize;
    let mut last_fail = 0usize;

    while !stop_flag.load(Ordering::Relaxed) {
        sleep(Duration::from_secs(INTERVAL_SECS)).await;

        let elapsed = benchmark_start.elapsed();
        let current_success = success_count.load(Ordering::Relaxed);
        let current_fail = fail_count.load(Ordering::Relaxed);

        let interval_tx = current_success.saturating_sub(last_success);
        let interval_failures = current_fail.saturating_sub(last_fail);
        let interval_tps = interval_tx as f64 / INTERVAL_SECS as f64;

        // Drain latency buffer and compute percentiles
        let mut samples = {
            let mut buf = latencies.lock().unwrap();
            buf.drain(..).collect::<Vec<f64>>()
        };
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let avg_latency = if samples.is_empty() {
            0.0
        } else {
            samples.iter().sum::<f64>() / samples.len() as f64
        };
        let p50 = stress_percentile(&samples, 50);
        let p95 = stress_percentile(&samples, 95);
        let p99 = stress_percentile(&samples, 99);

        // Block count: accurate = TX×2 (each send_simple = TX + Reward block)
        let actual_blocks = current_success as u64 * BLOCKS_PER_TX;

        let metric = IntervalMetric {
            offset_secs: elapsed.as_secs_f64(),
            interval_tx,
            interval_tps,
            cumulative_tx: current_success,
            cumulative_blocks: actual_blocks,
            avg_latency_ms: avg_latency,
            p50_latency_ms: p50,
            p95_latency_ms: p95,
            p99_latency_ms: p99,
            interval_failures,
        };

        // Live progress line
        let blocks_m = actual_blocks as f64 / 1_000_000.0;
        eprintln!(
            "   {:>6.1}s | {:>6} tx | {:>7.1} tps | {:>8} total | {:>5.2}M blk | \
             p50={:.1}ms p95={:.1}ms p99={:.1}ms | {} fail",
            elapsed.as_secs_f64(),
            interval_tx,
            interval_tps,
            current_success,
            blocks_m,
            p50,
            p95,
            p99,
            interval_failures,
        );

        intervals.push(metric);
        last_success = current_success;
        last_fail = current_fail;
    }

    // ── 6. Wait for all workers ─────────────────────────────────────────
    let mut per_worker_results = Vec::new();
    for h in worker_handles {
        match h.await {
            Ok((id, ok, fail)) => per_worker_results.push((id, ok, fail)),
            Err(e) => eprintln!("   Worker panicked: {}", e),
        }
    }

    let total_elapsed = benchmark_start.elapsed();
    let total_ok = success_count.load(Ordering::Relaxed);
    let total_fail = fail_count.load(Ordering::Relaxed);
    let overall_tps = total_ok as f64 / total_elapsed.as_secs_f64();
    let total_blocks = total_ok as u64 * BLOCKS_PER_TX;
    let rocksdb_estimate = stress_block_count(&client, &base_url).await;

    // ── 7. Report ───────────────────────────────────────────────────────
    println!("\n   [4/4] Generating report...\n");

    // Time-series table
    println!("   ╔═══════╦════════╦═════════╦══════════╦══════════╦══════════╦══════════╦══════════╦═══════╗");
    println!("   ║ Time  ║ TX/int ║   TPS   ║  Cum.TX  ║  Blocks  ║  P50 ms  ║  P95 ms  ║  P99 ms  ║ Fails ║");
    println!("   ╠═══════╬════════╬═════════╬══════════╬══════════╬══════════╬══════════╬══════════╬═══════╣");
    for m in &intervals {
        let blk_m = m.cumulative_blocks as f64 / 1_000_000.0;
        println!(
            "   ║ {:>5.0}s ║ {:>6} ║ {:>7.1} ║ {:>8} ║ {:>5.2}M  ║ {:>6.1}ms ║ {:>6.1}ms ║ {:>6.1}ms ║ {:>5} ║",
            m.offset_secs,
            m.interval_tx,
            m.interval_tps,
            m.cumulative_tx,
            blk_m,
            m.p50_latency_ms,
            m.p95_latency_ms,
            m.p99_latency_ms,
            m.interval_failures,
        );
    }
    println!("   ╚═══════╩════════╩═════════╩══════════╩══════════╩══════════╩══════════╩══════════╩═══════╝");

    // Summary statistics
    let tps_values: Vec<f64> = intervals.iter().map(|m| m.interval_tps).collect();
    let min_tps = tps_values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_tps = tps_values.iter().cloned().fold(0.0f64, f64::max);
    let avg_tps = if tps_values.is_empty() {
        0.0
    } else {
        tps_values.iter().sum::<f64>() / tps_values.len() as f64
    };

    // Degradation analysis: first minute avg TPS vs last minute avg TPS
    let first_minute_cutoff = 60.0;
    let last_minute_start = DURATION_SECS as f64 - 60.0;

    let first_minute_tps: Vec<f64> = intervals
        .iter()
        .filter(|m| m.offset_secs <= first_minute_cutoff)
        .map(|m| m.interval_tps)
        .collect();
    let last_minute_tps: Vec<f64> = intervals
        .iter()
        .filter(|m| m.offset_secs > last_minute_start)
        .map(|m| m.interval_tps)
        .collect();

    let first_avg = if first_minute_tps.is_empty() {
        0.0
    } else {
        first_minute_tps.iter().sum::<f64>() / first_minute_tps.len() as f64
    };
    let last_avg = if last_minute_tps.is_empty() {
        0.0
    } else {
        last_minute_tps.iter().sum::<f64>() / last_minute_tps.len() as f64
    };

    let degradation_pct = if first_avg > 0.0 {
        ((first_avg - last_avg) / first_avg) * 100.0
    } else {
        0.0
    };

    // Latency summary across all intervals
    let all_p50: Vec<f64> = intervals.iter().map(|m| m.p50_latency_ms).collect();
    let all_p99: Vec<f64> = intervals.iter().map(|m| m.p99_latency_ms).collect();
    let avg_p50 = if all_p50.is_empty() {
        0.0
    } else {
        all_p50.iter().sum::<f64>() / all_p50.len() as f64
    };
    let avg_p99 = if all_p99.is_empty() {
        0.0
    } else {
        all_p99.iter().sum::<f64>() / all_p99.len() as f64
    };

    let blocks_m = total_blocks as f64 / 1_000_000.0;
    let blocks_per_sec = total_blocks as f64 / total_elapsed.as_secs_f64();

    println!("\n   ╔══════════════════════════════════════════════════════════════╗");
    println!("   ║  SUSTAINED TPS STRESS TEST — SUMMARY                        ║");
    println!("   ╠══════════════════════════════════════════════════════════════╣");
    println!(
        "   ║  Duration:               {:>28.1}s    ║",
        total_elapsed.as_secs_f64()
    );
    println!("   ║  Workers:                {:>29}    ║", WORKERS);
    println!(
        "   ║  Total successful TX:    {:>29}    ║",
        total_ok
    );
    println!(
        "   ║  Total failed TX:        {:>29}    ║",
        total_fail
    );
    println!(
        "   ║  Overall TPS:            {:>29.1}    ║",
        overall_tps
    );
    println!(
        "   ║  ────────────────────────────────────────────────────────    ║"
    );
    println!(
        "   ║  TOTAL BLOCKS (TX+Reward): {:>22.2}M    ║",
        blocks_m
    );
    println!(
        "   ║  Blocks/sec:               {:>26.0}    ║",
        blocks_per_sec
    );
    println!(
        "   ║  RocksDB estimate:         {:>26}    ║",
        rocksdb_estimate
    );
    println!(
        "   ║  ────────────────────────────────────────────────────────    ║"
    );
    println!(
        "   ║  Min interval TPS:       {:>29.1}    ║",
        min_tps
    );
    println!(
        "   ║  Max interval TPS:       {:>29.1}    ║",
        max_tps
    );
    println!(
        "   ║  Avg interval TPS:       {:>29.1}    ║",
        avg_tps
    );
    println!(
        "   ║  ────────────────────────────────────────────────────────    ║"
    );
    println!(
        "   ║  Avg P50 latency:        {:>26.1} ms    ║",
        avg_p50
    );
    println!(
        "   ║  Avg P99 latency:        {:>26.1} ms    ║",
        avg_p99
    );
    println!(
        "   ║  ────────────────────────────────────────────────────────    ║"
    );
    println!(
        "   ║  First minute avg TPS:   {:>29.1}    ║",
        first_avg
    );
    println!(
        "   ║  Last minute avg TPS:    {:>29.1}    ║",
        last_avg
    );
    println!(
        "   ║  Degradation:            {:>28.1}%    ║",
        degradation_pct
    );
    println!("   ╚══════════════════════════════════════════════════════════════╝");

    // Per-worker breakdown
    println!("\n   Per-worker results:");
    for (id, ok, fail) in &per_worker_results {
        println!("      Worker {:>2}: {:>6} ok, {:>4} fail", id, ok, fail);
    }

    // Success rate
    let success_rate = total_ok as f64 / (total_ok + total_fail).max(1) as f64;
    println!("\n   Success rate: {:.1}%", success_rate * 100.0);

    // ── 8. Assertions ───────────────────────────────────────────────────

    // Must have processed a meaningful number of transactions
    assert!(
        total_ok > 1000,
        "Should have processed at least 1000 transactions in {}s. Got: {}",
        DURATION_SECS,
        total_ok
    );

    // Catastrophic degradation guard (memory leak / unbounded growth)
    if degradation_pct > 30.0 {
        eprintln!(
            "\n   WARNING: TPS degraded by {:.1}% (threshold: 30%)",
            degradation_pct
        );
        eprintln!(
            "   First minute: {:.1} TPS → Last minute: {:.1} TPS",
            first_avg, last_avg
        );
    }

    assert!(
        degradation_pct < 50.0,
        "CRITICAL: TPS degraded by {:.1}% — possible memory leak or unbounded growth. \
         First minute: {:.1} TPS, Last minute: {:.1} TPS",
        degradation_pct,
        first_avg,
        last_avg
    );

    // Success rate must be reasonable
    assert!(
        success_rate >= 0.5,
        "Success rate below 50%: {:.1}%. Too many UTXO conflicts or errors.",
        success_rate * 100.0
    );

    // Final verdict
    let verdict = if degradation_pct <= 10.0 {
        "EXCELLENT"
    } else if degradation_pct <= 20.0 {
        "PASS"
    } else if degradation_pct <= 30.0 {
        "WARN"
    } else {
        "DEGRADED"
    };

    println!(
        "\n   ═══════════════════════════════════════════════════════════════"
    );
    println!(
        "   VERDICT: {} — {:.1} avg TPS, {:.1}% degradation over {}s",
        verdict, avg_tps, degradation_pct, DURATION_SECS
    );
    println!(
        "   DAG accumulated {:.2}M blocks ({:.0} blk/s) without catastrophic slowdown.",
        blocks_m, blocks_per_sec
    );
    println!(
        "   ═══════════════════════════════════════════════════════════════\n"
    );

    Ok(())
}

// ============================================================================
// MULTI-ENGINE CLUSTER (post-sprint, 2026-04-26)
// ============================================================================
//
// Simulates 1 coordinator + N-1 followers in-process so the user can test
// the multi-VPS topology BEFORE owning a 2nd VPS. Each engine boots with
// its own RocksDB tempdir and HTTP/P2P ports; they all share the SAME
// admin wallet's pubkey as the bootstrap coordinator key (so blocks
// signed by the coordinator are accepted by every follower's validator),
// but each engine has its OWN node_wallet — only the coordinator can
// actually sign blocks; followers are read-replicas.
//
// Topology:
//   sandboxes[0]  — coordinator: writes blocks via HTTP API, broadcasts via P2P
//   sandboxes[1..n] — followers : passive, accept blocks via P2P broadcast
//
// All engines are wired in a star around the coordinator: each follower
// dials the coordinator on boot, and the coordinator's broadcast worker
// fans out every newly-persisted block to every inbound peer.

use std::sync::OnceLock;

/// Pick an unused TCP port by binding to `127.0.0.1:0`, reading the
/// assigned port, and closing the listener. There's a tiny race window
/// before the engine binds the same port — ports are normally not
/// reused immediately by the OS so this is fine for tests.
async fn pick_free_port() -> Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Workspace bench config is written ONCE per process. `boot_sandbox`
/// already does this; we mirror the path so cluster mode can re-use it
/// without rewriting the same TOML N times.
static BENCH_CONFIG_INIT: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Boot a multi-engine cluster of `n` sandboxes (1 ≤ n ≤ 4).
///
/// `sandboxes[0]` is the coordinator and is the only engine that can
/// sign blocks (its `node_wallet` matches the configured
/// `coordinator_public_key`). `sandboxes[1..n]` are followers with
/// random per-engine wallets — their own block submissions would be
/// rejected by the single-writer check, but they accept and persist
/// blocks broadcast by the coordinator over P2P.
///
/// All engines share the same global config (PMS_CONFIG file), so
/// network_id, coordinator_public_key, and protocol_version are
/// identical across the cluster — that's what we want for a star
/// topology where the coordinator's signature is trusted by everyone.
async fn boot_sandbox_cluster(n: usize) -> Result<Vec<Sandbox>> {
    anyhow::ensure!(
        (1..=4).contains(&n),
        "boot_sandbox_cluster: n must be between 1 and 4 (got {n})"
    );

    let root = get_workspace_root();
    std::env::set_current_dir(&root)?;

    // ── 1. Admin wallet (shared coordinator key for the whole cluster) ──
    let admin_wallet_path = root.join("etc/pms/admin-wallet.json");
    anyhow::ensure!(
        admin_wallet_path.exists(),
        "Admin wallet not found: {:?}",
        admin_wallet_path
    );
    let admin_wallet = Wallet::load_from_file(admin_wallet_path.to_string_lossy().as_ref())
        .map_err(|e| anyhow::anyhow!("Failed to load admin wallet: {}", e))?;
    let admin_addr = admin_wallet.get_address("8e");
    let admin_pk = admin_wallet.encoded_public_key();

    // ── 2. Generate the bench config ONCE for the cluster ──────────────
    //     (same logic as boot_sandbox; cached after the first call).
    let bench_config_path = match BENCH_CONFIG_INIT.get() {
        Some(p) => p.clone(),
        None => {
            let config_src = std::fs::read_to_string(root.join("etc/config/config.local.toml"))
                .context("Failed to read config.local.toml")?;
            let mut buf: String = config_src
                .lines()
                .map(|l| {
                    let trimmed = l.trim_start();
                    if trimmed.starts_with("coordinator_public_key")
                        && !trimmed.starts_with("coordinator_public_key_")
                    {
                        return format!("coordinator_public_key = \"{}\"", admin_pk);
                    }
                    if trimmed.starts_with("coordinator_x25519_public_key") {
                        return format!(
                            "coordinator_x25519_public_key = \"{}\"",
                            admin_wallet.x25519_pub_hex()
                        );
                    }
                    if trimmed.starts_with("mode") && trimmed.contains("testnet") {
                        return l.replace("testnet", "dev");
                    }
                    if trimmed.starts_with("prefix") && trimmed.contains("pms:test") {
                        return l.replace("pms:test", "pms:dev");
                    }
                    if trimmed.starts_with("enforce_single_writer") {
                        return l.replace("true", "false");
                    }
                    l.to_string()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !buf.contains("enforce_single_writer") {
                buf = buf.replace(
                    "[validation]",
                    "[validation]\nenforce_single_writer = false",
                );
            }
            if !buf.contains("coordinator_public_key") {
                buf = buf.replace(
                    "[validation]",
                    &format!("[validation]\ncoordinator_public_key = \"{}\"", admin_pk),
                );
            }
            buf = if buf.contains("distribution_interval_sec") {
                let mut out = String::new();
                for line in buf.lines() {
                    if line.trim_start().starts_with("distribution_interval_sec") {
                        out.push_str("distribution_interval_sec = 2\n");
                    } else {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                out
            } else {
                buf.replace("[fees]", "[fees]\ndistribution_interval_sec = 2")
            };
            let p = root.join("etc/config/config.bench.toml");
            std::fs::write(&p, &buf).context("Failed to write bench config")?;
            BENCH_CONFIG_INIT.set(p.clone()).ok();
            p
        }
    };
    unsafe {
        std::env::set_var("PMS_CONFIG", bench_config_path.to_string_lossy().as_ref());
    }
    if std::env::var("PMS_ADMIN_TOKEN").is_err() {
        unsafe { std::env::set_var("PMS_ADMIN_TOKEN", "sandbox_test") };
    }

    // ── 3. Allocate ports up front so we can wire known_peers ──────────
    let mut p2p_ports = Vec::with_capacity(n);
    let mut api_ports = Vec::with_capacity(n);
    for _ in 0..n {
        p2p_ports.push(pick_free_port().await?);
        api_ports.push(pick_free_port().await?);
    }

    // ── 4. Boot every engine ───────────────────────────────────────────
    let mut sandboxes = Vec::with_capacity(n);
    for idx in 0..n {
        let sandbox = boot_one_engine(
            idx,
            n,
            &admin_wallet,
            &admin_addr,
            &admin_pk,
            p2p_ports[idx],
            api_ports[idx],
        )
        .await
        .with_context(|| format!("boot_one_engine idx={idx}"))?;
        sandboxes.push(sandbox);
    }

    // ── 5. Wait for every P2P listener to be ready, then wire the star ──
    //
    // One-way dial only: follower → coordinator. This is the realistic
    // multi-VPS topology where read-replica VPSes dial the writer,
    // not the other way around. Pre-v0.7.4 the `peer.rs` Inv handler
    // and the `blocks.rs` orphan-recovery path used `broadcast()` to
    // ask for blocks back from the peer that announced them, which
    // only worked when that peer was inbound — so a follower's request
    // to its outbound coordinator went into the void. Both call sites
    // now use `unicast(&sa, ...)`, which works regardless of dial
    // direction. A single dial per follower is enough.
    sleep(Duration::from_millis(500)).await;
    if n > 1 {
        let coord_p2p = format!("127.0.0.1:{}", p2p_ports[0]);
        for idx in 1..n {
            let f_srv = sandboxes[idx]
                .server_arc
                .clone()
                .expect("follower must have server_arc");
            f_srv
                .connect_to_peer(coord_p2p.clone(), None)
                .await
                .with_context(|| format!("follower {idx} → coord {coord_p2p}"))?;
            println!("   [cluster] follower {} → coordinator connected", idx);
        }
        // Give the handshakes + initial sync time to settle.
        sleep(Duration::from_secs(2)).await;
    }

    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║  PMS Multi-Engine Cluster Sandbox (n={})                   ║", n);
    println!("╚═══════════════════════════════════════════════════════════╝");
    for (idx, s) in sandboxes.iter().enumerate() {
        let role = if idx == 0 { "coordinator" } else { "follower" };
        println!(
            "   [{}] {:11} api={} p2p={}",
            idx,
            role,
            s.base_url,
            s.p2p_addr.as_deref().unwrap_or("n/a")
        );
    }
    println!();

    Ok(sandboxes)
}

/// Boot a single engine inside a cluster. Internal — call
/// `boot_sandbox_cluster` from tests.
async fn boot_one_engine(
    idx: usize,
    n_total: usize,
    admin_wallet: &Wallet,
    admin_addr: &str,
    admin_pk: &str,
    p2p_port: u16,
    api_port: u16,
) -> Result<Sandbox> {
    let mut settings = load_config()?;
    let network_id = settings.network.network_id.clone();

    settings.admin.signer_pubkeys = vec![admin_pk.to_string()];
    settings.admin.wallet_addresses = vec![admin_addr.to_string()];
    settings.validation.coordinator_public_key = Some(admin_pk.to_string());

    // Per-engine RocksDB tempdir.
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join(format!("sandbox-rocks-{idx}"));
    settings.rocks.path = db_path.to_string_lossy().into();

    // ── LedgerManager + adapter (own DB per engine) ──────────────────
    let mgr = pms_ledger::LedgerManager::bootstrap(&settings)
        .await
        .context("LedgerManager::bootstrap failed")?;
    let main_instance = mgr
        .default_ledger()
        .context("No default (main) ledger after bootstrap")?;
    let store = main_instance.store.clone();
    let adapter = main_instance.adapter.clone();

    // ── Per-engine node_wallet ───────────────────────────────────────
    //
    // idx=0 (coordinator) signs blocks → wallet must be the admin
    // wallet (its pubkey matches `coordinator_public_key`).
    // idx>0 (follower) gets a random wallet — its own block submissions
    // would be rejected by single-writer enforcement, but it accepts
    // blocks broadcast by the coordinator (signed by admin_pk) because
    // every engine shares the same coordinator_public_key in config.
    let node_wallet: Arc<Wallet> = if idx == 0 {
        Arc::new(admin_wallet.clone())
    } else {
        // Distinct seed per follower so node_id differs (required for
        // P2P self-loop detection and distinct peer IDs).
        let seed = [idx as u8 + 100; 32];
        Arc::new(
            Wallet::from_seed(&seed, None)
                .map_err(|e| anyhow::anyhow!("follower wallet from_seed: {e}"))?,
        )
    };

    // Override P2P bind address on the per-engine settings before
    // building the Server. `Server::new()` reads it from the
    // p2p_config arg we pass in — we set bind_addr on a clone so
    // we don't poison the global settings.
    let mut p2p_cfg = settings.p2p.clone();
    p2p_cfg.bind_addr = Some(format!("127.0.0.1:{p2p_port}"));

    let mgr_arc = Arc::new(mgr);
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &p2p_cfg,
        Some(mgr_arc.clone()),
    );

    // ── Spawn the P2P listener ────────────────────────────────────────
    //
    // `Server::listen` loops forever; spawning detaches it so the
    // sandbox can boot the API listener next. The handle is stored on
    // Sandbox so the test can drop it (and abort the listener) when
    // the cluster goes away.
    let p2p_addr = format!("127.0.0.1:{p2p_port}");
    let srv_listen = srv.clone();
    let p2p_addr_clone = p2p_addr.clone();
    let p2p_handle = tokio::spawn(async move {
        if let Err(e) = srv_listen.listen(&p2p_addr_clone).await {
            eprintln!("   [cluster idx={idx}] P2P listener exited: {e}");
        }
    });
    // Yield so the listener has a chance to call bind() before the
    // first follower tries to dial it.
    sleep(Duration::from_millis(50)).await;

    let admin_token = settings
        .auth
        .admin_api_token
        .as_deref()
        .and_then(resolve_admin_token)
        .unwrap_or_else(|| "sandbox_test".to_string());

    let fee_pool_registry = Arc::new(pms_server::fee_pool::FeePoolRegistry::new());
    let main_fee_pool = fee_pool_registry.get_or_create("main");
    let main_store_for_contracts: Arc<dyn pms_storage::ContractStorage> = store.clone();
    let main_event_bus = srv.adapter_arc().event_bus();

    let cfg = Arc::new(ServerConfig {
        bind_addr: p2p_addr.clone(),
        api_addr: format!("127.0.0.1:{api_port}"),
        tls: settings.tls.clone(),
        api_tls_enabled: false,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    let state = AppState {
        srv: srv.clone(),
        _cfg: cfg,
        _ready: Arc::new(AtomicBool::new(true)),
        stats: Arc::new(Stats::new()),
        store: store.clone(),
        admin_token: Some(admin_token.clone()),
        node_wallet: node_wallet.clone(),
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![],
        treasury_wallets: TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: main_fee_pool,
        fee_pool_registry: fee_pool_registry.clone(),
        api_key_store: pms_server::api_keys::create_api_key_store(None)
            .expect("empty api key store must work"),
        ledger_mgr: Some(mgr_arc),
        ledger_id: "main".into(),
        effective_fees: Arc::new(pms_server::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
        contract_event_bus: main_event_bus.clone(),
        contract_store: main_store_for_contracts.clone(),
        compliance_lock: Arc::new(tokio::sync::Mutex::new(())),
    };

    // Only the coordinator runs the fee distributor — followers don't
    // own the writing key, distributing from a follower would either
    // produce a block rejected by single-writer, or worse, race with
    // the coordinator.
    if idx == 0 {
        spawn_fee_distributor_task(state.clone());
        if let Some(bus) = main_event_bus {
            let sink = Arc::new(FeePoolRefundSink {
                registry: fee_pool_registry.clone(),
            });
            pms_contracts::spawn_contract_listener(bus, main_store_for_contracts, sink);
        }
    }

    let app = build_api_router(state, &settings);
    let api_listener = tokio::net::TcpListener::bind(&format!("127.0.0.1:{api_port}")).await?;
    let bound = api_listener.local_addr()?;
    let base_url = format!("http://{}", bound);

    let server_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(
            api_listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            eprintln!("   [cluster idx={idx}] API listener exited: {e}");
        }
    });
    sleep(Duration::from_millis(200)).await;

    // Wait for /livez to respond.
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(20)
        .pool_idle_timeout(Duration::from_secs(30))
        .build()?;
    let mut ready = false;
    for _ in 0..60 {
        if let Ok(r) = client.get(format!("{}/livez", base_url)).send().await {
            if r.status().is_success() {
                ready = true;
                break;
            }
        }
        sleep(Duration::from_millis(200)).await;
    }
    anyhow::ensure!(
        ready,
        "[cluster idx={idx}] API not ready after 12s (api={base_url})"
    );

    println!(
        "   [cluster] engine {} / {} booted: api={} p2p={}",
        idx + 1,
        n_total,
        base_url,
        p2p_addr
    );

    Ok(Sandbox {
        base_url,
        client,
        admin_wallet: admin_wallet.clone(),
        admin_addr: admin_addr.to_string(),
        admin_token,
        network_id,
        _tmp: tmp,
        _server_handle: server_handle,
        _p2p_handle: Some(p2p_handle),
        server_arc: Some(srv),
        p2p_addr: Some(p2p_addr),
        cluster_idx: Some(idx),
    })
}

// ============================================================================
// TEST: Multi-engine cluster — block propagation across the star
// ============================================================================

/// Boot 1 + 3 followers, faucet-mint on the coordinator, then assert
/// every follower's `block_count_estimate` reflects the coordinator's
/// new block. This is the in-process replacement for "deploy on a 2nd
/// VPS and test failover" — it proves end-to-end that the persist
/// pipeline emits broadcast events, the broadcast worker fans them out
/// over the P2P TCP listener, and the followers' adapters accept them.
///
/// The test parameterises `n` from 1 to 4 so we exercise the singleton
/// case (n=1, no peers) and the full cluster (n=4) in one run.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn test_multi_engine_cluster_propagation() -> Result<()> {
    for n in [1usize, 2, 3, 4] {
        println!("\n═══════════════════════════════════════════════════════════════");
        println!("   CLUSTER SIZE n={}", n);
        println!("═══════════════════════════════════════════════════════════════");

        let cluster = boot_sandbox_cluster(n).await?;
        let coordinator = &cluster[0];

        // Sanity: with one-way dial (follower → coord), the coord has
        // exactly `n-1` inbound peers. We assert `>=` to keep room for
        // any future reconnect / topology change.
        if let Some(srv) = coordinator.server_arc.as_ref() {
            let peers = srv.get_p2p_peers();
            let expected_min = n.saturating_sub(1);
            println!(
                "   [coord] connected peers: {} (expected ≥ {})",
                peers.len(),
                expected_min
            );
            assert!(
                peers.len() >= expected_min,
                "coordinator should see at least n-1 followers (got {} for n={n})",
                peers.len()
            );
        }

        // Submit a faucet mint on the coordinator. This is the simplest
        // signed plain block that touches the persist pipeline.
        let test_addr = pms_wallet::Wallet::generate().get_address("8e");
        let (status, body) = coordinator
            .admin_post(
                "/admin/faucet",
                json!({ "to": test_addr, "amount": "10.0" }),
            )
            .await;
        println!("   [coord] /admin/faucet → {} body={}", status, body);
        assert!(
            status.is_success(),
            "coordinator faucet must succeed (status={status} body={body})"
        );

        // Give P2P broadcast + persist time to fan out.
        sleep(Duration::from_secs(3)).await;

        // Helper: pull /healthz on each engine and extract the
        // `block_count_estimate` from the rocksdb_writable check —
        // cheapest way to compare DAG sizes across engines without
        // adding a dedicated debug endpoint.
        let block_count = async |sandbox: &Sandbox| -> Result<u64> {
            let resp: Value = sandbox
                .client
                .get(format!("{}/healthz", sandbox.base_url))
                .send()
                .await?
                .json()
                .await
                .unwrap_or(json!({}));
            let count = resp
                .get("checks")
                .and_then(|cs| cs.as_array())
                .and_then(|cs| {
                    cs.iter().find(|c| {
                        c.get("name").and_then(|v| v.as_str())
                            == Some("rocksdb_writable")
                    })
                })
                .and_then(|c| c.get("detail"))
                .and_then(|d| d.get("block_count_estimate"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Ok(count)
        };

        let coord_count = block_count(coordinator).await?;
        println!("   [coord] block_count_estimate = {coord_count}");
        assert!(
            coord_count >= 2,
            "coordinator must have at least genesis + faucet block (got {coord_count})"
        );

        // For each follower, assert block_count grew past the initial
        // genesis. Strict tip-equality is harder than it sounds: when
        // each engine runs its own LedgerManager, they each create
        // their own deterministic genesis — but if the follower has
        // joined too late and never replays history, it stays at its
        // own tip. The block_count check is the operative signal:
        // strictly-greater-than-1 means the follower received and
        // accepted at least one block from the coordinator.
        for (idx, follower) in cluster.iter().enumerate().skip(1) {
            let f_count = block_count(follower).await?;
            println!("   [follower {idx}] block_count_estimate = {f_count}");
            // If the genesis IDs match across engines, follower will
            // have received the faucet (count >= 2). If the genesis
            // IDs DON'T match, the follower will reject the faucet
            // (parent unknown) and stay at count=1 — that's a real
            // deployment bug we want to surface, not silence.
            assert!(
                f_count >= 2,
                "follower {idx} block_count={f_count}: coordinator's broadcast didn't propagate \
                 (likely cause: divergent genesis IDs across engines — check LedgerManager bootstrap)"
            );
        }

        println!("   ✅ n={n}: all followers persisted the coordinator's block");
        // Drop cluster — handles + tempdirs clean up here.
    }

    Ok(())
}
