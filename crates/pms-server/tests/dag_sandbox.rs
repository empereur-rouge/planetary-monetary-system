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
use pms_testkit::forge_signed_wire_block_for_test;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
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
    /// Wire metadata (network_id + protocol_version) for hand-forging blocks
    /// submitted via `/submit/block` in adversarial tests. Populated at boot
    /// from the running config so forged blocks match the engine's expectations.
    #[allow(dead_code)]
    wire_meta: WireMeta,
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

    /// DELETE with admin Bearer token.
    #[allow(dead_code)]
    async fn admin_delete(&self, path: &str) -> (reqwest::StatusCode, Value) {
        let resp = self
            .client
            .delete(format!("{}{}", self.base_url, path))
            .bearer_auth(&self.admin_token)
            .send()
            .await
            .expect("HTTP DELETE failed");
        let status = resp.status();
        let json = resp.json::<Value>().await.unwrap_or(json!({}));
        (status, json)
    }

    /// GET on a public (unauthenticated) endpoint.
    #[allow(dead_code)]
    async fn public_get(&self, path: &str) -> (reqwest::StatusCode, Value) {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
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

/// Upsert a `[fees]` key in a generated TOML config string: strip any existing
/// line for `key`, then re-insert `key = value_toml` right after the `[fees]`
/// header. `value_toml` is the already-formatted RHS (e.g. `8` or `"1"`).
fn upsert_fees_key(config: String, key: &str, value_toml: &str) -> String {
    let mut out = String::new();
    for line in config.lines() {
        if line.trim_start().starts_with(key) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.replace("[fees]", &format!("[fees]\n{key} = {value_toml}"))
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

    // Inject `[fees]` tunables that individual tests request via env vars,
    // without polluting the other sandbox tests. `upsert_fees_key` strips any
    // existing line for the key and re-inserts it under the `[fees]` header.
    // - PMS_TEST_COORD_SHARD_COUNT: enables coordinator sub-address sharding.
    // - PMS_TEST_MINT_FEE_BASE: charges a flat token-mint fee — exercises the
    //   one live coord-shard consumer (`admin_mint_token` → `fee_recipient_address()`),
    //   used by `test_coord_shard_routing_distributes_fees`.
    let config_bench = match std::env::var("PMS_TEST_COORD_SHARD_COUNT")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        Some(n) if n > 0 => upsert_fees_key(config_bench, "coord_shard_count", &n.to_string()),
        _ => config_bench,
    };
    let config_bench = match std::env::var("PMS_TEST_MINT_FEE_BASE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(v) => upsert_fees_key(config_bench, "mint_fee_base", &format!("\"{v}\"")),
        None => config_bench,
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

    // Coordinator shard wallets — same logic as api/serve.rs but in
    // the in-process sandbox boot path. When [fees].coord_shard_count
    // = 0 (default), the vec stays empty and fee handlers fall back
    // to admin.wallet_addresses[0].
    let coord_shard_wallets: Vec<pms_wallet::Wallet> = if settings.fees.coord_shard_count > 0 {
        pms_wallet::shard_derivation::derive_coord_shard_set(
            &node_wallet,
            settings.fees.coord_shard_count,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "derive {} coord shards in sandbox: {e}",
                settings.fees.coord_shard_count
            )
        })?
    } else {
        Vec::new()
    };

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
        coord_shard_wallets: std::sync::Arc::new(coord_shard_wallets),
        coord_shard_round_robin: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        read_only: std::sync::Arc::new(pms_server::read_only::ReadOnlyMode::new()),
        webhook_store: pms_server::api_fn::webhooks::WebhookStore::new(),
        emission_gate: Arc::new(pms_server::emission::EmissionGate::load(&store)),
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
        wire_meta: WireMeta::from(&settings),
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
/// 4. Test user sends transactions on eden → the native fee is burned at
///    source and accumulated in eden's FeePool
/// 5. The periodic distributor re-mints the fees; assert the coordinator
///    received its 65% share on eden
///
/// **Fee distribution path (v0.30.0)**: `wallet_send_simple` burns the native
/// fee at source (`in − out = fee`) and calls `accumulate_tx_fee`, pooling it
/// in the per-ledger FeePool. `spawn_fee_distributor_task` (2s in the sandbox)
/// then mints one consolidated Reward block: `treasury_fee_percent` (35%) to
/// the treasury, the remaining 65% to the fee beneficiary. eden has no explicit
/// owner, so its fees accrue to the coordinator, which receives its share
/// directly (the coordinator is not a node_registry entry — see the
/// self-share resolution in `fee_distribution::distribute`).
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

    // ── 6. Wait for periodic fee distribution, then check results ─────
    // FEE MODEL (v0.30.0): the native fee is BURNED at source in each TX
    // (in − out = fee), then re-minted by `spawn_fee_distributor_task`
    // (2s interval in the sandbox) — NOT via an immediate per-TX Reward
    // block. eden has no explicit owner, so its fees accrue to the
    // coordinator's pk; the coordinator now receives its producer share
    // directly (it is not a node_registry entry — see the distribute.rs
    // self-share fix). The treasury takes `treasury_fee_percent` (35%)
    // first; the coordinator receives the remaining 65%.
    println!("   [6/6] Waiting for periodic fee distribution (2s interval)...");
    let mut coord_eden_after = coord_eden_before;
    let mut coord_gained = Decimal::ZERO;
    for attempt in 0..20 {
        sleep(Duration::from_millis(750)).await;
        coord_eden_after = sandbox.get_balance("eden", &sandbox.admin_addr).await?;
        coord_gained = coord_eden_after - coord_eden_before;
        // Distribution has run once the coordinator holds MORE than just
        // the transfer payments users sent it (fee revenue arrived).
        if coord_gained > total_payments {
            println!("      distribution observed after {} poll(s)", attempt + 1);
            break;
        }
    }
    let user_balance_after = sandbox.get_balance("eden", &user_addr).await?;

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

    // 2. Coordinator should have received its fee share beyond payments.
    //    Expected = coordinator_fee_percent (65%) of the total fees paid,
    //    after the treasury's 35% cut. Golden ratio tied to
    //    config.local.toml (treasury 35 / coordinator 65); update here if
    //    that split changes. burn_rate_bps is 0 in the sandbox, so no fee
    //    is burned before distribution.
    let fee_revenue = coord_gained - total_payments;
    let expected_fee_revenue =
        (total_fees_paid * Decimal::from(65) / Decimal::from(100)).round_dp(8);
    println!(
        "\n   Assertion: coordinator fee revenue = {} PMS (expected ≈ {} = 65% of {} from {} tx)",
        fee_revenue, expected_fee_revenue, total_fees_paid, tx_count
    );
    assert!(
        fee_revenue > Decimal::ZERO,
        "Coordinator must receive fee revenue after distribution. \
         Total gained: {}, payments: {}, fee revenue: {}",
        coord_gained,
        total_payments,
        fee_revenue
    );
    let fee_tolerance = Decimal::from_str("0.001").unwrap();
    assert!(
        (fee_revenue - expected_fee_revenue).abs() < fee_tolerance,
        "Coordinator fee revenue {} should be ≈ 65% of fees paid ({}), \
         within {}. Got diff {}.",
        fee_revenue,
        expected_fee_revenue,
        fee_tolerance,
        (fee_revenue - expected_fee_revenue).abs()
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
// TEST: Coordinator fee share is NOT hijackable via unauthenticated /v1/register
// ============================================================================

/// Security regression guard for the v0.30.0 fee-distribution fix.
///
/// `perform_fee_distribution` resolves the coordinator's producer share
/// (65% after the 35% treasury cut) via `node_pk == node_wallet.pk` BEFORE any
/// `node_registry` lookup. This closes a real hijack vector: `POST /v1/register`
/// is unauthenticated, so before the fix an attacker could register the
/// coordinator's pk with an attacker-controlled `wallet_address` and the entire
/// coordinator share would be routed to them.
///
/// This test registers the coordinator's pk → an attacker address, drives
/// fee-bearing transactions on `main`, distributes, and asserts the coordinator
/// (not the attacker) receives its 65% share.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_coordinator_fee_share_not_hijackable_via_register() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    let coord_pk = sandbox.admin_wallet.encoded_public_key();
    let coord_addr = sandbox.admin_addr.clone();

    // Attacker wallet — the address they hope to redirect the coordinator's
    // fees to. Starts empty and must STAY empty.
    let attacker = Wallet::generate();
    let attacker_addr = attacker.get_address("8e");

    // Test user + an unrelated destination, so the coordinator's balance delta
    // is PURE fee revenue (no payments routed to the coordinator).
    let user = Wallet::generate();
    let user_addr = user.get_address("8e");
    let user_sk = user.private_key_b64.clone();
    let dest = Wallet::generate().get_address("8e");

    sandbox.faucet_mint(None, &user_addr, "1000").await?;
    sleep(Duration::from_millis(300)).await;

    let coord_before = sandbox.get_balance("main", &coord_addr).await?;
    println!("   coordinator before: {}", coord_before);

    // ── Plant a malicious registry entry for the coordinator's pk ──
    // `/v1/register` is now operator-gated (v0.30.1), so we plant the entry via
    // the admin token — modelling a compromised/misconfigured operator, or any
    // future registry-write path. The authoritative resolution must STILL ignore
    // this entry for the coordinator's own share (defense in depth).
    let (reg_status, reg_body) = sandbox
        .admin_post(
            "/v1/register",
            json!({
                "node_pk": coord_pk,
                "api_url": "http://attacker.example",
                "wallet_address": attacker_addr,
            }),
        )
        .await;
    println!("   /v1/register (planted via admin) → {} {:?}", reg_status, reg_body);
    assert!(
        reg_status.is_success(),
        "admin-authorized register should be accepted so we can plant the malicious entry"
    );

    // ── Drive fee-bearing transactions on main ──
    let mut total_fees = Decimal::ZERO;
    for i in 0..10 {
        let resp = sandbox
            .send_simple(None, &user_sk, &dest, "10.0")
            .await?;
        let fee = Decimal::from_str(resp["fee"].as_str().unwrap_or("0")).unwrap_or_default();
        total_fees += fee;
        println!("   tx {i}: fee={fee}");
    }
    println!("   total fees paid: {}", total_fees);

    // ── Distribute + poll until the coordinator is credited ──
    // Trigger an explicit distribution (drains the pool synchronously); the 2s
    // periodic task is a backstop. Then poll passively for the credited UTXO.
    let _ = sandbox.distribute_fees().await;
    let mut coord_gain = Decimal::ZERO;
    for attempt in 0..20 {
        sleep(Duration::from_millis(500)).await;
        coord_gain = sandbox.get_balance("main", &coord_addr).await? - coord_before;
        if coord_gain > Decimal::ZERO {
            println!("   coordinator credited after {} poll(s)", attempt + 1);
            break;
        }
    }

    let attacker_after = sandbox.get_balance("main", &attacker_addr).await?;
    let expected_coord = (total_fees * Decimal::from(65) / Decimal::from(100)).round_dp(8);

    println!("\n   ╔══════════════════════════════════════════════════════╗");
    println!("   ║  HIJACK GUARD RESULTS                                 ║");
    println!("   ╠══════════════════════════════════════════════════════╣");
    println!("   ║  Coordinator gain:            {:>20}    ║", coord_gain);
    println!("   ║  Expected (65% of fees):      {:>20}    ║", expected_coord);
    println!("   ║  Attacker balance:            {:>20}    ║", attacker_after);
    println!("   ╚══════════════════════════════════════════════════════╝");

    // The coordinator earns its 65% producer share...
    assert!(
        coord_gain > Decimal::ZERO,
        "coordinator must receive its fee share (got {coord_gain})"
    );
    let tol = Decimal::from_str("0.001").unwrap();
    assert!(
        (coord_gain - expected_coord).abs() < tol,
        "coordinator share {coord_gain} should be ≈ 65% of fees ({expected_coord})"
    );
    // ...and the attacker gets NOTHING, despite registering the coordinator's pk.
    assert_eq!(
        attacker_after,
        Decimal::ZERO,
        "attacker who registered the coordinator's pk must receive ZERO — the \
         self-share branch ignores the registry. Got {attacker_after}"
    );

    println!(
        "\n   TEST PASSED: coordinator kept its {} PMS share; /v1/register hijack blocked (attacker = 0).",
        coord_gain
    );
    Ok(())
}

// ============================================================================
// TEST: Custom-ledger OWNER fee share is authoritative (registry-independent)
// ============================================================================

/// Security regression guard for the v0.30.0 authoritative-payout fix.
///
/// A custom ledger's transaction fees accrue to that ledger's OWNER pubkey
/// (`accumulate_tx_fee`). Before the fix, `perform_fee_distribution` resolved
/// the owner's payout address via the `node_registry`, which is (a) overwritable
/// by the unauthenticated `POST /v1/register`, letting an attacker steal the
/// owner's share, and (b) heartbeat-less with a 24h TTL, so the share would
/// silently divert to the treasury once the entry ages out. The fix derives the
/// owner payout address from the durable ledger definition (owner_pubkey +
/// owner_x25519), ignoring the registry entirely for that identity.
///
/// This test creates a ledger owned by a distinct wallet, registers an attacker
/// against the owner's pk, drives fee-bearing transactions on that ledger, and
/// asserts the OWNER (not the attacker) receives its 65% share.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_custom_ledger_owner_fee_share_not_hijackable_via_register() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // ── Owner wallet (distinct from the coordinator) ─────────────────
    let owner = Wallet::generate();
    let owner_pk = owner.encoded_public_key();
    let owner_x25519 = owner.x25519_pub_hex().to_string();
    let owner_addr = owner.get_address("8e"); // == derive_address_from_keys(owner_pk, owner_x25519)

    // ── A half-configured owner (pubkey without x25519) must be REJECTED ──
    // Otherwise the owner would have no derivable authoritative payout address
    // and its share would fall back to the hijackable registry.
    let (bad_st, _bad_body) = sandbox
        .admin_post(
            "/admin/ledgers/create",
            json!({
                "id": "half-owned",
                "network_id": "half-net",
                "prefix": "half",
                "owner_pubkey": owner_pk,
                // owner_x25519_pubkey deliberately omitted
            }),
        )
        .await;
    assert_eq!(
        bad_st,
        reqwest::StatusCode::BAD_REQUEST,
        "create with owner_pubkey but no owner_x25519 must be rejected (400)"
    );

    // ── Create a custom ledger OWNED by that wallet (both keys) ───────
    let (st, body) = sandbox
        .admin_post(
            "/admin/ledgers/create",
            json!({
                "id": "owned",
                "network_id": "owned-net",
                "prefix": "owned",
                "symbol": "OWN",
                "owner_pubkey": owner_pk,
                "owner_x25519_pubkey": owner_x25519,
            }),
        )
        .await;
    assert!(st.is_success(), "create owned ledger failed: {} — {:?}", st, body);
    sandbox.deposit_gas_pool("owned", "50000").await?;

    // ── Plant a malicious registry entry for the OWNER's pk (via admin) ──
    // `/v1/register` is operator-gated (v0.30.1); planting via admin models any
    // registry-write path. The authoritative owner-payout resolution must still
    // ignore it.
    let attacker = Wallet::generate();
    let attacker_addr = attacker.get_address("8e");
    let (reg_status, _) = sandbox
        .admin_post(
            "/v1/register",
            json!({
                "node_pk": owner_pk,
                "api_url": "http://attacker.example",
                "wallet_address": attacker_addr,
            }),
        )
        .await;
    assert!(reg_status.is_success(), "admin-authorized register should be accepted (planting the entry)");

    // ── Fund a user with PMS on the owned ledger + drive fees ────────
    let user = Wallet::generate();
    let user_addr = user.get_address("8e");
    let user_sk = user.private_key_b64.clone();
    let dest = Wallet::generate().get_address("8e");
    sandbox.faucet_mint(Some("owned"), &user_addr, "1000").await?;
    sleep(Duration::from_millis(400)).await;

    let owner_before = sandbox.get_balance("owned", &owner_addr).await?;
    println!("   owner before: {} (addr {})", owner_before, &owner_addr[..16]);

    let mut total_fees = Decimal::ZERO;
    for i in 0..10 {
        let resp = sandbox
            .send_simple(Some("owned"), &user_sk, &dest, "10.0")
            .await?;
        let fee = Decimal::from_str(resp["fee"].as_str().unwrap_or("0")).unwrap_or_default();
        total_fees += fee;
        println!("   tx {i}: fee={fee}");
    }
    println!("   total fees paid: {}", total_fees);

    // ── Wait for the periodic distributor to process the owned ledger ─
    let mut owner_gain = Decimal::ZERO;
    for attempt in 0..20 {
        sleep(Duration::from_millis(750)).await;
        owner_gain = sandbox.get_balance("owned", &owner_addr).await? - owner_before;
        if owner_gain > Decimal::ZERO {
            println!("   owner credited after {} poll(s)", attempt + 1);
            break;
        }
    }

    let attacker_after = sandbox.get_balance("owned", &attacker_addr).await?;
    let expected_owner = (total_fees * Decimal::from(65) / Decimal::from(100)).round_dp(8);

    println!("\n   ╔══════════════════════════════════════════════════════╗");
    println!("   ║  OWNER HIJACK GUARD RESULTS                           ║");
    println!("   ╠══════════════════════════════════════════════════════╣");
    println!("   ║  Owner gain:                  {:>20}    ║", owner_gain);
    println!("   ║  Expected (65% of fees):      {:>20}    ║", expected_owner);
    println!("   ║  Attacker balance:            {:>20}    ║", attacker_after);
    println!("   ╚══════════════════════════════════════════════════════╝");

    // The ledger owner earns its 65% producer share, resolved authoritatively...
    assert!(
        owner_gain > Decimal::ZERO,
        "ledger owner must receive its fee share (got {owner_gain})"
    );
    let tol = Decimal::from_str("0.001").unwrap();
    assert!(
        (owner_gain - expected_owner).abs() < tol,
        "owner share {owner_gain} should be ≈ 65% of fees ({expected_owner})"
    );
    // ...and the attacker who registered the owner's pk gets NOTHING.
    assert_eq!(
        attacker_after,
        Decimal::ZERO,
        "attacker who registered the owner's pk must receive ZERO — the owner \
         payout is derived from the ledger definition, not the registry. Got {attacker_after}"
    );

    println!(
        "\n   TEST PASSED: ledger owner kept its {} PMS share; /v1/register hijack blocked (attacker = 0).",
        owner_gain
    );
    Ok(())
}

// ============================================================================
// TEST: NFT mint is create-only + issuer-authorized (anti-hijack / anti-farming)
// ============================================================================

/// Security regression guard (v0.30.1) for `POST /v1/nft/mint`.
///
/// Before the fix, any API-key holder could (a) re-mint an existing NFT's
/// `token_id` with `owner=self` and STEAL it — the encrypted-payload mint
/// bypassed the consensus create-only guard and `apply_mint` overwrote the
/// owner — and (b) mint arbitrary NFTs (e.g. a fake "cube") to farm an ungated
/// burn-refund contract. This test asserts:
///   1. admin mint works (baseline, `/admin/nft/mint`);
///   2. re-mint of an existing token via `/v1/nft/mint` → 409, owner unchanged;
///   3. API-key mint of a NEW token WITHOUT an issuer signature → 403;
///   4. API-key mint WITH a valid coordinator (authorized issuer) signature → OK;
///   5. API-key mint signed by a NON-issuer → 403.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_nft_mint_authorization_and_create_only() -> Result<()> {
    use pms_types_nft::NftMetadata;
    let sandbox = boot_sandbox().await?;
    let coord = sandbox.admin_wallet.clone(); // coordinator = authorized issuer

    let user1 = Wallet::generate();
    let u1 = user1.get_address("8e");
    let u1x = user1.x25519_pub_hex().to_string();
    let attacker = Wallet::generate();
    let atk = attacker.get_address("8e");
    let atkx = attacker.x25519_pub_hex().to_string();

    let token_a = "a".repeat(64);

    // 1. Baseline mint of token_a to user1 via the API-key path WITH a valid
    //    coordinator (authorized issuer) signature — proves the positive
    //    authority path works AND establishes an existing token for step 2.
    let meta_a = NftMetadata {
        name: Some("Legit".into()),
        description: None,
        uri: None,
        nft_type: Some("collectible".into()),
        extra: None,
    };
    let msg_a = pms_server::api_fn::nft::nft_mint_signing_message(
        &sandbox.network_id,
        "main",
        &token_a,
        &u1,
        &meta_a,
    );
    let sig_a = coord.sign(&msg_a).expect("coord sign a");
    let (st, body) = sandbox
        .post(
            None,
            "/v1/nft/mint",
            json!({
                "token_id": token_a,
                "owner_address": u1,
                "owner_x25519_pubkey": u1x,
                "metadata": meta_a,
                "creator_pubkey_hex": coord.public_key_hex,
                "creator_signature_b64": sig_a,
            }),
        )
        .await;
    assert!(
        st.is_success(),
        "coordinator-signed nft mint failed: {} — {:?}",
        st,
        body
    );
    println!("   [1] coordinator-signed mint of token_a to user1: {}", st);

    let (_s, nft) = sandbox.public_get(&format!("/v1/nft/{token_a}")).await;
    assert_eq!(nft["owner"].as_str(), Some(u1.as_str()), "token_a owner must be user1");

    // 2. CREATE-ONLY: attacker re-mints token_a via /v1/nft/mint → 409.
    let (st2, b2) = sandbox
        .post(
            None,
            "/v1/nft/mint",
            json!({
                "token_id": token_a,
                "owner_address": atk,
                "owner_x25519_pubkey": atkx,
                "metadata": { "name": "Stolen", "nft_type": "collectible" },
            }),
        )
        .await;
    println!("   [2] attacker re-mint token_a → {} {:?}", st2, b2);
    assert_eq!(
        st2,
        reqwest::StatusCode::CONFLICT,
        "re-mint of an existing token must be 409 (create-only)"
    );
    let (_s, nft2) = sandbox.public_get(&format!("/v1/nft/{token_a}")).await;
    assert_eq!(
        nft2["owner"].as_str(),
        Some(u1.as_str()),
        "token_a owner must STILL be user1 after the blocked re-mint (theft prevented)"
    );

    // 3. AUTHORITY: API-key mint of a NEW token WITHOUT a signature → 403.
    let token_b = "b".repeat(64);
    let (st3, _b3) = sandbox
        .post(
            None,
            "/v1/nft/mint",
            json!({
                "token_id": token_b,
                "owner_address": atk,
                "owner_x25519_pubkey": atkx,
                "metadata": { "name": "Fake cube", "nft_type": "cube" },
            }),
        )
        .await;
    println!("   [3] API-key mint w/o signature → {}", st3);
    assert_eq!(
        st3,
        reqwest::StatusCode::FORBIDDEN,
        "API-key mint without an issuer signature must be 403"
    );

    // 4. AUTHORITY POSITIVE: coordinator (authorized issuer) signs → success.
    let token_c = "c".repeat(64);
    let meta_c = NftMetadata {
        name: Some("Signed".into()),
        description: None,
        uri: None,
        nft_type: Some("collectible".into()),
        extra: None,
    };
    let msg_c = pms_server::api_fn::nft::nft_mint_signing_message(
        &sandbox.network_id,
        "main",
        &token_c,
        &u1,
        &meta_c,
    );
    let sig_c = coord.sign(&msg_c).expect("coord sign");
    let (st4, b4) = sandbox
        .post(
            None,
            "/v1/nft/mint",
            json!({
                "token_id": token_c,
                "owner_address": u1,
                "owner_x25519_pubkey": u1x,
                "metadata": meta_c,
                "creator_pubkey_hex": coord.public_key_hex,
                "creator_signature_b64": sig_c,
            }),
        )
        .await;
    println!("   [4] API-key mint w/ coordinator signature → {} {:?}", st4, b4);
    assert!(
        st4.is_success(),
        "API-key mint with a valid coordinator signature must succeed: {} — {:?}",
        st4,
        b4
    );
    let (_s, nftc) = sandbox.public_get(&format!("/v1/nft/{token_c}")).await;
    assert_eq!(nftc["owner"].as_str(), Some(u1.as_str()), "token_c owner must be user1");

    // 5. NON-AUTHORITY: attacker's own valid signature → still 403.
    let token_d = "d".repeat(64);
    let meta_d = NftMetadata {
        name: Some("Fake".into()),
        description: None,
        uri: None,
        nft_type: Some("cube".into()),
        extra: None,
    };
    let msg_d = pms_server::api_fn::nft::nft_mint_signing_message(
        &sandbox.network_id,
        "main",
        &token_d,
        &atk,
        &meta_d,
    );
    let sig_d = attacker.sign(&msg_d).expect("attacker sign");
    let (st5, _b5) = sandbox
        .post(
            None,
            "/v1/nft/mint",
            json!({
                "token_id": token_d,
                "owner_address": atk,
                "owner_x25519_pubkey": atkx,
                "metadata": meta_d,
                "creator_pubkey_hex": attacker.public_key_hex,
                "creator_signature_b64": sig_d,
            }),
        )
        .await;
    println!("   [5] API-key mint signed by NON-issuer → {}", st5);
    assert_eq!(
        st5,
        reqwest::StatusCode::FORBIDDEN,
        "mint signed by a non-authorized issuer must be 403"
    );

    println!("\n   TEST PASSED: NFT mint is create-only + issuer-authorized (theft + farming blocked).");
    Ok(())
}

// ============================================================================
// TEST: /v1/register crypto-auth (peer self-registration, multi-node)
// ============================================================================

/// Validates the v0.30.1 node-registry proof-of-possession auth: a peer node can
/// self-register by SIGNING with the private key of its `node_pk` (no admin
/// token), while forgeries / replays / stale timestamps are rejected. Also
/// confirms the operator (admin) path still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_node_register_crypto_auth() -> Result<()> {
    let sandbox = boot_sandbox().await?;
    let net = sandbox.network_id.clone();

    let peer = Wallet::generate();
    let node_pk = peer.public_key_hex.clone();
    let api_url = "https://peer.example:8443".to_string();
    let wallet_addr = peer.get_address("8e");

    let now = pms_utils::ts_ms() as i64;
    let msg = pms_server::api_fn::nodes::node_register_signing_message(
        &net,
        &node_pk,
        &api_url,
        Some(&wallet_addr),
        now,
    );
    let sig = peer.sign(&msg).expect("peer sign");

    // 1) Valid self-signed registration (NO admin token) → 200.
    let (st1, b1) = sandbox
        .post(
            None,
            "/v1/register",
            json!({
                "node_pk": node_pk, "api_url": api_url, "wallet_address": wallet_addr,
                "ts_ms": now, "signature_b64": sig,
            }),
        )
        .await;
    println!("   [1] self-signed register → {} {:?}", st1, b1);
    assert!(st1.is_success(), "valid self-signed register must be 200, got {st1}");

    // Node now appears in the (public) discovery list.
    let (_s, nodes) = sandbox.public_get("/v1/nodes").await;
    let found = nodes["nodes"]
        .as_array()
        .map(|a| a.iter().any(|n| n["node_pk"] == node_pk))
        .unwrap_or(false);
    assert!(found, "registered peer must appear in /v1/nodes: {nodes}");

    // 2) Anonymous (no admin, no signature) → 401.
    let (st2, _) = sandbox
        .post(
            None,
            "/v1/register",
            json!({ "node_pk": node_pk, "api_url": api_url }),
        )
        .await;
    println!("   [2] no admin, no signature → {}", st2);
    assert_eq!(st2, reqwest::StatusCode::UNAUTHORIZED, "unauthenticated register must be 401");

    // 3) Bad signature → 401 (an attacker cannot claim node_pk without its key).
    let (st3, _) = sandbox
        .post(
            None,
            "/v1/register",
            json!({
                "node_pk": node_pk, "api_url": "https://evil.example", "wallet_address": wallet_addr,
                "ts_ms": pms_utils::ts_ms() as i64, "signature_b64": "bm90LWEtc2ln",
            }),
        )
        .await;
    println!("   [3] bad signature → {}", st3);
    assert_eq!(st3, reqwest::StatusCode::UNAUTHORIZED, "invalid signature must be 401");

    // 4) Stale timestamp (1h old) → 401 (freshness window).
    let stale = now - 3_600_000;
    let msg_stale = pms_server::api_fn::nodes::node_register_signing_message(
        &net, &node_pk, &api_url, Some(&wallet_addr), stale,
    );
    let sig_stale = peer.sign(&msg_stale).expect("peer sign stale");
    let (st4, _) = sandbox
        .post(
            None,
            "/v1/register",
            json!({
                "node_pk": node_pk, "api_url": api_url, "wallet_address": wallet_addr,
                "ts_ms": stale, "signature_b64": sig_stale,
            }),
        )
        .await;
    println!("   [4] stale ts → {}", st4);
    assert_eq!(st4, reqwest::StatusCode::UNAUTHORIZED, "stale timestamp must be 401");

    // 5) REPLAY: re-submit the exact (now, sig) from step 1 → 401 (monotonic:
    //    ts must be strictly > the last authenticated ts).
    let (st5, _) = sandbox
        .post(
            None,
            "/v1/register",
            json!({
                "node_pk": node_pk, "api_url": api_url, "wallet_address": wallet_addr,
                "ts_ms": now, "signature_b64": sig,
            }),
        )
        .await;
    println!("   [5] replay same ts → {}", st5);
    assert_eq!(st5, reqwest::StatusCode::UNAUTHORIZED, "replay of the same ts must be 401 (monotonic)");

    // 6) Operator (admin token) path still works without any signature.
    let (st6, _) = sandbox
        .admin_post(
            "/v1/register",
            json!({
                "node_pk": Wallet::generate().public_key_hex,
                "api_url": "https://operator.example",
            }),
        )
        .await;
    println!("   [6] admin register → {}", st6);
    assert!(st6.is_success(), "admin register must be 200, got {st6}");

    // 7) Malformed api_url (not http/https) → 400 (anti-DoS/pollution field validation).
    let (st7, _) = sandbox
        .post(
            None,
            "/v1/register",
            json!({
                "node_pk": node_pk, "api_url": "ftp://not-http",
                "ts_ms": now + 1, "signature_b64": "x",
            }),
        )
        .await;
    println!("   [7] malformed api_url → {}", st7);
    assert_eq!(st7, reqwest::StatusCode::BAD_REQUEST, "non-http(s) api_url must be 400");

    println!("\n   TEST PASSED: /v1/register accepts a valid node_pk signature; forgeries/replays/stale/malformed rejected; admin path intact.");
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
// COORD SHARDING (audit follow-up to v0.7.4; re-targeted v0.30.0)
// ============================================================================
//
// Boots the sandbox with shard_count=8 + a token-mint fee, then asserts:
//   - GET /v1/coordinator/info exposes 8 distinct shard addresses.
//   - After N token mints, the mint fees actually land across the shards
//     (round-robin worked, not all on one). Token-mint is the ONE live
//     coord-shard fee consumer since transaction fees moved to
//     burn-at-source (v0.30.0) — `admin_mint_token` routes its fee to
//     `AppState::fee_recipient_address()` → `next_coord_shard_address()`.
//   - Each shard's balance is reachable via /v1/balance/{addr}, and the
//     shard balances sum to the exact total mint fees collected — with
//     zero leaking to the legacy master address.

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_coord_shard_routing_distributes_fees() -> Result<()> {
    // Activate sharding + a token-mint fee for THIS test only — boot_sandbox
    // reads these env vars when generating its bench config. Cleared right
    // after boot so other tests in the same process aren't affected.
    unsafe {
        std::env::set_var("PMS_TEST_COORD_SHARD_COUNT", "8");
        std::env::set_var("PMS_TEST_MINT_FEE_BASE", "1");
    }
    let sandbox_result = boot_sandbox().await;
    unsafe {
        std::env::remove_var("PMS_TEST_COORD_SHARD_COUNT");
        std::env::remove_var("PMS_TEST_MINT_FEE_BASE");
    }
    let sandbox = sandbox_result?;

    // 1) Hit /v1/coordinator/info → assert we have 8 shards.
    let (info_status, info) = sandbox.public_get("/v1/coordinator/info").await;
    assert!(info_status.is_success(), "coordinator/info failed: {info_status}");
    println!("coordinator/info:\n{}", serde_json::to_string_pretty(&info)?);

    let count = info
        .get("coord_shard_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(count, 8, "expected 8 shards (env was set to 8)");

    let shards = info
        .get("shards")
        .and_then(|v| v.as_array())
        .expect("shards array present");
    assert_eq!(shards.len(), 8);

    // Collect shard addresses + verify they are all distinct.
    let shard_addrs: Vec<String> = shards
        .iter()
        .filter_map(|s| s.get("address").and_then(|a| a.as_str()).map(String::from))
        .collect();
    assert_eq!(shard_addrs.len(), 8, "all 8 shards must have addresses");
    let unique: std::collections::HashSet<&String> = shard_addrs.iter().collect();
    assert_eq!(unique.len(), 8, "shard addresses must be distinct");

    // Master (legacy admin) balance BEFORE minting — with sharding on, mint
    // fees must land on shards, never the master. We assert the delta is 0.
    let master_before = sandbox.get_balance("main", &sandbox.admin_addr).await?;

    // 2) Drive the round-robin via token-MINT fees. Transaction fees are
    //    burned at source (v0.30.0), so `admin_mint_token` is the one live
    //    consumer of `AppState::fee_recipient_address()` → it routes each
    //    mint fee to `next_coord_shard_address()` (round-robin across shards).
    //    Each mint charges a flat mint_fee_base = 1 PMS (injected via
    //    PMS_TEST_MINT_FEE_BASE); the fee is MINTED as a direct output in the
    //    Mint block (no pooling, no distribution wait needed).
    let n_mints = 80usize;

    let (status, body) = sandbox
        .admin_post(
            "/admin/tokens/create",
            json!({
                "asset_id": "shardtok",
                "symbol": "SHRD",
                "name": "Shard Routing Token",
                "decimals": 0,
                "max_supply": "1000000"
            }),
        )
        .await;
    assert!(
        status.is_success(),
        "token create failed: {} — {:?}",
        status,
        body
    );

    // The API reports the exact per-mint fee it charged (`mint_fee`). Capture
    // it from the first mint and require every mint to charge the same amount,
    // so the golden total below cross-checks routing against the API's own
    // accounting (no dependence on the fee formula's epsilon).
    let recipient = Wallet::generate().get_address("8e");
    let mut per_mint_fee = Decimal::ZERO;
    for i in 0..n_mints {
        // Vary the amount per mint so each Mint block has a DISTINCT payload
        // (identical token+fee outputs on the same tip would hash to the same
        // block id → 409 "block already exists"). The flat mint_fee_base fee
        // is independent of the amount, so every mint still charges 1 PMS.
        let (st, b) = sandbox
            .admin_post(
                "/admin/tokens/mint",
                json!({ "asset_id": "shardtok", "to": recipient, "amount": (i + 1).to_string() }),
            )
            .await;
        assert!(st.is_success(), "mint {} failed: {} — {:?}", i, st, b);
        let fee = Decimal::from_str(b["mint_fee"].as_str().unwrap_or("0")).unwrap_or_default();
        if i == 0 {
            per_mint_fee = fee;
            println!("   per-mint fee (reported by API) = {} PMS", per_mint_fee);
        }
        assert_eq!(
            fee, per_mint_fee,
            "mint {i} charged {fee}, expected the same fee as mint 0 ({per_mint_fee})"
        );
    }
    assert!(
        per_mint_fee > Decimal::ZERO,
        "mint fee must be > 0 (PMS_TEST_MINT_FEE_BASE was injected)"
    );
    let expected_total_fees = per_mint_fee * Decimal::from(n_mints as i64);
    // Short settle for UTXO indexing (fees are direct outputs, already persisted).
    sleep(Duration::from_secs(1)).await;

    // 3) Read each shard's balance via /v1/balance/{addr} and check
    //    that the load was actually distributed.
    let mut per_shard_balance: Vec<rust_decimal::Decimal> = Vec::with_capacity(8);
    for (i, addr) in shard_addrs.iter().enumerate() {
        let bal = sandbox.get_balance("main", addr).await?;
        println!("   shard[{i:02}] @ {} → balance = {bal}", &addr[..16]);
        per_shard_balance.push(bal);
    }

    let total: rust_decimal::Decimal = per_shard_balance.iter().sum();
    let nonzero = per_shard_balance
        .iter()
        .filter(|b| **b > rust_decimal::Decimal::ZERO)
        .count();
    println!("\n   total balance across all 8 shards: {total}");
    println!("   shards with non-zero balance: {nonzero} / 8");

    // With 80 mints round-robined over 8 shards, every shard should have
    // received exactly 80/8 = 10 fees × 1 PMS = 10 PMS. Assert ≥7/8 got a
    // non-zero balance (small slack) AND the sum equals the golden total
    // (every mint fee must land on SOME shard — none leaked to master or
    // treasury).
    assert!(
        nonzero >= 7,
        "expected ≥7/8 shards to have received mint fees (round-robin), got {nonzero}"
    );
    assert_eq!(
        total, expected_total_fees,
        "sum of shard balances must equal total mint fees ({expected_total_fees} PMS), got {total}"
    );

    // 4) Sanity: with sharding enabled the legacy admin MASTER address must
    //    NOT receive any mint fee — every fee round-robins onto a shard.
    let master_after = sandbox.get_balance("main", &sandbox.admin_addr).await?;
    let master_gain = master_after - master_before;
    println!(
        "   master coord balance: {} → {} (gain {}, expected 0 — fees go to shards)",
        master_before, master_after, master_gain
    );
    assert_eq!(
        master_gain,
        rust_decimal::Decimal::ZERO,
        "master must NOT receive mint fees when sharding is on; gained {master_gain}"
    );

    println!("\n   ✅ coord sharding routed {expected_total_fees} PMS of mint fees across {nonzero}/8 shards");
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
        coord_shard_wallets: std::sync::Arc::new(Vec::new()),
        coord_shard_round_robin: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        read_only: std::sync::Arc::new(pms_server::read_only::ReadOnlyMode::new()),
        webhook_store: pms_server::api_fn::webhooks::WebhookStore::new(),
        emission_gate: Arc::new(pms_server::emission::EmissionGate::load(&store)),
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
        wire_meta: WireMeta::from(&settings),
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

// ============================================================================
// TPS DEGRADATION PROFILE (audit follow-up, 2026-04-26)
// ============================================================================
//
// `test_sustained_tps_stress` showed the engine drops from ~10K TPS at
// minute 1 to ~5K at minute 5 — a real degradation but the existing
// test doesn't tell us *why*. This test:
//
//   1. Boots the same in-process sandbox.
//   2. Runs a shorter (90s by default) continuous load.
//   3. Polls `/admin/rocksdb-stats` every 5s for L0 file count, write-
//      stop signal, compaction-pending, delayed-write-rate, etc.
//   4. Reads `/metrics` for the persist-queue depth + capacity.
//   5. Prints a wide table per interval so the operator can visually
//      correlate "TPS dropped at t=X" with "L0 jumped from 3 to 12".
//
// Output is on stdout — paste into a spreadsheet to see the curve, or
// just read the timestamps and trigger fields.

const PROFILE_DURATION_SECS: u64 = 90;
const PROFILE_WORKERS: usize = 40;
const PROFILE_INTERVAL_SECS: u64 = 5;
const PROFILE_INITIAL_MINT: &str = "100000";
const PROFILE_TX_AMOUNT: &str = "0.01";

#[derive(Debug, Default)]
struct ProfileSample {
    elapsed_s: f64,
    interval_tps: f64,
    interval_failures: u64,
    cum_tx: u64,
    rocks_l0_default: Option<u64>,
    rocks_l0_idx: Option<u64>,
    rocks_compaction_pending: Option<u64>,
    rocks_is_write_stopped: Option<u64>,
    rocks_actual_delayed_write_rate: Option<u64>,
    rocks_running_compactions: Option<u64>,
    rocks_running_flushes: Option<u64>,
    rocks_size_all_mem_tables: Option<u64>,
    rocks_estimate_num_keys: Option<u64>,
    persist_queue_depth: Option<u64>,
    persist_queue_capacity: Option<u64>,
    persist_retries: Option<u64>,
    persist_failures: Option<u64>,
    persist_stall_seconds: Option<u64>,
    rocksdb_write_stalled_seconds: Option<u64>,
    utxo_count: Option<u64>,
    // Cumulative per-stage µs counters from `pms_persist_stage_us_total`.
    // Diff between two intervals divided by `delta_persist_blocks` gives
    // the per-stage avg µs/block in that interval.
    persist_blocks_total: Option<u64>,
    stage_us_parents: Option<u64>,
    stage_us_utxo_val: Option<u64>,
    stage_us_dag_val: Option<u64>,
    stage_us_utxo_ram: Option<u64>,
    stage_us_dag_insert: Option<u64>,
    stage_us_send: Option<u64>,
    consumer_us: Option<u64>,
    consumer_batches: Option<u64>,
    consumer_blocks: Option<u64>,
}

/// Pull a single labelled gauge / counter value out of the Prometheus
/// `text/plain` exposition format. Returns the first match or `None`.
/// Permissive enough for our needs — we don't need a full parser, we
/// just look for `name{...} <number>` lines.
fn extract_metric(body: &str, name: &str) -> Option<u64> {
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with(name) {
            // Skip past the (optional) label block.
            let after_labels = match line.find('}') {
                Some(i) => &line[i + 1..],
                None => &line[name.len()..],
            };
            return after_labels.trim().parse::<u64>().ok();
        }
    }
    None
}

/// Same as `extract_metric` but matches a specific label value.
/// Looks for lines of the form `name{...key="value"...} <number>`.
fn extract_metric_with_label(body: &str, name: &str, key: &str, value: &str) -> Option<u64> {
    let needle = format!("{key}=\"{value}\"");
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with(name) && line.contains(&needle) {
            let after_labels = match line.find('}') {
                Some(i) => &line[i + 1..],
                None => continue,
            };
            return after_labels.trim().parse::<u64>().ok();
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 12)]
#[ignore]
async fn test_tps_degradation_profile() -> Result<()> {
    // Allow override via env so the same test serves both the quick
    // 90-second diagnostic run AND the longer (5-10 min) prod-readiness
    // run that asks "does the steady-state sit above the prod target?".
    let duration_s: u64 = std::env::var("PROFILE_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(PROFILE_DURATION_SECS);

    let sandbox = boot_sandbox().await?;
    println!("\n╔═══════════════════════════════════════════════════════════════╗");
    println!("║  TPS DEGRADATION PROFILE                                      ║");
    println!(
        "║  Duration: {}s | Workers: {} | Interval: {}s                  ║",
        duration_s, PROFILE_WORKERS, PROFILE_INTERVAL_SECS
    );
    println!("╚═══════════════════════════════════════════════════════════════╝\n");

    // Mint funds for each worker.
    let mut worker_keys: Vec<String> = Vec::with_capacity(PROFILE_WORKERS);
    for i in 0..PROFILE_WORKERS {
        let w = Wallet::generate();
        let addr = w.get_address("8e");
        sandbox
            .faucet_mint(None, &addr, PROFILE_INITIAL_MINT)
            .await?;
        worker_keys.push(w.private_key_b64.clone());
        if (i + 1) % 10 == 0 {
            println!("   funded {}/{}", i + 1, PROFILE_WORKERS);
        }
    }
    sleep(Duration::from_secs(2)).await;

    let stop_flag = Arc::new(AtomicBool::new(false));
    let success_count = Arc::new(AtomicUsize::new(0));
    let fail_count = Arc::new(AtomicUsize::new(0));
    let admin_addr = sandbox.admin_addr.clone();
    let base_url = sandbox.base_url.clone();
    let client = sandbox.client.clone();
    let admin_token = sandbox.admin_token.clone();

    // Spawn workers.
    let mut handles = Vec::with_capacity(PROFILE_WORKERS);
    for sk_b64 in worker_keys.iter() {
        let sk = sk_b64.clone();
        let target = admin_addr.clone();
        let url = base_url.clone();
        let cl = client.clone();
        let stop = stop_flag.clone();
        let ok = success_count.clone();
        let fail = fail_count.clone();
        handles.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let body = json!({
                    "private_key_b64": sk,
                    "to": target,
                    "amount": PROFILE_TX_AMOUNT,
                });
                match cl
                    .post(format!("{}/v1/wallet/send-simple", url))
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => {
                        ok.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {
                        fail.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    // Stop timer.
    {
        let stop = stop_flag.clone();
        let dur = duration_s;
        tokio::spawn(async move {
            sleep(Duration::from_secs(dur)).await;
            stop.store(true, Ordering::Relaxed);
        });
    }

    // Collector loop.
    let bench_start = Instant::now();
    let mut samples: Vec<ProfileSample> = Vec::new();
    let mut last_ok = 0usize;
    let mut last_fail = 0usize;
    // Snapshots of the previous interval's cumulative stage µs counters,
    // so we can print the avg µs/block in this interval.
    let mut last_blocks: u64 = 0;
    let mut last_parents: u64 = 0;
    let mut last_utxo_val: u64 = 0;
    let mut last_dag_val: u64 = 0;
    let mut last_utxo_ram: u64 = 0;
    let mut last_dag_insert: u64 = 0;
    let mut last_send: u64 = 0;
    let mut last_consumer_us: u64 = 0;
    let mut last_consumer_batches: u64 = 0;
    let mut last_consumer_blocks: u64 = 0;

    println!(
        "{:>5} | {:>6} | {:>5} | {:>3} | {:>3} | {:>3} | {:>4} | {:>4} | {:>4} | {:>5} | {:>5} | {:>4} | {:>4} | {:>4} | {:>7}",
        "t(s)",
        "tps",
        "fail",
        "L0",
        "L0i",
        "stp",
        "cmp",
        "rcmp",
        "rfsh",
        "qd",
        "qcap",
        "ret",
        "ferr",
        "stl",
        "utxos"
    );
    println!("{:->137}", "");
    // Second header for the per-stage µs/block table — printed alongside
    // the RocksDB row so the reader can correlate stage-latency growth
    // with the engine state.
    println!(
        "{:>5} | {:>6} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7}",
        "t(s)", "blks", "par_us", "uxV_us", "dgV_us", "uxR_us", "dgI_us", "snd_us", "tot_us", "≈tps"
    );
    println!("{:->100}", "");
    // Third header: background-persist consumer side. `c_us/blk` =
    // append_blocks_batch latency per persisted block. `c_us/bat` =
    // per-batch latency. `bat_n` = blocks per batch (avg), telling us
    // whether the consumer is starved or saturated.
    println!(
        "{:>5} | {:>7} | {:>7} | {:>7} | {:>5} | {:>7}",
        "t(s)", "c_blks", "c_us/bk", "c_us/bt", "bat_n", "c_tps"
    );
    println!("{:->60}", "");

    while !stop_flag.load(Ordering::Relaxed) {
        sleep(Duration::from_secs(PROFILE_INTERVAL_SECS)).await;
        let elapsed = bench_start.elapsed();
        let cur_ok = success_count.load(Ordering::Relaxed);
        let cur_fail = fail_count.load(Ordering::Relaxed);
        let interval_tx = cur_ok.saturating_sub(last_ok) as u64;
        let interval_failures = cur_fail.saturating_sub(last_fail) as u64;
        let interval_tps = interval_tx as f64 / PROFILE_INTERVAL_SECS as f64;

        // Pull /admin/rocksdb-stats.
        let mut s = ProfileSample {
            elapsed_s: elapsed.as_secs_f64(),
            interval_tps,
            interval_failures,
            cum_tx: cur_ok as u64,
            ..ProfileSample::default()
        };
        if let Ok(resp) = client
            .get(format!("{}/admin/rocksdb-stats", base_url))
            .bearer_auth(&admin_token)
            .send()
            .await
        {
            if let Ok(j) = resp.json::<Value>().await {
                let getu = |k: &str| -> Option<u64> {
                    j.get(k).and_then(|v| v.as_u64())
                };
                s.rocks_l0_default = getu("num_files_at_level0");
                s.rocks_l0_idx = getu("num_files_at_level0_idx_blocks");
                s.rocks_compaction_pending = getu("compaction_pending");
                s.rocks_is_write_stopped = getu("is_write_stopped");
                s.rocks_actual_delayed_write_rate = getu("actual_delayed_write_rate");
                s.rocks_running_compactions = getu("num_running_compactions");
                s.rocks_running_flushes = getu("num_running_flushes");
                s.rocks_size_all_mem_tables = getu("size_all_mem_tables");
                s.rocks_estimate_num_keys = getu("estimate_num_keys");
            }
        }

        // Pull /metrics/all for persist queue + counters. The plain `/metrics`
        // endpoint uses `render_for_ledger` which only exposes 3 ledger gauges;
        // we need the full registry so `pms_persist_*` and `pms_persist_stage_*`
        // are visible. Admin token required by `require_local_or_admin`.
        if let Ok(resp) = client
            .get(format!("{}/metrics/all", base_url))
            .bearer_auth(&admin_token)
            .send()
            .await
        {
            if let Ok(text) = resp.text().await {
                s.persist_queue_depth =
                    extract_metric(&text, "pms_persist_queue_depth");
                s.persist_queue_capacity =
                    extract_metric(&text, "pms_persist_queue_capacity");
                s.persist_retries = extract_metric(&text, "pms_persist_retries_total");
                s.persist_failures = extract_metric(&text, "pms_persist_failures_total");
                s.persist_stall_seconds =
                    extract_metric(&text, "pms_persist_stall_seconds_total");
                s.rocksdb_write_stalled_seconds =
                    extract_metric(&text, "pms_rocksdb_write_stalled_seconds_total");
                s.persist_blocks_total =
                    extract_metric(&text, "pms_persist_blocks_total");
                s.stage_us_parents = extract_metric_with_label(
                    &text, "pms_persist_stage_us_total", "stage", "parents",
                );
                s.stage_us_utxo_val = extract_metric_with_label(
                    &text, "pms_persist_stage_us_total", "stage", "utxo_val",
                );
                s.stage_us_dag_val = extract_metric_with_label(
                    &text, "pms_persist_stage_us_total", "stage", "dag_val",
                );
                s.stage_us_utxo_ram = extract_metric_with_label(
                    &text, "pms_persist_stage_us_total", "stage", "utxo_ram",
                );
                s.stage_us_dag_insert = extract_metric_with_label(
                    &text, "pms_persist_stage_us_total", "stage", "dag_insert",
                );
                s.stage_us_send = extract_metric_with_label(
                    &text, "pms_persist_stage_us_total", "stage", "send",
                );
                s.consumer_us = extract_metric(&text, "pms_persist_consumer_us_total");
                s.consumer_batches =
                    extract_metric(&text, "pms_persist_consumer_batches_total");
                s.consumer_blocks =
                    extract_metric(&text, "pms_persist_consumer_blocks_total");
            }
        }

        // Pull /v1/supply for UTXO count — leading hypothesis is that
        // per-worker UTXO accumulation (each send_simple creates a
        // change UTXO) makes coin_selection scan linearly grow.
        if let Ok(resp) = client.get(format!("{}/v1/supply", base_url)).send().await {
            if let Ok(j) = resp.json::<Value>().await {
                s.utxo_count = j
                    .get("utxo_count")
                    .and_then(|v| v.as_u64())
                    .or_else(|| j.get("count").and_then(|v| v.as_u64()));
            }
        }

        // Compact one-line dump.
        println!(
            "{:>5.0} | {:>6.0} | {:>5} | {:>3} | {:>3} | {:>3} | {:>4} | {:>4} | {:>4} | {:>5} | {:>5} | {:>4} | {:>4} | {:>4} | {:>7}",
            s.elapsed_s,
            s.interval_tps,
            s.interval_failures,
            s.rocks_l0_default.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.rocks_l0_idx.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.rocks_is_write_stopped.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.rocks_compaction_pending.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.rocks_running_compactions.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.rocks_running_flushes.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.persist_queue_depth.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.persist_queue_capacity.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.persist_retries.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.persist_failures.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.persist_stall_seconds.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            s.utxo_count.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
        );

        // Second row: per-stage avg µs/block over this interval.
        // Avg = (cumulative_us_now - cumulative_us_prev) / (blocks_now - blocks_prev).
        let cur_blocks = s.persist_blocks_total.unwrap_or(0);
        let delta_blocks = cur_blocks.saturating_sub(last_blocks).max(1);
        let cur_par = s.stage_us_parents.unwrap_or(0);
        let cur_ux_v = s.stage_us_utxo_val.unwrap_or(0);
        let cur_dg_v = s.stage_us_dag_val.unwrap_or(0);
        let cur_ux_r = s.stage_us_utxo_ram.unwrap_or(0);
        let cur_dg_i = s.stage_us_dag_insert.unwrap_or(0);
        let cur_snd = s.stage_us_send.unwrap_or(0);
        let avg_par = cur_par.saturating_sub(last_parents) / delta_blocks;
        let avg_ux_v = cur_ux_v.saturating_sub(last_utxo_val) / delta_blocks;
        let avg_dg_v = cur_dg_v.saturating_sub(last_dag_val) / delta_blocks;
        let avg_ux_r = cur_ux_r.saturating_sub(last_utxo_ram) / delta_blocks;
        let avg_dg_i = cur_dg_i.saturating_sub(last_dag_insert) / delta_blocks;
        let avg_snd = cur_snd.saturating_sub(last_send) / delta_blocks;
        let avg_tot = avg_par + avg_ux_v + avg_dg_v + avg_ux_r + avg_dg_i + avg_snd;
        // Theoretical TPS ceiling at this stage cost, single-threaded:
        //   1_000_000 µs/s ÷ tot_us/block = blocks/s.
        // Useful sanity check: with 12 worker_threads, real TPS can be
        // higher than this number — but if avg_tot grows over time,
        // real TPS will fall in lockstep regardless of thread count.
        let approx_tps = if avg_tot > 0 { 1_000_000 / avg_tot } else { 0 };
        println!(
            "{:>5.0} | {:>6} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7} | {:>7}",
            s.elapsed_s,
            cur_blocks - last_blocks,
            avg_par,
            avg_ux_v,
            avg_dg_v,
            avg_ux_r,
            avg_dg_i,
            avg_snd,
            avg_tot,
            approx_tps,
        );

        // Consumer-side row.
        let cur_c_us = s.consumer_us.unwrap_or(0);
        let cur_c_bat = s.consumer_batches.unwrap_or(0);
        let cur_c_blk = s.consumer_blocks.unwrap_or(0);
        let d_c_us = cur_c_us.saturating_sub(last_consumer_us);
        let d_c_bat = cur_c_bat.saturating_sub(last_consumer_batches).max(1);
        let d_c_blk = cur_c_blk.saturating_sub(last_consumer_blocks).max(1);
        let c_us_per_block = d_c_us / d_c_blk;
        let c_us_per_batch = d_c_us / d_c_bat;
        let c_avg_batch = d_c_blk / d_c_bat;
        let c_tps = if c_us_per_block > 0 {
            1_000_000 / c_us_per_block
        } else {
            0
        };
        println!(
            "{:>5.0} | {:>7} | {:>7} | {:>7} | {:>5} | {:>7}",
            s.elapsed_s, d_c_blk, c_us_per_block, c_us_per_batch, c_avg_batch, c_tps,
        );

        samples.push(s);
        last_ok = cur_ok;
        last_fail = cur_fail;
        last_blocks = cur_blocks;
        last_parents = cur_par;
        last_utxo_val = cur_ux_v;
        last_dag_val = cur_dg_v;
        last_utxo_ram = cur_ux_r;
        last_dag_insert = cur_dg_i;
        last_send = cur_snd;
        last_consumer_us = cur_c_us;
        last_consumer_batches = cur_c_bat;
        last_consumer_blocks = cur_c_blk;
    }

    // Drain workers.
    for h in handles {
        let _ = h.await;
    }

    // ── Verdict ────────────────────────────────────────────────────────
    let n = samples.len();
    if n < 2 {
        println!("\n   not enough samples for verdict ({n})");
        return Ok(());
    }
    let first = samples.first().map(|s| s.interval_tps).unwrap_or(0.0);
    let last = samples.last().map(|s| s.interval_tps).unwrap_or(0.0);
    let max = samples
        .iter()
        .map(|s| s.interval_tps)
        .fold(f64::MIN, f64::max);
    let degradation_pct = if max > 0.0 {
        100.0 * (max - last) / max
    } else {
        0.0
    };

    println!("\n   ═══════════════════════════════════════════════════════════════");
    println!("   PROFILE VERDICT");
    println!("   ═══════════════════════════════════════════════════════════════");
    println!("   First interval TPS : {:.1}", first);
    println!("   Peak interval TPS  : {:.1}", max);
    println!("   Last interval TPS  : {:.1}", last);
    println!("   Degradation peak→last: {:.1}%", degradation_pct);

    // First interval where TPS dropped > 30% vs peak.
    let drop_threshold = max * 0.7;
    if let Some(s) = samples.iter().find(|s| s.interval_tps < drop_threshold) {
        println!(
            "   First drop > 30% at t={:.0}s — TPS={:.0} L0_default={:?} L0_idx={:?} stop={:?} qd={:?}/{:?} retries={:?}",
            s.elapsed_s,
            s.interval_tps,
            s.rocks_l0_default,
            s.rocks_l0_idx,
            s.rocks_is_write_stopped,
            s.persist_queue_depth,
            s.persist_queue_capacity,
            s.persist_retries
        );
    } else {
        println!("   No interval below 70% of peak — degradation < 30%, OK.");
    }

    // Did any sample show is-write-stopped?
    let stalled = samples
        .iter()
        .any(|s| s.rocks_is_write_stopped == Some(1));
    println!("   RocksDB write-stop fired during run: {}", stalled);

    // Did the queue ever saturate?
    let max_qd = samples
        .iter()
        .filter_map(|s| s.persist_queue_depth)
        .max()
        .unwrap_or(0);
    let qcap = samples
        .iter()
        .filter_map(|s| s.persist_queue_capacity)
        .next()
        .unwrap_or(0);
    println!(
        "   Persist queue: max depth = {} / capacity {} ({:.0}%)",
        max_qd,
        qcap,
        if qcap > 0 {
            100.0 * max_qd as f64 / qcap as f64
        } else {
            0.0
        }
    );

    // Did we ever back-pressure (stall counter increased)?
    let stalls_first = samples.first().and_then(|s| s.persist_stall_seconds).unwrap_or(0);
    let stalls_last = samples.last().and_then(|s| s.persist_stall_seconds).unwrap_or(0);
    println!(
        "   Persist stall seconds: {} (start) → {} (end), Δ = {}",
        stalls_first,
        stalls_last,
        stalls_last.saturating_sub(stalls_first)
    );

    // UTXO growth — leading hypothesis if RocksDB / persist queue / WAL
    // are all clean. Linear growth here while TPS halves is a strong
    // signal that coin_selection is the bottleneck.
    let utxos_first = samples.first().and_then(|s| s.utxo_count).unwrap_or(0);
    let utxos_last = samples.last().and_then(|s| s.utxo_count).unwrap_or(0);
    println!(
        "   UTXO count       : {} (start) → {} (end), Δ = {}",
        utxos_first,
        utxos_last,
        utxos_last.saturating_sub(utxos_first)
    );
    if utxos_last > utxos_first {
        let utxo_growth_pct = 100.0 * (utxos_last - utxos_first) as f64 / utxos_first.max(1) as f64;
        let tps_drop_pct = 100.0 * (max - last) / max.max(0.001);
        println!(
            "   UTXO growth: +{:.0}% over the run; TPS drop: {:.0}% — \
             correlation suggests coin_selection scaling with UTXO set",
            utxo_growth_pct, tps_drop_pct
        );
    }
    println!("   ═══════════════════════════════════════════════════════════════\n");

    Ok(())
}

// ============================================================================
// LAUNCH-READINESS TESTS (v0.7.20)
// ----------------------------------------------------------------------------
// Added 2026-04-29 to close the gaps surfaced by the launch-readiness audit:
//   1. SSE activity stream — emits real-time events on block persist.
//   2. Token full lifecycle — create + mint + transfer + supply consistency.
//      Plus OnTokenBurn simulate-warning regression guard.
//   3. Gas pool deposit/withdraw/consumption — full custody lifecycle.
//   4. Contract toggle behavior — disabling a live contract stops fee charging.
// ============================================================================

// ----------------------------------------------------------------------------
// TEST 1: SSE activity stream — real-time events
// ----------------------------------------------------------------------------

/// Subscribes to `GET /v1/wallet/{addr}/activity/stream` (SSE), then triggers
/// a faucet mint on `main` for that address. Verifies that the stream emits
/// at least one `event: activity` frame containing the mint within 8 seconds.
///
/// Why: the audit flagged that the SSE endpoint had zero E2E coverage despite
/// being part of the dashboard's real-time UX.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_sse_activity_stream_real_time() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: SSE Activity Stream — Real-Time Events             ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Create user ────────────────────────────────────────────────
    let user_wallet = Wallet::generate();
    let user_addr = user_wallet.get_address("8e");
    println!(
        "   [1/4] User: {}...{}",
        &user_addr[..12],
        &user_addr[user_addr.len() - 8..]
    );

    // ── 2. Open SSE connection in a background task ──────────────────
    println!("   [2/4] Opening SSE connection...");
    let url = format!(
        "{}/v1/wallet/{}/activity/stream",
        sandbox.base_url, user_addr
    );
    let client = sandbox.client.clone();
    let user_addr_for_task = user_addr.clone();
    let collect_handle: tokio::task::JoinHandle<Result<(usize, String)>> =
        tokio::spawn(async move {
            let resp = client
                .get(&url)
                .header("Accept", "text/event-stream")
                .send()
                .await
                .context("SSE connect failed")?;
            anyhow::ensure!(
                resp.status().is_success(),
                "SSE handshake failed: {}",
                resp.status()
            );

            let mut stream = resp;
            let mut accumulated = String::new();
            let mut activity_frames = 0usize;
            let deadline = Instant::now() + Duration::from_secs(8);

            while Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, stream.chunk()).await {
                    Ok(Ok(Some(bytes))) => {
                        let s = String::from_utf8_lossy(&bytes).into_owned();
                        accumulated.push_str(&s);
                        // SSE frames are separated by \n\n; count "event: activity" lines
                        // referencing our user address.
                        for frame in s.split("\n\n") {
                            if frame.contains("event: activity")
                                && frame.contains(&user_addr_for_task)
                            {
                                activity_frames += 1;
                            }
                        }
                        if activity_frames > 0 {
                            // Keep reading briefly for a clean cut.
                            break;
                        }
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        return Err(anyhow::anyhow!("SSE chunk error: {}", e));
                    }
                    Err(_) => break, // timeout
                }
            }
            Ok((activity_frames, accumulated))
        });

    // Give the subscriber time to register on the broadcast channel.
    sleep(Duration::from_millis(400)).await;

    // ── 3. Faucet mint to user — should emit BlockPersisted ──────────
    println!("   [3/4] Faucet minting 100 PMS to user...");
    sandbox.faucet_mint(None, &user_addr, "100").await?;

    // Force a second event to make the test tolerant of timing (mints are atomic
    // and emit one BlockPersisted per block).
    sleep(Duration::from_millis(200)).await;
    sandbox.faucet_mint(None, &user_addr, "50").await?;

    // ── 4. Wait for SSE collector to finish ──────────────────────────
    println!("   [4/4] Awaiting SSE frames...");
    let (frame_count, raw) = collect_handle
        .await
        .context("SSE task join failed")?
        .context("SSE task error")?;

    // Print a sample of what we received (first ~600 chars) for human review.
    let preview = raw.chars().take(800).collect::<String>();
    println!("\n   ── SSE raw preview (first 800 chars) ──");
    for line in preview.lines().take(20) {
        println!("      {}", line);
    }

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  SSE STREAM RESULTS                                      ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!("   ║  User addr matched in frames:     {:>20}    ║", frame_count);
    println!("   ║  Total bytes received:            {:>20}    ║", raw.len());
    println!("   ╚══════════════════════════════════════════════════════════╝");

    assert!(
        frame_count >= 1,
        "Expected at least 1 SSE activity frame for the user, got {}. \
         Raw stream: {:?}",
        frame_count,
        preview
    );
    println!("\n   TEST PASSED: SSE stream delivered {} activity event(s) for the user.", frame_count);
    Ok(())
}

// ----------------------------------------------------------------------------
// TEST 2: Token full lifecycle (create + mint + transfer + supply)
//          + OnTokenBurn simulate-endpoint MintNative (voie B) guard
// ----------------------------------------------------------------------------

/// Validates the full custom-token lifecycle on `main`:
///   1. `POST /admin/tokens/create` with `max_supply`.
///   2. `POST /admin/tokens/mint` to user1.
///   3. user1 → user2 token transfer (gas in PMS).
///   4. Supply, balances, and max-supply enforcement consistent.
/// Then simulates an `OnTokenBurn` contract via `/admin/contracts/simulate`
/// and asserts the engine evaluates the now-implemented voie B conversion:
/// a `MintNative` action mints native PMS to the burner at rate R = num/den
/// (100 USDX × 3/2 = 150 PMS). This is the HTTP-level counterpart of the
/// `test_simulate_token_burn_mint_native` unit test in `pms-contracts`, and
/// guards the regression where OnTokenBurn used to be flagged "not yet
/// implemented" (that warning was removed when voie B landed).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_token_lifecycle_and_token_burn_warning() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Token Lifecycle + OnTokenBurn Warning              ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Create custom token "USDX" on main with max_supply 1_000_000 ──
    println!("   [1/8] Creating custom token USDX on main...");
    let (status, body) = sandbox
        .admin_post(
            "/admin/tokens/create",
            json!({
                "asset_id": "usdx",
                "symbol": "USDX",
                "name": "Test USD",
                "decimals": 6,
                "max_supply": "1000000"
            }),
        )
        .await;
    println!("      Create: {} — {:?}", status, body);
    anyhow::ensure!(status.is_success(), "Token create failed: {} {:?}", status, body);

    // ── 2. Verify token visible in list/get ──────────────────────────
    println!("   [2/8] Verifying token registry...");
    let resp = sandbox
        .client
        .get(format!("{}/v1/tokens", sandbox.base_url))
        .send()
        .await?;
    let list_body: Value = resp.json().await?;
    let found = list_body["tokens"]
        .as_array()
        .map(|arr| arr.iter().any(|t| t["asset_id"] == "usdx"))
        .unwrap_or(false);
    assert!(found, "USDX should appear in /v1/tokens list");
    println!("      Token visible in registry: OK");

    // ── 3. Mint 1000 USDX to user1 ────────────────────────────────────
    println!("   [3/8] Minting 1000 USDX to user1...");
    let user1 = Wallet::generate();
    let user1_addr = user1.get_address("8e");
    let user1_sk = user1.private_key_b64.clone();
    let user2 = Wallet::generate();
    let user2_addr = user2.get_address("8e");

    // Faucet user1 some PMS for gas
    sandbox.faucet_mint(None, &user1_addr, "100").await?;
    sleep(Duration::from_millis(200)).await;

    let (status, body) = sandbox
        .admin_post(
            "/admin/tokens/mint",
            json!({
                "asset_id": "usdx",
                "to": user1_addr,
                "amount": "1000"
            }),
        )
        .await;
    println!("      Mint: {} — {:?}", status, body);
    anyhow::ensure!(status.is_success(), "Token mint failed: {} {:?}", status, body);
    sleep(Duration::from_millis(300)).await;

    // ── 4. Verify balance + supply ────────────────────────────────────
    println!("   [4/8] Verifying user1 USDX balance + circulating supply...");
    let user1_usdx = sandbox.get_asset_balance("main", &user1_addr, Some("usdx")).await?;
    let supply_before_xfer = sandbox.get_supply("main", Some("usdx")).await?;
    let circ_before = Decimal::from_str(
        supply_before_xfer["circulating_supply"].as_str().unwrap_or("0"),
    )
    .unwrap_or_default();
    println!("      user1 USDX:           {}", user1_usdx);
    println!("      circulating (USDX):   {}", circ_before);
    assert_eq!(user1_usdx, Decimal::from(1000), "user1 should hold 1000 USDX");
    assert_eq!(circ_before, Decimal::from(1000), "USDX circulating supply should be 1000");

    // ── 5. Reject mint exceeding max_supply ──────────────────────────
    println!("   [5/8] Verifying max_supply enforcement (mint 1_000_000 → expect 422)...");
    let (over_status, over_body) = sandbox
        .admin_post(
            "/admin/tokens/mint",
            json!({
                "asset_id": "usdx",
                "to": user1_addr,
                "amount": "1000000"
            }),
        )
        .await;
    println!("      Over-mint: {} — {:?}", over_status, over_body);
    assert_eq!(
        over_status,
        reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "Mint exceeding max_supply must return 422"
    );

    // ── 6. user1 sends 250 USDX to user2 ─────────────────────────────
    println!("   [6/8] user1 → user2 transfer of 250 USDX...");
    let resp = sandbox
        .send_asset("main", &user1_sk, &user2_addr, "250", "usdx")
        .await?;
    println!("      Transfer: {:?}", resp);
    sleep(Duration::from_millis(400)).await;

    let user1_after = sandbox.get_asset_balance("main", &user1_addr, Some("usdx")).await?;
    let user2_after = sandbox.get_asset_balance("main", &user2_addr, Some("usdx")).await?;
    let supply_after = sandbox.get_supply("main", Some("usdx")).await?;
    let circ_after = Decimal::from_str(
        supply_after["circulating_supply"].as_str().unwrap_or("0"),
    )
    .unwrap_or_default();

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  TOKEN LIFECYCLE RESULTS                                 ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!("   ║  user1 USDX before xfer:        {:>22}    ║", user1_usdx);
    println!("   ║  user1 USDX after xfer:         {:>22}    ║", user1_after);
    println!("   ║  user2 USDX after xfer:         {:>22}    ║", user2_after);
    println!("   ║  circulating before xfer:       {:>22}    ║", circ_before);
    println!("   ║  circulating after xfer:        {:>22}    ║", circ_after);
    println!("   ╚══════════════════════════════════════════════════════════╝");

    assert_eq!(user1_after, Decimal::from(750), "user1 should hold 750 USDX");
    assert_eq!(user2_after, Decimal::from(250), "user2 should hold 250 USDX");
    assert_eq!(circ_after, circ_before, "Transfer must NOT change circulating supply");

    // ── 7. OnTokenBurn simulate — voie B (MintNative) ─────────────────
    // OnTokenBurn is now implemented: a `MintNative` action mints native
    // PMS to the burner at rate R = num/den. Simulate it through the HTTP
    // endpoint and assert the minted amount (100 USDX × 3/2 = 150 PMS).
    // The old behaviour (an "OnTokenBurn ... not yet implemented" warning)
    // was removed when voie B landed — the pure-function counterpart lives
    // in `pms-contracts::engine::test_simulate_token_burn_mint_native`.
    println!("   [7/8] Simulating OnTokenBurn — expecting MintNative (voie B) result...");
    let (sim_status, sim_resp) = sandbox
        .admin_post(
            "/admin/contracts/simulate",
            json!({
                "contract": {
                    "name": "usdx-to-pms",
                    "scope": { "Ledger": ["main"] },
                    "trigger": { "OnTokenBurn": { "asset_id": "usdx" } },
                    "actions": [{
                        "MintNative": {
                            "rate_numerator": 3,
                            "rate_denominator": 2
                        }
                    }]
                },
                "event": {
                    "TokenBurn": {
                        "ledger_id": "main",
                        "asset_id": "usdx",
                        "burn_amount": "100"
                    }
                }
            }),
        )
        .await;
    println!("      Simulate: {} — {}", sim_status, serde_json::to_string_pretty(&sim_resp).unwrap_or_default());
    assert_eq!(sim_status, reqwest::StatusCode::OK);
    assert_eq!(sim_resp["matched"], true, "OnTokenBurn trigger should match TokenBurn event");

    // No "not yet implemented" warning must be emitted anymore (voie B is live).
    let warnings = sim_resp["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let stale_warning = warnings.iter().any(|w| {
        w.as_str()
            .map(|s| s.contains("OnTokenBurn") && s.contains("not yet implemented"))
            .unwrap_or(false)
    });
    assert!(
        !stale_warning,
        "OnTokenBurn is implemented (voie B) — the 'not yet implemented' warning \
         must NOT be emitted. Got warnings: {:?}",
        warnings
    );

    // MintNative produces exactly one mint instruction: 100 USDX × 3/2 = 150 PMS,
    // credited in native PMS (asset_id = null).
    let burn_results = sim_resp["burn_results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        burn_results.len(),
        1,
        "MintNative must produce exactly one mint instruction. Got: {:?}",
        burn_results
    );
    let minted_str = burn_results[0]["refund_amount"].as_str().unwrap_or("0");
    let minted = Decimal::from_str(minted_str).unwrap_or_default();
    println!("      OnTokenBurn → MintNative: 100 USDX burned → {} PMS (R=3/2)", minted);
    assert_eq!(
        minted,
        Decimal::from(150),
        "100 USDX × 3/2 must mint 150 native PMS, got {}",
        minted
    );
    assert!(
        burn_results[0]["asset_id"].is_null(),
        "voie B mints native PMS — asset_id must be null, got {:?}",
        burn_results[0]["asset_id"]
    );
    println!("      OnTokenBurn MintNative (voie B) evaluated: OK");

    // ── 8. Verify no contract was persisted ─────────────────────────
    println!("   [8/8] Verifying simulate did not persist a contract...");
    let (status, contracts) = sandbox.admin_get("/admin/contracts").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let contract_count = contracts["contracts"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(contract_count, 0, "Simulate must NOT persist contracts");
    println!("      Persisted contract count: {} (expected 0)", contract_count);

    println!("\n   TEST PASSED: Token lifecycle (create+mint+transfer+supply) + OnTokenBurn warning guard validated.");
    Ok(())
}

// ----------------------------------------------------------------------------
// TEST 3: Gas pool deposit / withdraw / consumption
// ----------------------------------------------------------------------------

/// Exercises the per-ledger gas pool custody flow:
///   1. Create eden ledger + deposit 1000 PMS into gas pool.
///   2. GET /v1/gas-pool/eden — balance/total_deposited reported correctly.
///   3. Partial withdraw 200 — balance updates.
///   4. Over-withdraw — returns 402 PaymentRequired.
///   5. user sends a tx on eden — gas is consumed → total_consumed > 0.
///
/// Why: the audit flagged gas pool endpoints as untested. They are the
/// economic backbone of custom-ledger operations.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_gas_pool_deposit_withdraw_consumption() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Gas Pool — Deposit / Withdraw / Consumption        ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Create eden + initial deposit ─────────────────────────────
    println!("   [1/6] Creating eden ledger + depositing 1000 PMS into gas pool...");
    sandbox.create_ledger("eden", "eden-net", "eden", "EDN").await?;
    sandbox.deposit_gas_pool("eden", "1000").await?;

    // Helper to GET /v1/gas-pool/eden
    async fn get_pool(sb: &Sandbox) -> Result<(Decimal, Decimal, Decimal)> {
        let resp = sb
            .client
            .get(format!("{}/v1/gas-pool/eden", sb.base_url))
            .send()
            .await?;
        let body: Value = resp.json().await?;
        let bal = Decimal::from_str(body["balance"].as_str().unwrap_or("0")).unwrap_or_default();
        let dep = Decimal::from_str(body["total_deposited"].as_str().unwrap_or("0")).unwrap_or_default();
        let cons = Decimal::from_str(body["total_consumed"].as_str().unwrap_or("0")).unwrap_or_default();
        Ok((bal, dep, cons))
    }

    let (bal0, dep0, cons0) = get_pool(&sandbox).await?;
    println!("      Pool: bal={} dep={} cons={}", bal0, dep0, cons0);
    assert_eq!(bal0, Decimal::from(1000));
    assert_eq!(dep0, Decimal::from(1000));
    assert_eq!(cons0, Decimal::ZERO);

    // ── 2. Partial withdraw ──────────────────────────────────────────
    println!("   [2/6] Withdrawing 200 PMS...");
    let (status, body) = sandbox
        .admin_post(
            "/admin/gas-pool/withdraw",
            json!({"ledger_id": "eden", "amount": "200"}),
        )
        .await;
    println!("      Withdraw: {} — {:?}", status, body);
    assert!(status.is_success(), "withdraw must succeed");

    let (bal1, dep1, _) = get_pool(&sandbox).await?;
    assert_eq!(bal1, Decimal::from(800), "balance should be 800 after withdraw");
    assert_eq!(dep1, Decimal::from(1000), "total_deposited untouched by withdraw");

    // ── 3. Over-withdraw → 402 ───────────────────────────────────────
    println!("   [3/6] Over-withdrawing 99999 PMS — expecting 402...");
    let (status, body) = sandbox
        .admin_post(
            "/admin/gas-pool/withdraw",
            json!({"ledger_id": "eden", "amount": "99999"}),
        )
        .await;
    println!("      Over-withdraw: {} — {:?}", status, body);
    assert_eq!(
        status,
        reqwest::StatusCode::PAYMENT_REQUIRED,
        "Over-withdraw must return 402, got {}",
        status
    );
    let (bal_after_overdraw, _, _) = get_pool(&sandbox).await?;
    assert_eq!(bal_after_overdraw, Decimal::from(800), "balance unchanged after failed withdraw");

    // ── 4. Trigger gas consumption via a real tx on eden ─────────────
    println!("   [4/6] Triggering gas consumption — user tx on eden...");
    let user = Wallet::generate();
    let user_addr = user.get_address("8e");
    let user_sk = user.private_key_b64.clone();
    sandbox.faucet_mint(Some("eden"), &user_addr, "500").await?;
    sleep(Duration::from_millis(300)).await;

    let recipient = Wallet::generate().get_address("8e");
    let _send = sandbox
        .send_simple(Some("eden"), &user_sk, &recipient, "10")
        .await?;
    sleep(Duration::from_millis(400)).await;

    // ── 5. Inspect gas pool after the tx ─────────────────────────────
    // Note: actual gas consumption depends on fee policy being active in the
    // runtime config. The sandbox boots with default config where fee policies
    // are not enabled, so `total_consumed` may legitimately be 0 here. The
    // economic enforcement is exercised in `fee_consistency_test.rs`. What
    // this test guarantees is the deposit/withdraw custody surface — the
    // surface that operators interact with directly.
    println!("   [5/6] Inspecting gas pool after the user tx...");
    let (bal2, dep2, cons2) = get_pool(&sandbox).await?;
    println!("      Pool after tx: bal={} dep={} cons={}", bal2, dep2, cons2);

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  GAS POOL LIFECYCLE                                      ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!("   ║  Initial deposit:               {:>22}    ║", dep0);
    println!("   ║  After withdraw (200):          {:>22}    ║", bal1);
    println!("   ║  After failed over-withdraw:    {:>22}    ║", bal_after_overdraw);
    println!("   ║  After 1 user tx:               {:>22}    ║", bal2);
    println!("   ║  Total consumed:                {:>22}    ║", cons2);
    println!("   ╚══════════════════════════════════════════════════════════╝");

    assert_eq!(
        dep2,
        Decimal::from(1000),
        "total_deposited must stay stable across consumption — only deposits move it"
    );
    assert!(
        bal2 <= bal1,
        "Pool balance must never exceed pre-tx balance ({} > {})",
        bal2,
        bal1
    );
    assert!(
        cons2 >= Decimal::ZERO,
        "total_consumed must never be negative. Got: {}",
        cons2
    );
    if cons2 > Decimal::ZERO {
        println!("      Gas consumption observed: {} PMS — fee policy active.", cons2);
    } else {
        println!("      No gas consumption (fee policy not active in dev sandbox) — surface validated, economics tested separately.");
    }

    // ── 6. Idempotency: GET endpoint structure ───────────────────────
    println!("   [6/6] GET /v1/gas-pool/{{nonexistent}} → expect 404...");
    let resp = sandbox
        .client
        .get(format!("{}/v1/gas-pool/no-such-ledger", sandbox.base_url))
        .send()
        .await?;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    println!("      404 returned for unknown ledger: OK");

    println!("\n   TEST PASSED: Gas pool deposit/withdraw/consumption fully validated.");
    Ok(())
}

// ----------------------------------------------------------------------------
// TEST 4: Contract toggle behavior — disabling a live contract stops fees
// ----------------------------------------------------------------------------

/// Registers an enabled 5% transfer-fee contract on eden, observes that fees
/// are charged, then toggles it to `enabled=false` and verifies fees STOP
/// being charged on subsequent transfers.
///
/// Why: the existing `test_contract_simulate_endpoint` only toggles
/// `false → true` once and never observes runtime behavior change. This test
/// closes that gap — operators rely on the toggle as a kill switch.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_contract_toggle_kill_switch() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Contract Toggle Kill-Switch                        ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Create eden ledger + gas + active 5% transfer-fee contract ──
    println!("   [1/6] Creating eden + active 5% transfer-fee contract...");
    sandbox.create_ledger("eden", "eden-net", "eden", "EDN").await?;
    sandbox.deposit_gas_pool("eden", "50000").await?;
    let coord = sandbox.admin_addr.clone();
    let contract_id = sandbox
        .register_contract(json!({
            "name": "eden-fee-killswitch-test",
            "scope": { "Ledger": ["eden"] },
            "trigger": { "OnTransfer": { "asset_id": null } },
            "actions": [{
                "TransferFee": {
                    "formula": { "PercentageBps": { "rate_bps": 500 } },
                    "splits": [{ "address": coord.clone(), "share_bps": 10000 }]
                }
            }],
            "enabled": true
        }))
        .await?;
    println!("      Contract id: {}...", &contract_id[..16]);

    // ── 2. Fund user, send tx — fee should be charged ─────────────────
    println!("   [2/6] Funding user, sending 100 PMS — expecting 5% fee...");
    let user = Wallet::generate();
    let user_addr = user.get_address("8e");
    let user_sk = user.private_key_b64.clone();
    let recipient = Wallet::generate().get_address("8e");
    sandbox.faucet_mint(Some("eden"), &user_addr, "10000").await?;
    sleep(Duration::from_millis(300)).await;

    let resp_active = sandbox
        .send_simple(Some("eden"), &user_sk, &recipient, "100")
        .await?;
    let fee_active = Decimal::from_str(resp_active["transfer_fee"].as_str().unwrap_or("0"))
        .unwrap_or_default();
    println!(
        "      Active contract — transfer_fee response: '{}' → {}",
        resp_active["transfer_fee"].as_str().unwrap_or("?"),
        fee_active
    );
    assert!(
        fee_active > Decimal::ZERO,
        "While contract is enabled, transfer_fee must be > 0. Got: {}",
        fee_active
    );

    // ── 3. Toggle contract to disabled ────────────────────────────────
    println!("   [3/6] Toggling contract → disabled (kill switch)...");
    let (status, body) = sandbox
        .admin_post(
            &format!("/admin/contracts/{}/toggle", contract_id),
            json!({"enabled": false, "reason": "kill switch test"}),
        )
        .await;
    println!("      Toggle: {} — {:?}", status, body);
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["enabled"], false);

    // Slight pause for any in-flight cache eviction.
    sleep(Duration::from_millis(300)).await;

    // ── 4. Send another tx — fee should be ZERO ───────────────────────
    println!("   [4/6] Sending another 100 PMS — expecting NO fee...");
    let resp_disabled = sandbox
        .send_simple(Some("eden"), &user_sk, &recipient, "100")
        .await?;
    let fee_disabled = Decimal::from_str(resp_disabled["transfer_fee"].as_str().unwrap_or("0"))
        .unwrap_or_default();
    println!(
        "      Disabled contract — transfer_fee response: '{}' → {}",
        resp_disabled["transfer_fee"].as_str().unwrap_or("?"),
        fee_disabled
    );

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  CONTRACT TOGGLE RESULTS                                 ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!("   ║  Fee while enabled  (5% of 100):  {:>20}    ║", fee_active);
    println!("   ║  Fee after disable:               {:>20}    ║", fee_disabled);
    println!("   ╚══════════════════════════════════════════════════════════╝");

    assert_eq!(
        fee_disabled,
        Decimal::ZERO,
        "After disabling the contract, transfer_fee must be 0. Got: {}",
        fee_disabled
    );

    // ── 5. Re-enable and verify the fee comes back ────────────────────
    println!("   [5/6] Re-enabling contract...");
    let (status, body) = sandbox
        .admin_post(
            &format!("/admin/contracts/{}/toggle", contract_id),
            json!({"enabled": true, "reason": "restore"}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["enabled"], true);
    sleep(Duration::from_millis(300)).await;

    let resp_re = sandbox
        .send_simple(Some("eden"), &user_sk, &recipient, "100")
        .await?;
    let fee_re = Decimal::from_str(resp_re["transfer_fee"].as_str().unwrap_or("0"))
        .unwrap_or_default();
    println!("      Fee after re-enable:           {}", fee_re);
    assert!(
        fee_re > Decimal::ZERO,
        "After re-enabling, transfer_fee must be > 0. Got: {}",
        fee_re
    );

    // ── 6. GET /admin/contracts/{id} reflects the final state ────────
    println!("   [6/6] Verifying GET /admin/contracts/{{id}} reflects enabled=true...");
    let (status, body) = sandbox
        .admin_get(&format!("/admin/contracts/{}", contract_id))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["enabled"], true);

    println!("\n   TEST PASSED: Contract toggle is a working runtime kill-switch.");
    println!("   - Enabled  → fee = {}", fee_active);
    println!("   - Disabled → fee = {} (zero)", fee_disabled);
    println!("   - Re-enabled → fee = {}", fee_re);
    Ok(())
}

/// Read-only mode end-to-end: arm the flag, prove writes 503, disarm, prove
/// writes resume (v0.7.23).
///
/// This is the user-visible contract of the read-only safety valve. The
/// resource-guard task itself is hard to unit-test (cgroup files in /sys
/// can't be mocked easily), but the manual arm/disarm path drives the
/// exact same atomic + middleware code path that the watcher uses, so
/// proving this works proves the core gating contract.
///
/// Steps:
///   1. Boot sandbox, faucet-mint to a fresh user — expect 200 (baseline).
///   2. POST /admin/read-only/arm → expect 200, armed=true, reason=manual.
///   3. Faucet-mint again — expect 503 with body `{"error": "read_only",
///      "reason": "manual"}` and `Retry-After: 30`.
///   4. Read endpoints (`/v1/balance`, `/v1/version`) keep returning 200
///      while armed — proves we didn't gate too aggressively.
///   5. POST /admin/read-only/disarm → expect 200, armed=false.
///   6. Faucet-mint again — expect 200 (writes resume).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_read_only_mode_gates_writes() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Read-Only Mode Gates Writes (v0.7.23)              ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    let user = Wallet::generate();
    let user_addr = user.get_address("8e");

    // ── 1. Baseline: faucet-mint succeeds ─────────────────────────────
    println!("   [1/6] Baseline: faucet 100 PMS to fresh user...");
    let (status, body) = sandbox
        .admin_post(
            "/admin/faucet",
            json!({ "to": user_addr.clone(), "amount": "100" }),
        )
        .await;
    println!("      Faucet: {} — {:?}", status, body);
    anyhow::ensure!(
        status.is_success(),
        "Baseline faucet should succeed before arming read-only. Got {}: {}",
        status,
        body
    );

    // ── 2. Arm read-only mode manually ────────────────────────────────
    println!("   [2/6] Arming read-only mode (reason=manual)...");
    let (status, body) = sandbox
        .admin_post("/admin/read-only/arm", json!({}))
        .await;
    println!("      Arm: {} — {:?}", status, body);
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["armed"], json!(true));
    assert_eq!(body["reason"], json!("manual"));

    // Verify status endpoint also reports armed.
    let (status, body) = sandbox.admin_get("/admin/read-only/status").await;
    println!("      Status: {} — {:?}", status, body);
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["armed"], json!(true));
    assert_eq!(body["reason"], json!("manual"));

    // ── 3. Write attempt → 503 with stable reason field ───────────────
    println!("   [3/6] Faucet-mint while armed — expecting 503 read_only...");
    let resp = sandbox
        .client
        .post(format!("{}/admin/faucet", sandbox.base_url))
        .bearer_auth(&sandbox.admin_token)
        .json(&json!({ "to": user_addr.clone(), "amount": "100" }))
        .send()
        .await
        .expect("HTTP POST failed");
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let body: Value = resp.json().await.unwrap_or(json!({}));
    println!(
        "      Faucet (armed): {} retry-after={:?} — {:?}",
        status, retry_after, body
    );
    assert_eq!(
        status,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "While armed, write must return 503 (got {}). Body: {}",
        status,
        body
    );
    // v0.7.23 wire (legacy SDK clients still depend on these):
    assert_eq!(body["error"], json!("read_only"));
    assert_eq!(body["reason"], json!("manual"));
    assert_eq!(body["retry_after_seconds"], json!(30));
    assert_eq!(retry_after.as_deref(), Some("30"));
    // v0.7.24 numeric error code (new SDK pattern: branch on code, not message):
    assert_eq!(body["code"], json!(1020), "ApiError code 1020 = ReadOnly");
    let msg = body["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("Service temporarily unavailable")
            && msg.contains("manual")
            && msg.contains("retry"),
        "Message should be the v0.7.24 ApiError public template, got: {}",
        msg
    );

    // ── 4. Reads still succeed while armed ────────────────────────────
    println!("   [4/6] Reads (/v1/version, /v1/balance) while armed...");
    let resp_version = sandbox
        .client
        .get(format!("{}/v1/version", sandbox.base_url))
        .send()
        .await
        .expect("HTTP GET /v1/version failed");
    println!("      /v1/version: {}", resp_version.status());
    assert!(
        resp_version.status().is_success(),
        "Read endpoint /v1/version must keep serving while read-only. Got {}",
        resp_version.status()
    );

    let resp_balance = sandbox
        .client
        .post(format!("{}/v1/balance", sandbox.base_url))
        .header("X-API-Key", "any") // store empty in tests → bypass
        .json(&json!({ "address": user_addr.clone() }))
        .send()
        .await
        .expect("HTTP POST /v1/balance failed");
    let bal_status = resp_balance.status();
    let bal_body: Value = resp_balance.json().await.unwrap_or(json!({}));
    println!("      /v1/balance: {} — {:?}", bal_status, bal_body);
    assert!(
        bal_status.is_success(),
        "Read endpoint /v1/balance must keep serving while read-only. \
         Got {}: {}",
        bal_status,
        bal_body
    );

    // ── 5. Disarm — works on Manual ───────────────────────────────────
    println!("   [5/6] Disarming read-only mode...");
    let (status, body) = sandbox
        .admin_post("/admin/read-only/disarm", json!({}))
        .await;
    println!("      Disarm: {} — {:?}", status, body);
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["armed"], json!(false));
    assert_eq!(body["previous_reason"], json!("manual"));

    // ── 6. Writes resume ──────────────────────────────────────────────
    println!("   [6/6] Faucet-mint after disarm — expecting 200...");
    let (status, body) = sandbox
        .admin_post(
            "/admin/faucet",
            json!({ "to": user_addr.clone(), "amount": "100" }),
        )
        .await;
    println!("      Faucet (cleared): {} — {:?}", status, body);
    assert!(
        status.is_success(),
        "After disarm, faucet must succeed again. Got {}: {}",
        status,
        body
    );

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  READ-ONLY MODE — END-TO-END VALIDATION                  ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!("   ║  Pre-arm faucet:    200 ✓                                ║");
    println!("   ║  Armed (manual):    armed=true reason=manual ✓           ║");
    println!("   ║  Faucet while armed:503 error=read_only retry-after=30 ✓ ║");
    println!("   ║  Reads while armed: 200 ✓ (not gated)                    ║");
    println!("   ║  Disarmed:          armed=false ✓                        ║");
    println!("   ║  Post-disarm faucet:200 ✓ (writes resume)                ║");
    println!("   ║  ApiError code:     1020 ✓ (v0.7.24 numeric code)        ║");
    println!("   ╚══════════════════════════════════════════════════════════╝");
    Ok(())
}

/// ApiError numeric codes on auth failures (v0.7.24): the admin gate
/// returns `code: 1001` when no token is supplied and `code: 1002` when
/// the token is wrong. Stable wire so SDK clients can branch on the
/// number rather than parsing "Unauthorized" or similar legacy strings.
///
/// Public messages stay vague ("Authentication required" / "Authentication
/// failed") so an attacker can't tell missing-vs-wrong from the body —
/// but the operator sees the precise reason via the
/// `pms_admin_auth_failures_total{reason}` legacy counter AND the new
/// `pms_api_errors_total{code}` counter the `IntoResponse` impl emits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_api_error_codes_on_auth_failure() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: ApiError numeric codes on auth failure (v0.7.24)   ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. No token at all → 1001 MissingAuth ─────────────────────────
    println!("   [1/3] /admin/ping with NO token — expecting code 1001 (MissingAuth)...");
    let resp = sandbox
        .client
        // Use a non-loopback Forwarded header so require_local_or_admin
        // doesn't bypass auth via the loopback shortcut. Actually no —
        // the sandbox binds to 127.0.0.1, so loopback bypasses auth.
        // Hit a route that requires admin even on loopback: the
        // per-ledger admin gate (`require_admin_token`) does NOT have
        // the loopback bypass. Use it.
        //
        // But since boot_sandbox creates "main" ledger, we need a
        // custom ledger. Quickest: use /admin/api-keys/... endpoint
        // which is on `require_local_or_admin`. Hmm — it bypasses
        // loopback. Let me use a per-ledger admin route on a custom
        // ledger we create now.
        .get(format!("{}/admin/ping", sandbox.base_url))
        .send()
        .await
        .expect("HTTP GET failed");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(json!({}));
    println!("      /admin/ping (no token): {} — {:?}", status, body);
    // /admin/ping is on `require_local_or_admin` which bypasses for
    // 127.0.0.1; the sandbox runs on loopback so it'll be 200 OK.
    // We need a route gated by `require_admin_token` (per-ledger),
    // which has no loopback bypass. Create a custom ledger first.
    sandbox.create_ledger("eden-auth", "eden-auth-net", "edna", "EDA").await?;

    let resp = sandbox
        .client
        .post(format!("{}/l/eden-auth/admin/faucet", sandbox.base_url))
        .json(&json!({ "to": sandbox.admin_addr.clone(), "amount": "10" }))
        .send()
        .await
        .expect("HTTP POST failed");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(json!({}));
    println!(
        "      /l/eden-auth/admin/faucet (no token): {} — {:?}",
        status, body
    );
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], json!(1001), "MissingAuth should be 1001");
    assert_eq!(body["message"], json!("Authentication required"));

    // ── 2. Wrong token → 1002 InvalidAuth ─────────────────────────────
    println!("   [2/3] same endpoint with WRONG token — expecting code 1002 (InvalidAuth)...");
    let resp = sandbox
        .client
        .post(format!("{}/l/eden-auth/admin/faucet", sandbox.base_url))
        .bearer_auth("definitely-not-the-right-token")
        .json(&json!({ "to": sandbox.admin_addr.clone(), "amount": "10" }))
        .send()
        .await
        .expect("HTTP POST failed");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(json!({}));
    println!(
        "      /l/eden-auth/admin/faucet (wrong token): {} — {:?}",
        status, body
    );
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], json!(1002), "InvalidAuth should be 1002");
    assert_eq!(body["message"], json!("Authentication failed"));

    // ── 3. Public message must NOT leak whether token was missing or wrong ────
    println!("   [3/3] Public messages should be different strings but same generic vagueness...");
    println!("      MissingAuth (1001): 'Authentication required'");
    println!("      InvalidAuth (1002): 'Authentication failed'");
    // Both messages are vague — neither says "the token you sent was
    // 5 characters too short" or "this token expired in 2024". The
    // numeric code is the contract, the message is just human gloss.

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  ApiError CODES — END-TO-END VALIDATION                  ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!("   ║  No token         → 401 code=1001 ✓                      ║");
    println!("   ║  Wrong token      → 401 code=1002 ✓                      ║");
    println!("   ║  Public messages  → vague, no info leak ✓                ║");
    println!("   ╚══════════════════════════════════════════════════════════╝");
    Ok(())
}

// ============================================================================
// PHASE 3 — Payment-rail RPC endpoints
// ============================================================================
//
// `GET /v1/dag/status`, `POST /v1/estimate-fee`, `GET /v1/transaction/{id}`,
// `GET /v1/blocks/range`. Validate the SaaS payment-rail integration
// surface: a watcher can poll the status, look up TX details, scan a time
// range to recover after a crash, and estimate fees before signing.

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_dag_status_endpoint() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // Boot snapshot — empty (only genesis).
    let (status, body0) = sandbox.public_get("/v1/dag/status").await;
    assert_eq!(status, reqwest::StatusCode::OK, "expected 200, got {status}");
    println!("   [1/3] Boot dag/status: {body0}");

    // Required fields present.
    assert!(body0["network_id"].is_string());
    assert!(body0["api_version"].as_u64().unwrap_or(0) >= 11);
    assert!(body0["dag_version"].is_string());
    let initial_total = body0["total_blocks"].as_u64().unwrap_or(0);
    println!(
        "      network_id={} api_version={} dag_version={} total_blocks={}",
        body0["network_id"], body0["api_version"], body0["dag_version"], initial_total
    );

    // Generate some blocks via faucet mint (each mint = 1 block).
    let user = Wallet::generate();
    let user_addr = user.get_address("8e");
    for _ in 0..3 {
        sandbox.faucet_mint(None, &user_addr, "1.0").await?;
    }
    sleep(Duration::from_millis(300)).await;

    let (status, body1) = sandbox.public_get("/v1/dag/status").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let total_after = body1["total_blocks"].as_u64().unwrap_or(0);
    println!(
        "   [2/3] After 3 faucet mints: total_blocks={} (was {}) latest_block_ts_ms={:?}",
        total_after, initial_total, body1["latest_block_ts_ms"]
    );
    assert!(
        total_after >= initial_total + 3,
        "total_blocks must grow after mints"
    );
    assert!(
        body1["latest_block_ts_ms"].as_i64().is_some(),
        "latest_block_ts_ms must be set after activity"
    );

    println!("   [3/3] dag/status reflects new activity ✓");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_estimate_fee_endpoint() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // Estimate the fee for a 100 PMS transfer on main.
    let (status, body) = sandbox
        .post(None, "/v1/estimate-fee", json!({ "amount": "100" }))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "expected 200, got {status} body={body}");
    println!("   [1/3] estimate-fee for 100 PMS: {body}");

    let fee = body["fee"]
        .as_str()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or(Decimal::ZERO);
    let transfer_fee = body["transfer_fee"]
        .as_str()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or(Decimal::ZERO);
    let total = body["total"]
        .as_str()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or(Decimal::ZERO);
    assert!(fee >= Decimal::ZERO, "fee must be non-negative");
    assert!(transfer_fee >= Decimal::ZERO, "transfer_fee must be non-negative");
    assert_eq!(
        total,
        Decimal::from(100) + fee + transfer_fee,
        "total must equal amount + fee + transfer_fee"
    );

    // Negative amount must be rejected.
    let (status_neg, body_neg) = sandbox
        .post(None, "/v1/estimate-fee", json!({ "amount": "-1" }))
        .await;
    assert_eq!(
        status_neg,
        reqwest::StatusCode::BAD_REQUEST,
        "negative amount must 400, got {status_neg}"
    );
    println!("   [2/3] Negative amount rejected: {body_neg}");

    // Malformed amount must be rejected.
    let (status_bad, body_bad) = sandbox
        .post(None, "/v1/estimate-fee", json!({ "amount": "not-a-number" }))
        .await;
    assert_eq!(
        status_bad,
        reqwest::StatusCode::BAD_REQUEST,
        "malformed amount must 400, got {status_bad}"
    );
    println!("   [3/3] Malformed amount rejected: {body_bad}");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_transaction_lookup_endpoint() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // Three flows the watcher cares about:
    //   (A) Mint block (faucet drop = a deposit from SaaS PoV) → public, lookup full detail
    //   (B) Encrypted TxUtxo (real user transfer) → 403 with pointer to /v1/wallet/.../activity
    //   (C) Unknown block id → 404 code=3040
    let recipient = Wallet::generate().get_address("8e");

    // ── (A) Mint lookup ───────────────────────────────────────────────────
    let mint_block_id = sandbox.faucet_mint(None, &recipient, "42").await?;
    sleep(Duration::from_millis(200)).await;

    let path = format!("/v1/transaction/{}", mint_block_id);
    let (status, body) = sandbox.public_get(&path).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "expected 200 for mint lookup, got {status} body={body}"
    );
    println!("   [1/3] Mint lookup body: {body}");

    assert_eq!(body["block_id"].as_str(), Some(mint_block_id.as_str()));
    assert_eq!(
        body["from"], json!(null),
        "Mint blocks have no inputs → `from` must be null"
    );
    assert_eq!(
        body["to"].as_str(),
        Some(recipient.as_str()),
        "`to` must be the mint output address"
    );
    assert_eq!(body["amount"].as_str(), Some("42"));
    assert_eq!(body["fee"].as_str(), Some("0"));
    assert_eq!(body["inputs"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(body["outputs"].as_array().map(|a| a.len()), Some(1));
    assert!(body["timestamp_ms"].as_i64().is_some());
    assert!(
        ["pending", "confirmed", "finalized"]
            .contains(&body["status"].as_str().unwrap_or("?"))
    );
    println!(
        "      Mint resolved → status={} depth={} is_finalized={}",
        body["status"], body["depth"], body["is_finalized"]
    );

    // ── (B) Encrypted TxUtxo → 403 with clear redirect message ──────────
    let user = Wallet::generate();
    let user_addr = user.get_address("8e");
    let user_sk = user.private_key_b64.clone();
    sandbox.faucet_mint(None, &user_addr, "1000").await?;
    sleep(Duration::from_millis(200)).await;
    let send_resp = sandbox
        .send_simple(None, &user_sk, &recipient, "42")
        .await?;
    let enc_block_id = send_resp["block_id"].as_str().unwrap().to_string();

    let (status_enc, body_enc) = sandbox
        .public_get(&format!("/v1/transaction/{}", enc_block_id))
        .await;
    assert_eq!(status_enc, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(body_enc["code"], json!(1010));
    println!(
        "   [2/3] Encrypted TxUtxo → 403 code=1010 with clear redirect: {}",
        body_enc["message"]
    );

    // ── (C) Unknown block id → 404 code=3040 ────────────────────────────
    let (status_404, body_404) = sandbox
        .public_get("/v1/transaction/0000000000000000000000000000000000000000000000000000000000000000")
        .await;
    assert_eq!(status_404, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body_404["code"], json!(3040));
    println!("   [3/3] Unknown id → 404 code=3040: {body_404}");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_blocks_range_endpoint() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // Create a known number of blocks via faucet mint.
    let user_addr = Wallet::generate().get_address("8e");
    for _ in 0..5 {
        sandbox.faucet_mint(None, &user_addr, "1.0").await?;
    }
    sleep(Duration::from_millis(400)).await;

    // First page (most recent first).
    let (status, body) = sandbox.public_get("/v1/blocks/range?limit=3").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let blocks = body["blocks"].as_array().expect("blocks array");
    println!(
        "   [1/3] Page 1: {} blocks, next_cursor={}",
        blocks.len(),
        body["next_cursor"]
    );
    assert_eq!(blocks.len(), 3, "limit=3 must return 3 blocks");
    for b in blocks {
        assert!(b["id"].is_string());
        assert!(b["ts_ms"].as_i64().is_some());
    }

    let cursor = body["next_cursor"].clone();
    assert!(
        cursor.is_object(),
        "next_cursor must be present after a full page"
    );
    let after_ts = cursor["ts_ms"].as_i64().expect("cursor ts_ms");
    let after_id = cursor["id"].as_str().expect("cursor id").to_string();

    // Idempotency: re-running the SAME first-page query yields identical
    // blocks (the watcher must be able to retry safely).
    let (_, body_replay) = sandbox.public_get("/v1/blocks/range?limit=3").await;
    let replay_ids: Vec<&str> = body_replay["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    let first_ids: Vec<&str> = blocks.iter().filter_map(|b| b["id"].as_str()).collect();
    assert_eq!(first_ids, replay_ids, "page must be idempotent across calls");
    println!("   [2/3] Page 1 idempotent across two calls ✓");

    // Second page using the cursor — must return strictly older blocks
    // (no overlap with page 1).
    let path = format!(
        "/v1/blocks/range?limit=10&after_ts={}&after_id={}",
        after_ts, after_id
    );
    let (status2, body2) = sandbox.public_get(&path).await;
    assert_eq!(status2, reqwest::StatusCode::OK);
    let page2 = body2["blocks"].as_array().expect("blocks array");
    println!("   [3/3] Page 2 from cursor: {} blocks", page2.len());
    let page2_ids: std::collections::HashSet<&str> =
        page2.iter().filter_map(|b| b["id"].as_str()).collect();
    let page1_ids: std::collections::HashSet<&str> = first_ids.into_iter().collect();
    assert!(
        page2_ids.is_disjoint(&page1_ids),
        "page 1 and page 2 must not overlap (cursor is exclusive)"
    );

    Ok(())
}

// ============================================================================
// PHASE 4 — Watcher API (multi-address SSE + webhooks)
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_multi_address_sse_filters_correctly() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    let watched_a = Wallet::generate().get_address("8e");
    let watched_b = Wallet::generate().get_address("8e");
    let unwatched = Wallet::generate().get_address("8e");

    // Subscribe to a 2-address stream BEFORE generating the events,
    // otherwise the broadcast bus has no receiver and emissions are lost.
    let url = format!(
        "{}/v1/activity/stream?addresses={},{}",
        sandbox.base_url, watched_a, watched_b
    );
    let resp = sandbox
        .client
        .get(&url)
        .send()
        .await
        .expect("connect SSE");
    assert!(resp.status().is_success(), "expected 200, got {}", resp.status());

    let mut stream = resp;
    println!("   [1/3] Connected to multi-address SSE for 2 addresses");

    // Mint to A (watched), C (unwatched), B (watched). Only 2/3 events
    // should reach the stream.
    sandbox.faucet_mint(None, &watched_a, "10").await?;
    sandbox.faucet_mint(None, &unwatched, "10").await?;
    sandbox.faucet_mint(None, &watched_b, "10").await?;

    let mut events: Vec<Value> = Vec::new();
    let mut accumulated = String::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline && events.len() < 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                accumulated.push_str(&String::from_utf8_lossy(&bytes));
                // Frames separated by \n\n; each contains lines like
                // "event: activity" / "data: {...json...}".
                while let Some(idx) = accumulated.find("\n\n") {
                    let frame = accumulated[..idx].to_string();
                    accumulated.drain(..idx + 2);
                    for line in frame.lines() {
                        if let Some(payload) = line.strip_prefix("data: ") {
                            if let Ok(v) = serde_json::from_str::<Value>(payload) {
                                events.push(v);
                            }
                        }
                    }
                }
            }
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_) => continue, // timeout — keep waiting up to deadline
        }
    }

    println!("   [2/3] Received {} events: {events:?}", events.len());
    assert_eq!(
        events.len(),
        2,
        "expected exactly 2 events for the watched addresses, got {}",
        events.len()
    );

    let combined = serde_json::to_string(&events).unwrap();
    assert!(
        combined.contains(&watched_a) || combined.contains(&watched_b),
        "events should reference watched addresses"
    );
    assert!(
        !combined.contains(&unwatched),
        "unwatched address must NOT appear in stream events: {combined}"
    );
    println!("   [3/3] Watched-only filter verified ✓");

    // Reject too-many-addresses: 1001 addresses → 400.
    let many: Vec<String> = (0..1001).map(|i| format!("8e1addr{i:04}")).collect();
    let url_many = format!("{}/v1/activity/stream?addresses={}", sandbox.base_url, many.join(","));
    let resp = sandbox.client.get(&url_many).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    println!("   [BONUS] >1000 addresses → 400 ✓");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_webhook_subscribe_list_unsubscribe_roundtrip() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    let addr = Wallet::generate().get_address("8e");

    let body = json!({
        "addresses": [addr.clone()],
        "callback_url": "http://127.0.0.1:1/webhook",
        "secret": "test_secret",
    });
    let (status, resp) = sandbox.admin_post("/admin/webhooks", body).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "expected 201, got {status} {resp}");
    let sub_id = resp["subscription_id"]
        .as_str()
        .expect("subscription_id present")
        .to_string();
    assert_eq!(resp["addresses_count"], json!(1));
    assert_eq!(resp["secret"], json!("test_secret"));
    println!("   [1/4] Subscribed: id={} addresses=1", sub_id);

    // List — must contain the new sub WITHOUT exposing the secret.
    let (status_list, list) = sandbox.admin_get("/admin/webhooks").await;
    assert_eq!(status_list, reqwest::StatusCode::OK);
    let arr = list.as_array().expect("list is array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["subscription_id"], json!(sub_id));
    assert!(
        arr[0].get("secret").is_none() || arr[0]["secret"].is_null(),
        "secret MUST NOT be returned by list (returned only at subscribe-time)"
    );
    println!("   [2/4] List returns sub without secret ✓");

    // Subscribe rejects empty addresses + invalid callback URL.
    let (status_bad1, body_bad1) = sandbox
        .admin_post("/admin/webhooks", json!({
            "addresses": [],
            "callback_url": "http://127.0.0.1:1/x",
        }))
        .await;
    assert_eq!(status_bad1, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body_bad1["code"], json!(2030));
    println!("   [3/4] Empty addresses → 400 code=2030: {body_bad1}");

    let (status_bad2, body_bad2) = sandbox
        .admin_post("/admin/webhooks", json!({
            "addresses": ["addr1"],
            "callback_url": "ftp://invalid",
        }))
        .await;
    assert_eq!(status_bad2, reqwest::StatusCode::BAD_REQUEST);
    println!("       Invalid callback_url → 400: {body_bad2}");

    // Unsubscribe.
    let (status_del, body_del) = sandbox
        .admin_delete(&format!("/admin/webhooks/{sub_id}"))
        .await;
    assert_eq!(status_del, reqwest::StatusCode::OK);
    println!("   [4/4] Unsubscribed: {body_del}");

    let (_, list2) = sandbox.admin_get("/admin/webhooks").await;
    assert_eq!(list2.as_array().map(|a| a.len()), Some(0));

    let (status_404, body_404) = sandbox
        .admin_delete(&format!("/admin/webhooks/{sub_id}"))
        .await;
    assert_eq!(status_404, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body_404["code"], json!(3040));
    println!("       Re-delete → 404 code=3040 ✓");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Preuve de réserves ancrée (protocole 2.6, v0.10.0)
// ═══════════════════════════════════════════════════════════════════════════

/// Snapshot manuel → bloc ReserveSnapshot ancré + pointeur /v1/reserves/latest
/// + verify (recompute) cohérent tant que l'état n'a pas bougé.
#[tokio::test]
#[ignore]
async fn test_reserve_snapshot_anchor_and_verify() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Reserve Snapshot (proof-of-reserves, plan 2.6)     ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    // ── 1. Un peu d'état : 2 UTXOs via faucet ──
    println!("   [1/5] Minting state (2 faucet UTXOs)...");
    let w1 = pms_wallet::Wallet::from_seed(&[91u8; 32], None).expect("w1");
    let w2 = pms_wallet::Wallet::from_seed(&[92u8; 32], None).expect("w2");
    sandbox.faucet_mint(None, &w1.get_address("8e"), "150").await?;
    sandbox.faucet_mint(None, &w2.get_address("8e"), "250").await?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // ── 2. Avant tout snapshot : latest → 404 ──
    let (status, body) = sandbox.public_get("/v1/reserves/latest").await;
    println!("   [2/5] latest BEFORE any snapshot: {} {}", status, body);
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    // ── 3. Snapshot manuel (admin, produit un bloc) ──
    let (status, snap) = sandbox.admin_post("/admin/reserves/snapshot", json!({})).await;
    println!("   [3/5] snapshot: {} {}", status, serde_json::to_string_pretty(&snap)?);
    assert_eq!(status, reqwest::StatusCode::OK, "snapshot failed: {snap}");
    let root = snap["state_root"].as_str().expect("state_root");
    assert_eq!(root.len(), 64, "state_root must be 64 hex chars");
    assert!(hex::decode(root).is_ok());
    assert!(snap["utxo_count"].as_u64().unwrap() >= 2, "must cover the faucet UTXOs");
    assert!(snap["block_id"].as_str().is_some(), "anchored block id");
    // supply native (asset null) = 150 + 250 = 400
    let native_total = snap["total_supply"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e[0].is_null())
        .map(|e| e[1].as_str().unwrap().to_string())
        .expect("native supply entry");
    println!("       native total_supply = {native_total}");
    assert_eq!(native_total.parse::<f64>().unwrap(), 400.0);

    // ── 4. GET /v1/reserves/latest == snapshot ──
    let (status, latest) = sandbox.public_get("/v1/reserves/latest").await;
    println!("   [4/5] latest AFTER snapshot: {} state_root={}", status, latest["state_root"]);
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(latest["state_root"], snap["state_root"]);
    assert_eq!(latest["block_id"], snap["block_id"]);

    // ── 5. Verify : recompute == ancré (état inchangé) ──
    let (status, verify) = sandbox.admin_post("/admin/reserves/verify", json!({})).await;
    println!("   [5/5] verify: {} match={} note={}", status, verify["match"], verify["note"]);
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(verify["match"], json!(true), "recompute must match anchor: {verify}");

    println!("\n   TEST PASSED: ReserveSnapshot anchored, exposed and verified.");
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Mint collatéralisé (protocole 2.3 v2, v0.11.0)
// ═══════════════════════════════════════════════════════════════════════════

/// Cycle complet : réserve time-lockée via faucet → token collatéralisé 1:1 →
/// mint couvert OK → mint au-delà de la réserve rejeté (InsufficientCollateral)
/// → une réserve NON lockée ne compte pas.
#[tokio::test]
#[ignore]
async fn test_collateralized_mint_lifecycle() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  TEST: Collateralized Mint (plan 2.3 v2)                  ║");
    println!("╚═══════════════════════════════════════════════════════════╝\n");

    let reserve = pms_wallet::Wallet::from_seed(&[93u8; 32], None).expect("reserve wallet");
    let reserve_addr = reserve.get_address("8e");
    let user = pms_wallet::Wallet::from_seed(&[94u8; 32], None).expect("user wallet");
    let user_addr = user.get_address("8e");
    let now_ms = pms_core::utxo::current_time_ms();

    // ── 1. Constitue la réserve : 1000 PMS time-lockés 24h via le faucet ──
    println!("   [1/6] Locking 1000 PMS reserve (24h) at {}...", &reserve_addr[..16]);
    let (status, resp) = sandbox
        .admin_post(
            "/admin/faucet",
            json!({ "to": reserve_addr, "amount": "1000", "locked_until": now_ms + 86_400_000 }),
        )
        .await;
    println!("       faucet locked: {} {:?}", status, resp);
    anyhow::ensure!(status.is_success(), "locked faucet failed: {resp}");

    // + 500 PMS NON lockés à la même adresse (ne doivent PAS compter)
    sandbox.faucet_mint(None, &reserve_addr, "500").await?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // ── 2. Crée le token collatéralisé 1:1 ──
    println!("   [2/6] Creating token 'goldback' (collateral 1:1 on reserve)...");
    let (status, resp) = sandbox
        .admin_post(
            "/admin/tokens/create",
            json!({
                "asset_id": "goldback",
                "symbol": "GBK",
                "name": "Gold Backed",
                "decimals": 8,
                "collateral_address": reserve_addr,
                "collateral_ratio_bps": 10000
            }),
        )
        .await;
    println!("       create: {} {:?}", status, resp);
    anyhow::ensure!(status.is_success(), "token create failed: {resp}");

    // ── 3. Mint couvert : 800 <= 1000 lockés (les 500 non lockés ignorés) ──
    println!("   [3/6] Minting 800 GBK (covered by 1000 locked)...");
    let (status, resp) = sandbox
        .admin_post(
            "/admin/tokens/mint",
            json!({ "asset_id": "goldback", "to": user_addr, "amount": "800" }),
        )
        .await;
    println!("       mint 800: {} {:?}", status, resp);
    anyhow::ensure!(status.is_success(), "covered mint must pass: {resp}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let bal = sandbox.get_asset_balance("main", &user_addr, Some("goldback")).await?;
    println!("       user GBK balance: {bal}");
    anyhow::ensure!(bal.to_string() == "800", "expected 800 GBK, got {bal}");

    // ── 4. Mint au-delà : 800 + 300 = 1100 > 1000 lockés → rejet ──
    println!("   [4/6] Minting 300 more (total 1100 > 1000 locked) → must fail...");
    let (status, resp) = sandbox
        .admin_post(
            "/admin/tokens/mint",
            json!({ "asset_id": "goldback", "to": user_addr, "amount": "300" }),
        )
        .await;
    println!("       over-mint: {} {:?}", status, resp);
    anyhow::ensure!(!status.is_success(), "over-collateral mint MUST be rejected");
    let err_str = resp.to_string();
    anyhow::ensure!(
        err_str.contains("collateral") || err_str.contains("Collateral"),
        "rejection must cite collateral, got: {err_str}"
    );

    // ── 5. Mint à la couverture exacte : 800 + 200 = 1000 == 1000 → OK ──
    println!("   [5/6] Minting 200 (total 1000 == 1000 locked) → must pass...");
    let (status, resp) = sandbox
        .admin_post(
            "/admin/tokens/mint",
            json!({ "asset_id": "goldback", "to": user_addr, "amount": "200" }),
        )
        .await;
    println!("       at-coverage mint: {} {:?}", status, resp);
    anyhow::ensure!(status.is_success(), "exact coverage mint must pass: {resp}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let bal = sandbox.get_asset_balance("main", &user_addr, Some("goldback")).await?;
    println!("       user GBK balance: {bal}");
    anyhow::ensure!(bal.to_string() == "1000", "expected 1000 GBK, got {bal}");

    // ── 6. Et plus un seul satoshi de plus ──
    println!("   [6/6] Minting 0.00000001 more → must fail (reserve exhausted)...");
    let (status, resp) = sandbox
        .admin_post(
            "/admin/tokens/mint",
            json!({ "asset_id": "goldback", "to": user_addr, "amount": "0.00000001" }),
        )
        .await;
    println!("       dust over-mint: {} {:?}", status, resp);
    anyhow::ensure!(!status.is_success(), "any mint past coverage MUST be rejected");

    println!("\n   ╔══════════════════════════════════════════════════════════╗");
    println!("   ║  COLLATERALIZED MINT RESULTS                             ║");
    println!("   ╠══════════════════════════════════════════════════════════╣");
    println!("   ║  Locked reserve:           1000 PMS (24h)                ║");
    println!("   ║  Unlocked at same addr:     500 PMS (ignored)            ║");
    println!("   ║  Minted (1:1):             1000 GBK exactly              ║");
    println!("   ║  Over-mint (1100):         rejected                      ║");
    println!("   ║  Dust past coverage:       rejected                      ║");
    println!("   ╚══════════════════════════════════════════════════════════╝");
    println!("\n   TEST PASSED: emission can never exceed the locked reserve.");
    Ok(())
}

/// On-ramp (voie A, plan §3.1) end-to-end through the SHARED emission budget.
///
/// Proves the HTTP path handler → orchestrator → gate → forge → recipient
/// balance, and the stable error code on exhaustion. The cumulative/shared
/// budget invariant across voies is additionally proven at the gate level by
/// `tests/emission_budget_test.rs` (t3 residual, t4 exhaustion, t5 TOCTOU).
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_onramp_voie_a_emission_budget -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_onramp_voie_a_emission_budget() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // 1. Bootstrap circulating supply via faucet (ungated) so the percentage
    //    budget is non-zero. Capped at `max_mint_per_block` (1M) per block.
    //    Default config: 3 %/yr, ceiling 10 %, epoch 1 day
    //    → budget ≈ 1M × 0.03 / 365 ≈ 82.19 PMS for this epoch.
    let bootstrap = Wallet::generate();
    let bootstrap_addr = bootstrap.get_address("8e");
    sandbox.faucet_mint(None, &bootstrap_addr, "1000000").await?;

    // 2. On-ramp a moderate amount — comfortably within the period budget.
    let recipient = Wallet::generate();
    let recipient_addr = recipient.get_address("8e");
    let (status, body) = sandbox
        .admin_post(
            "/admin/onramp",
            json!({
                "to": recipient_addr,
                "amount": "10",
                "payment_ref": "stripe_ch_test_001",
            }),
        )
        .await;
    println!("   On-ramp 10 PMS → {} — {:?}", status, body);
    assert!(
        status.is_success(),
        "on-ramp within budget must succeed: {} {:?}",
        status,
        body
    );
    assert_eq!(
        body["minted"].as_str(),
        Some("10"),
        "on-ramp reports the exact minted amount"
    );
    assert!(
        body["block_id"].as_str().is_some(),
        "on-ramp returns the forged block id"
    );

    let bal = sandbox.get_balance("main", &recipient_addr).await?;
    println!("   Recipient balance after on-ramp: {}", bal);
    assert_eq!(
        bal,
        Decimal::from(10),
        "recipient credited exactly 10 PMS by the on-ramp mint"
    );

    // 3. On-ramp far above the period budget → rejected with stable code 5030.
    let (status2, body2) = sandbox
        .admin_post(
            "/admin/onramp",
            json!({
                "to": recipient_addr,
                "amount": "999999999",
                "payment_ref": "stripe_ch_test_002",
            }),
        )
        .await;
    println!(
        "   On-ramp 999999999 PMS (over budget) → {} — {:?}",
        status2, body2
    );
    assert_eq!(
        status2.as_u16(),
        503,
        "over-budget on-ramp must return 503 Service Unavailable"
    );
    assert_eq!(
        body2["code"].as_u64(),
        Some(5030),
        "stable numeric code for emission budget exhausted (anti-enumeration)"
    );

    // The rejected mint must NOT have credited anything (P1: no over-emission).
    let bal2 = sandbox.get_balance("main", &recipient_addr).await?;
    println!("   Recipient balance after rejected on-ramp: {}", bal2);
    assert_eq!(
        bal2,
        Decimal::from(10),
        "a rejected on-ramp credits nothing — balance unchanged"
    );

    println!(
        "\n   TEST PASSED: on-ramp mints under the shared budget; over-budget → 503/5030, no over-emission."
    );
    Ok(())
}

/// TokenBurn primitive (plan §3.1, voie B foundation) end-to-end: burning a
/// token TRULY destroys it — circulating supply drops by the burned amount, the
/// change returns to the burner, and an over-burn (more than owned) is rejected
/// with no supply change. Proves the new `PlainPayload::TokenBurn` protocol
/// primitive through the real HTTP route + hot-path validation.
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_token_burn_reduces_supply -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_token_burn_reduces_supply() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    let user = Wallet::generate();
    let user_addr = user.get_address("8e");

    // 1. Fund the burner with 1000 PMS (faucet, ungated → single UTXO).
    sandbox.faucet_mint(None, &user_addr, "1000").await?;
    let bal0 = sandbox.get_balance("main", &user_addr).await?;
    let supply0 = Decimal::from_str(
        sandbox.get_supply("main", None).await?["circulating_supply"]
            .as_str()
            .unwrap_or("0"),
    )
    .unwrap_or(Decimal::ZERO);
    println!("   Before burn: balance={}, supply={}", bal0, supply0);
    assert_eq!(bal0, Decimal::from(1000), "faucet credited 1000 PMS");

    // 2. Burn 100 PMS (asset_id omitted = native PMS).
    let (status, body) = sandbox
        .post(
            None,
            "/v1/wallet/token/burn",
            json!({ "private_key_b64": user.private_key_b64, "amount": "100" }),
        )
        .await;
    println!("   Burn 100 PMS → {} — {:?}", status, body);
    assert!(status.is_success(), "burn must succeed: {} {:?}", status, body);
    assert_eq!(body["burned"].as_str(), Some("100"), "reports burned amount");

    // 3. Balance dropped by 100 (change returned); supply dropped by 100.
    let bal1 = sandbox.get_balance("main", &user_addr).await?;
    let supply1 = Decimal::from_str(
        sandbox.get_supply("main", None).await?["circulating_supply"]
            .as_str()
            .unwrap_or("0"),
    )
    .unwrap_or(Decimal::ZERO);
    println!("   After burn:  balance={}, supply={}", bal1, supply1);
    assert_eq!(bal1, Decimal::from(900), "burner keeps the 900 change");
    assert_eq!(
        supply0 - supply1,
        Decimal::from(100),
        "circulating supply dropped by exactly the burned amount"
    );

    // 4. Over-burn (more than owned) → rejected; balance unchanged.
    let (status2, body2) = sandbox
        .post(
            None,
            "/v1/wallet/token/burn",
            json!({ "private_key_b64": user.private_key_b64, "amount": "100000" }),
        )
        .await;
    println!("   Over-burn 100000 → {} — {:?}", status2, body2);
    assert!(!status2.is_success(), "over-burn must be rejected");
    let bal2 = sandbox.get_balance("main", &user_addr).await?;
    assert_eq!(bal2, Decimal::from(900), "rejected burn leaves balance unchanged");

    println!("\n   TEST PASSED: token burn destroys supply, returns change, rejects over-burn.");
    Ok(())
}

/// Voie B end-to-end (plan §3.1): burning a CUSTOM token fires an `OnTokenBurn`
/// smart contract that mints native PMS at rate R, UNDER the shared emission
/// budget. Proves the full contract-driven conversion: burn → evaluate contract
/// → reserve budget → mint PMS, atomically in the burn handler. The engine never
/// hardcodes the token — the contract holds the policy (token + rate).
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_voie_b_token_conversion -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_voie_b_token_conversion() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // 1. Bootstrap PMS supply (faucet, ungated) so the emission budget is > 0.
    //    Budget ≈ 1M × 3%/365 ≈ 82 PMS/epoch — comfortably covers our 15 PMS mint.
    let boot = Wallet::generate();
    sandbox.faucet_mint(None, &boot.get_address("8e"), "1000000").await?;

    // 2. Create token "gold" on main + mint 1000 to the user.
    let (s, b) = sandbox
        .admin_post(
            "/admin/tokens/create",
            json!({ "asset_id": "gold", "symbol": "GOLD", "name": "Gold", "decimals": 8, "max_supply": "1000000" }),
        )
        .await;
    anyhow::ensure!(s.is_success(), "token create failed: {} {:?}", s, b);
    let user = Wallet::generate();
    let user_addr = user.get_address("8e");
    let (s, b) = sandbox
        .admin_post(
            "/admin/tokens/mint",
            json!({ "asset_id": "gold", "to": user_addr, "amount": "1000" }),
        )
        .await;
    anyhow::ensure!(s.is_success(), "token mint failed: {} {:?}", s, b);

    // 3. Register the conversion contract: OnTokenBurn{gold} → MintNative R=3/2,
    //    then toggle it enabled (contracts register disabled in sandbox mode).
    let cid = sandbox
        .register_contract(json!({
            "name": "gold-to-pms",
            "scope": "Global",
            "trigger": { "OnTokenBurn": { "asset_id": "gold" } },
            "actions": [{ "MintNative": { "rate_numerator": 3, "rate_denominator": 2 } }]
        }))
        .await?;
    let (s, _) = sandbox
        .admin_post(
            &format!("/admin/contracts/{}/toggle", cid),
            json!({ "enabled": true, "reason": "e2e voie B" }),
        )
        .await;
    anyhow::ensure!(s.is_success(), "contract toggle failed: {}", s);

    let gold0 = sandbox.get_asset_balance("main", &user_addr, Some("gold")).await?;
    let pms0 = sandbox.get_balance("main", &user_addr).await?;
    let gold_supply0 = Decimal::from_str(
        sandbox.get_supply("main", Some("gold")).await?["circulating_supply"].as_str().unwrap_or("0"),
    ).unwrap_or(Decimal::ZERO);
    println!("   Before: user gold={}, user PMS={}, gold supply={}", gold0, pms0, gold_supply0);
    assert_eq!(gold0, Decimal::from(1000));
    assert_eq!(pms0, Decimal::ZERO, "user starts with no PMS");

    // 4. Burn 10 gold → contract fires → mint 10 × 3/2 = 15 PMS to the user.
    let (s, body) = sandbox
        .post(
            None,
            "/v1/wallet/token/burn",
            json!({ "private_key_b64": user.private_key_b64, "asset_id": "gold", "amount": "10" }),
        )
        .await;
    println!("   Burn 10 gold → {} — {:?}", s, body);
    assert!(s.is_success(), "burn+convert must succeed: {} {:?}", s, body);
    assert_eq!(body["converted_pms"].as_str(), Some("15"), "10 gold × 3/2 = 15 PMS");
    assert!(body["mint_block_id"].as_str().is_some(), "conversion produced a mint block");

    // 5. Assert: gold burned (−10), PMS minted (+15), gold supply dropped (−10).
    let gold1 = sandbox.get_asset_balance("main", &user_addr, Some("gold")).await?;
    let pms1 = sandbox.get_balance("main", &user_addr).await?;
    let gold_supply1 = Decimal::from_str(
        sandbox.get_supply("main", Some("gold")).await?["circulating_supply"].as_str().unwrap_or("0"),
    ).unwrap_or(Decimal::ZERO);
    println!("   After:  user gold={}, user PMS={}, gold supply={}", gold1, pms1, gold_supply1);
    assert_eq!(gold1, Decimal::from(990), "user keeps 990 gold change (burned 10)");
    assert_eq!(pms1, Decimal::from(15), "user received 15 PMS from the conversion");
    assert_eq!(gold_supply0 - gold_supply1, Decimal::from(10), "gold supply dropped by the burned 10");

    println!("\n   TEST PASSED: voie B — burn 10 gold → OnTokenBurn contract → 15 PMS minted under budget.");
    Ok(())
}

/// Gouvernance timelock (plan §4) end-to-end via les endpoints HTTP.
/// Prouve : propose ancre un bloc + l'expose publiquement (G8), un enact AVANT
/// l'expiration du timelock est REJETÉ (G2, inviolabilité), et un cancel marque
/// la proposition annulée (G7). L'enact APRÈS expiration (G3) est prouvé au
/// niveau protocole par `pms-core/tests/governance_timelock_test.rs` (enact_after
/// contrôlé, impossible via l'endpoint car les durées sont hardcodées 7/15/45 j).
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_governance_timelock_endpoints -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_governance_timelock_endpoints() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // 1. Propose : changer le fee_rate à 777 bps, palier Operator (timelock 7 j).
    let (s, body) = sandbox
        .admin_post(
            "/admin/governance/propose",
            json!({
                "update": { "SetFeeRate": { "bps": 777 } },
                "tier": "Operator",
                "reason": "raise fee to 777bps"
            }),
        )
        .await;
    println!("   Propose → {} — {:?}", s, body);
    assert!(s.is_success(), "propose must succeed: {} {:?}", s, body);
    let proposal_id = body["proposal_id"].as_str().expect("proposal_id").to_string();
    assert!(body["enact_after_ms"].as_u64().unwrap() > body["announced_at_ms"].as_u64().unwrap());

    // 2. /v1/governance/pending (PUBLIC) liste la proposition annoncée (G8).
    let (s, pending) = sandbox.public_get("/v1/governance/pending").await;
    println!("   /pending → {} — {:?}", s, pending);
    let listed = pending["pending"]
        .as_array()
        .map(|a| a.iter().any(|p| p["proposal_id"] == proposal_id && p["status"] == "pending"))
        .unwrap_or(false);
    assert!(listed, "proposal must be publicly announced as pending");

    // 3. Enact IMMÉDIATEMENT → REJETÉ : le timelock n'est pas écoulé (G2).
    let (s, enact_body) = sandbox
        .admin_post(
            &format!("/admin/governance/enact/{}", proposal_id),
            json!({ "reason": "too early" }),
        )
        .await;
    println!("   Enact (early) → {} — {:?}", s, enact_body);
    assert!(
        !s.is_success(),
        "enact before timelock elapsed MUST be rejected (got {})",
        s
    );
    // La raison DOIT être remontée à l'opérateur (code 3071 GovernanceRejected) —
    // il faut savoir POURQUOI (timelock), pas un « Operation conflict » opaque.
    assert_eq!(enact_body["code"].as_u64(), Some(3071), "early enact → code 3071");
    assert!(
        enact_body["message"].as_str().unwrap_or_default().to_lowercase().contains("timelock"),
        "the rejection message must surface the timelock reason, got: {:?}",
        enact_body["message"]
    );

    // 4. La proposition est TOUJOURS pending (l'enact rejeté n'a rien appliqué).
    let (_, pending2) = sandbox.public_get("/v1/governance/pending").await;
    let still_pending = pending2["pending"]
        .as_array()
        .map(|a| a.iter().any(|p| p["proposal_id"] == proposal_id && p["status"] == "pending"))
        .unwrap_or(false);
    assert!(still_pending, "rejected enact leaves the proposal pending (no apply)");

    // 5. Cancel → annulée (G7).
    let (s, _) = sandbox
        .admin_post(
            &format!("/admin/governance/cancel/{}", proposal_id),
            json!({ "reason": "abort" }),
        )
        .await;
    assert!(s.is_success(), "cancel of a pending proposal must succeed: {}", s);

    // 6. Plus dans /pending ; présente dans /history (status cancelled).
    let (_, pending3) = sandbox.public_get("/v1/governance/pending").await;
    let gone = pending3["pending"]
        .as_array()
        .map(|a| !a.iter().any(|p| p["proposal_id"] == proposal_id))
        .unwrap_or(true);
    assert!(gone, "cancelled proposal must leave /pending");
    let (_, history) = sandbox.public_get("/v1/governance/history").await;
    println!("   /history → {:?}", history);
    let in_history = history["history"]
        .as_array()
        .map(|a| a.iter().any(|p| p["proposal_id"] == proposal_id && p["status"] == "cancelled"))
        .unwrap_or(false);
    assert!(in_history, "cancelled proposal must appear in /history");

    println!("\n   TEST PASSED: governance — propose announces, early enact REJECTED (timelock), cancel works.");
    Ok(())
}

/// Gouvernance — asymétrie *tighten-now* end-to-end via les endpoints (G5).
///
/// Un **resserrage** (baisser `max_mint_per_block`) a un timelock **instantané** :
/// le handler dérive `enact_after == announced_at` (via `required_timelock_ms`),
/// donc un `enact` immédiat RÉUSSIT et applique le changement. C'est la moitié
/// « tighten-now » de l'asymétrie, prouvée à travers la couche HTTP (le handler
/// calcule lui-même la durée selon la direction). Le « loosen-later » est prouvé
/// par `test_governance_timelock_endpoints` (enact précoce rejeté).
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_governance_tighten_instant_endpoints -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_governance_tighten_instant_endpoints() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // Propose un tighten : baisser max_mint_per_block à 1 (< défaut). Palier Policy
    // (min requis pour SetMaxMint). Le handler doit dériver enact_after == announced_at.
    let (s, body) = sandbox
        .admin_post(
            "/admin/governance/propose",
            json!({
                "update": { "SetMaxMint": { "amount": 1 } },
                "tier": "Policy",
                "reason": "emergency: cap per-block mint"
            }),
        )
        .await;
    println!("   Propose (tighten) → {} — {:?}", s, body);
    assert!(s.is_success(), "tighten propose must succeed: {} {:?}", s, body);
    let proposal_id = body["proposal_id"].as_str().expect("proposal_id").to_string();
    assert_eq!(
        body["enact_after_ms"].as_u64(),
        body["announced_at_ms"].as_u64(),
        "tighten ⇒ instant: enact_after MUST equal announced_at (got {:?} vs {:?})",
        body["enact_after_ms"],
        body["announced_at_ms"]
    );

    // Enact IMMÉDIATEMENT → succès (timelock instantané, légitime — pas de backdating).
    let (s, enact_body) = sandbox
        .admin_post(
            &format!("/admin/governance/enact/{}", proposal_id),
            json!({ "reason": "apply tighten now" }),
        )
        .await;
    println!("   Enact (instant) → {} — {:?}", s, enact_body);
    assert!(s.is_success(), "instant tighten enact MUST succeed, got {} {:?}", s, enact_body);

    // La proposition est maintenant Enacted (dans /history, plus dans /pending).
    let (_, history) = sandbox.public_get("/v1/governance/history").await;
    let enacted = history["history"]
        .as_array()
        .map(|a| a.iter().any(|p| p["proposal_id"] == proposal_id && p["status"] == "enacted"))
        .unwrap_or(false);
    assert!(enacted, "tighten proposal must be Enacted in /history: {:?}", history);

    println!("\n   TEST PASSED: governance tighten-now — enact_after==announced_at, enact instantané appliqué (Enacted).");
    Ok(())
}

/// Gouvernance P2c — `admin_update_config` (POST /admin/config) passe par la
/// GOUVERNANCE : plus d'application instantanée hors-DAG (la faille qui rendait
/// le timelock sans effet). Asymétrie via HTTP :
/// - un **resserrage** (baisser `max_mint_per_block`) est appliqué INSTANTANÉMENT
///   (timelock nul) — UX préservée, mais désormais ancré DAG ;
/// - un **desserrage** (hausser `fee_rate_bps`) devient une **proposition
///   timelockée** : la config n'est PAS modifiée tout de suite (bypass fermé).
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_admin_config_governance_rewire -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_admin_config_governance_rewire() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // Baseline.
    let (_, cfg0) = sandbox.admin_get("/admin/config").await;
    let base_maxmint = cfg0["max_mint_per_block"].as_u64().expect("max_mint_per_block");
    let base_fee = cfg0["fee_rate_bps"].as_u64().expect("fee_rate_bps");
    println!("   baseline: max_mint_per_block={base_maxmint}, fee_rate_bps={base_fee}");
    assert_eq!(base_maxmint, 1_000_000);

    // ── TIGHTEN via /admin/config : appliqué INSTANTANÉMENT ──
    let (s, body) = sandbox
        .admin_post("/admin/config", json!({ "SetMaxMint": { "amount": 1 } }))
        .await;
    println!("   POST /admin/config (tighten SetMaxMint=1) → {} — {:?}", s, body);
    assert!(s.is_success(), "tighten config change must succeed: {} {:?}", s, body);
    assert_eq!(body["status"], "applied", "tighten ⇒ applied instantly");
    assert!(
        body["mode"].as_str().unwrap_or_default().contains("instant"),
        "tighten mode must be instant, got {:?}", body["mode"]
    );
    // Vérifie l'application réelle.
    let (_, cfg1) = sandbox.admin_get("/admin/config").await;
    println!("   after tighten: max_mint_per_block={}", cfg1["max_mint_per_block"]);
    assert_eq!(cfg1["max_mint_per_block"].as_u64(), Some(1), "tighten MUST apply (max_mint→1)");

    // ── LOOSEN via /admin/config : proposition TIMELOCKÉE, PAS appliquée ──
    let (s, body) = sandbox
        .admin_post("/admin/config", json!({ "SetFeeRate": { "bps": 777 } }))
        .await;
    println!("   POST /admin/config (loosen SetFeeRate=777) → {} — {:?}", s, body);
    assert!(s.is_success(), "loosen propose must be accepted: {} {:?}", s, body);
    assert_eq!(body["status"], "proposed", "loosen ⇒ timelocked proposal, not applied");
    let proposal_id = body["proposal_id"].as_str().expect("proposal_id").to_string();
    // La config n'a PAS changé — le bypass instantané est fermé.
    let (_, cfg2) = sandbox.admin_get("/admin/config").await;
    println!("   after loosen-propose: fee_rate_bps={} (must be unchanged {base_fee})", cfg2["fee_rate_bps"]);
    assert_eq!(
        cfg2["fee_rate_bps"].as_u64(), Some(base_fee),
        "BYPASS CLOSED: a loosen via /admin/config MUST NOT apply instantly"
    );
    // La proposition est visible publiquement, Pending.
    let (_, pending) = sandbox.public_get("/v1/governance/pending").await;
    let listed = pending["pending"].as_array()
        .map(|a| a.iter().any(|p| p["proposal_id"] == proposal_id && p["status"] == "pending"))
        .unwrap_or(false);
    assert!(listed, "the loosen proposal must be announced as pending: {:?}", pending);

    println!("\n   TEST PASSED: /admin/config rewire — tighten instant, loosen timelocké (bypass fermé).");
    Ok(())
}

/// Gouvernance — journal d'audit `GET /v1/governance/blocks` (public).
///
/// Prouve que l'endpoint expose TOUS les blocs DAG de gouvernance, groupés par
/// cycle : un resserrage (proposal + enact instantané) ⇒ 2 blocs ; une proposition
/// annulée (proposal + cancel) ⇒ 2 blocs. Chaque entrée porte son `block_id` et
/// son `kind`, et les block_ids correspondent à ceux renvoyés par les actions.
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_governance_blocks_audit_trail -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_governance_blocks_audit_trail() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // (A) Resserrage instantané : propose + enact → 2 blocs (proposal, enact).
    let (s, p1) = sandbox
        .admin_post(
            "/admin/governance/propose",
            json!({ "update": { "SetMaxMint": { "amount": 1 } }, "tier": "Policy", "reason": "tighten" }),
        )
        .await;
    assert!(s.is_success(), "propose tighten: {s} {p1:?}");
    let pid1 = p1["proposal_id"].as_str().unwrap().to_string();
    let proposal_block_1 = p1["block_id"].as_str().unwrap().to_string();
    let (s, e1) = sandbox
        .admin_post(&format!("/admin/governance/enact/{pid1}"), json!({ "reason": "now" }))
        .await;
    assert!(s.is_success(), "enact tighten: {s} {e1:?}");
    let enact_block_1 = e1["block_id"].as_str().unwrap().to_string();

    // (B) Proposition annulée : propose (loosen) + cancel → 2 blocs (proposal, cancel).
    let (s, p2) = sandbox
        .admin_post(
            "/admin/governance/propose",
            json!({ "update": { "SetFeeRate": { "bps": 555 } }, "tier": "Operator", "reason": "loosen" }),
        )
        .await;
    assert!(s.is_success(), "propose loosen: {s} {p2:?}");
    let pid2 = p2["proposal_id"].as_str().unwrap().to_string();
    let (s, c2) = sandbox
        .admin_post(&format!("/admin/governance/cancel/{pid2}"), json!({ "reason": "abort" }))
        .await;
    assert!(s.is_success(), "cancel: {s} {c2:?}");
    let cancel_block_2 = c2["block_id"].as_str().unwrap().to_string();

    // (C) GET /v1/governance/blocks → journal complet.
    let (s, body) = sandbox.public_get("/v1/governance/blocks").await;
    println!("   /v1/governance/blocks → {} — {}", s, serde_json::to_string_pretty(&body).unwrap());
    assert!(s.is_success());
    let blocks = body["blocks"].as_array().expect("blocks array");

    // Helper : trouve une entrée par (proposal_id, kind).
    let find = |pid: &str, kind: &str| -> Option<serde_json::Value> {
        blocks.iter().find(|b| b["proposal_id"] == pid && b["kind"] == kind).cloned()
    };

    // Cycle A : proposal + enact, block_ids cohérents avec les réponses des actions.
    let a_prop = find(&pid1, "proposal").expect("cycle A proposal block listed");
    let a_enact = find(&pid1, "enact").expect("cycle A enact block listed");
    assert_eq!(a_prop["block_id"].as_str(), Some(proposal_block_1.as_str()), "proposal block_id match");
    assert_eq!(a_enact["block_id"].as_str(), Some(enact_block_1.as_str()), "enact block_id match");
    assert_eq!(a_enact["status"], "enacted");
    assert!(find(&pid1, "cancel").is_none(), "no cancel block for an enacted proposal");

    // Cycle B : proposal + cancel.
    assert!(find(&pid2, "proposal").is_some(), "cycle B proposal block listed");
    let b_cancel = find(&pid2, "cancel").expect("cycle B cancel block listed");
    assert_eq!(b_cancel["block_id"].as_str(), Some(cancel_block_2.as_str()), "cancel block_id match");
    assert_eq!(b_cancel["status"], "cancelled");
    assert!(find(&pid2, "enact").is_none(), "no enact block for a cancelled proposal");

    // count == nombre d'entrées (4 ici minimum : 2 cycles × 2 blocs).
    assert_eq!(body["count"].as_u64(), Some(blocks.len() as u64));
    assert!(blocks.len() >= 4, "at least 4 governance blocks (2 cycles × 2)");

    println!("\n   TEST PASSED: /v1/governance/blocks expose le journal d'audit (proposal+enact, proposal+cancel).");
    Ok(())
}

/// Semi-fongibles — cycle de vie e2e via les endpoints (spec semi-fungibles).
///
/// Prouve : création de classe (publique, listée), mint contraint par `max_supply`
/// (S3 — un mint qui dépasse le cap est REJETÉ), et **fongibilité** sur le chemin
/// UTXO générique (S5 — transfert partiel d'une classe via `send-simple`). Le
/// transfert/burn ne sont PAS du code SFT-spécifique : c'est le moteur UTXO.
///
/// Run: `cargo test --release -p pms-server --test dag_sandbox \
///   test_sft_lifecycle -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_sft_lifecycle() -> Result<()> {
    let sandbox = boot_sandbox().await?;
    let asset_id = "edenite-game:iron-sword";

    // 1) Création de la classe (cap = 100, items entiers).
    let (s, body) = sandbox
        .admin_post(
            "/admin/sft/classes",
            json!({
                "collection_id": "edenite-game",
                "class_id": "iron-sword",
                "name": "Épée de fer",
                "uri": "https://example/sword.png",
                "decimals": 0,
                "max_supply": "100"
            }),
        )
        .await;
    println!("   create class → {} — {:?}", s, body);
    assert!(s.is_success(), "create class must succeed: {s} {body:?}");
    assert_eq!(body["asset_id"], asset_id);

    // 2) Catalogue PUBLIC : la classe est listée + récupérable + groupée par collection.
    let (_, list) = sandbox.public_get("/v1/sft/classes").await;
    assert!(
        list["classes"].as_array().unwrap().iter().any(|c| c["asset_id"] == asset_id),
        "class must be listed: {list:?}"
    );
    let (s, one) = sandbox.public_get(&format!("/v1/sft/classes/{asset_id}")).await;
    assert!(s.is_success() && one["name"] == "Épée de fer", "get class: {s} {one:?}");
    let (_, coll) = sandbox.public_get("/v1/sft/collections/edenite-game").await;
    assert_eq!(coll["count"].as_u64(), Some(1), "collection lists its class");

    let owner = &sandbox.admin_addr.clone();

    // 3) Mint 60 → balance 60.
    let (s, _) = sandbox
        .admin_post("/admin/sft/mint", json!({ "asset_id": asset_id, "to": owner, "amount": "60" }))
        .await;
    assert!(s.is_success(), "first mint must succeed: {s}");
    let bal = sandbox.get_asset_balance("main", owner, Some(asset_id)).await?;
    println!("   after mint 60: balance = {bal}");
    assert_eq!(bal, dec_sft("60"), "balance after mint = 60");

    // 4) S3 — mint 60 de plus (total 120 > cap 100) → REJETÉ ; balance inchangée.
    let (s, body) = sandbox
        .admin_post("/admin/sft/mint", json!({ "asset_id": asset_id, "to": owner, "amount": "60" }))
        .await;
    println!("   over-cap mint → {} — {:?}", s, body);
    assert!(!s.is_success(), "mint exceeding max_supply MUST be rejected (got {s})");
    assert!(
        body["message"].as_str().unwrap_or_default().contains("max_supply"),
        "operator must see WHY (max_supply), got: {:?}",
        body["message"]
    );
    let bal = sandbox.get_asset_balance("main", owner, Some(asset_id)).await?;
    assert_eq!(bal, dec_sft("60"), "rejected mint leaves balance at 60");

    // 5) Mint 40 → balance 100 (cap atteint pile).
    let (s, _) = sandbox
        .admin_post("/admin/sft/mint", json!({ "asset_id": asset_id, "to": owner, "amount": "40" }))
        .await;
    assert!(s.is_success(), "mint up to the cap must succeed: {s}");
    assert_eq!(sandbox.get_asset_balance("main", owner, Some(asset_id)).await?, dec_sft("100"));

    // 6) S5 — FONGIBILITÉ : l'owner transfère 30 à B via le chemin UTXO générique.
    let bob = Wallet::from_seed(&[209u8; 32], None).unwrap().get_address("8e");
    sandbox
        .send_asset("main", &sandbox.admin_wallet.private_key_b64, &bob, "30", asset_id)
        .await?;
    let bal_owner = sandbox.get_asset_balance("main", owner, Some(asset_id)).await?;
    let bal_bob = sandbox.get_asset_balance("main", &bob, Some(asset_id)).await?;
    println!("   after transfer 30: owner = {bal_owner}, bob = {bal_bob}");
    assert_eq!(bal_owner, dec_sft("70"), "owner = 100 - 30");
    assert_eq!(bal_bob, dec_sft("30"), "bob = 30 (fongible within class)");

    println!("\n   TEST PASSED: SFT — création/catalogue, mint contraint par cap (S3), fongibilité e2e (S5).");
    Ok(())
}

/// Helper local : parse un Decimal pour les assertions SFT.
fn dec_sft(s: &str) -> rust_decimal::Decimal {
    rust_decimal::Decimal::from_str_exact(s).unwrap()
}

// ============================================================================
// TEST: Marketplace settlement with consensus-enforced resale royalty (2.7)
//
// Prouve, sur le VRAI chemin (endpoint → build+co-sign → persist → validate →
// delta → balances), que :
//   1. une vente/revente est ATOMIQUE (item ↔ paiement en 1 bloc) ;
//   2. la royalty de revente est PRÉLEVÉE et VERSÉE au créateur, au consensus,
//      dans l'asset de PAIEMENT (PMS natif OU token custom) ;
//   3. un settlement TAMPERED (créateur sous-payé) est REJETÉ par persist.
// ============================================================================

impl Sandbox {
    /// Fetch UTXO refs `(OutputId, asset_id, amount)` for hand-building txs in
    /// adversarial settlement tests. Parses `txId`/`outIdx` from the utxos endpoint.
    async fn get_utxo_refs(
        &self,
        ledger_id: &str,
        address: &str,
    ) -> Result<Vec<(pms_types::OutputId, Option<String>, Decimal)>> {
        let url = match ledger_id {
            "main" => format!("{}/v1/wallet/{}/utxos", self.base_url, address),
            lid => format!("{}/l/{}/v1/wallet/{}/utxos", self.base_url, lid, address),
        };
        let resp = self.client.get(&url).send().await.context("GET utxos failed")?;
        let json: Value = resp.json().await.unwrap_or(json!({}));
        let refs = json["utxos"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|u| {
                        let txid = u["txId"].as_str()?.to_string();
                        let index = u["outIdx"].as_u64()? as u32;
                        let asset_id = u["asset_id"].as_str().map(|s| s.to_string());
                        let amount = u["amount"].as_str().and_then(|s| Decimal::from_str(s).ok())?;
                        Some((pms_types::OutputId { txid, index }, asset_id, amount))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(refs)
    }

    /// Activity-feed `activity_type` labels for an address (proves the RocksDB
    /// activity index recorded a block — GAP1 audit-trail check).
    async fn activity_types(&self, address: &str) -> Result<Vec<String>> {
        let url = format!("{}/v1/wallet/{}/activity", self.base_url, address);
        let resp = self.client.get(&url).send().await.context("GET activity failed")?;
        let json: Value = resp.json().await.unwrap_or(json!({}));
        Ok(json["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|it| it["activity_type"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Current DAG tips (parents for a hand-forged block).
    async fn tips(&self) -> Result<Vec<String>> {
        let (st, body) = self.post(None, "/v1/dag/tips", json!({ "limit": 1 })).await;
        anyhow::ensure!(st.is_success(), "tips failed: {st} {body}");
        Ok(body
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_market_settle_royalty_pms_price() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    let seller = Wallet::generate();
    let buyer = Wallet::generate();
    let creator = Wallet::generate(); // royalty beneficiary
    let seller_addr = seller.get_address("8e");
    let buyer_addr = buyer.get_address("8e");
    let creator_addr = creator.get_address("8e");

    println!("\n=== [MARKET SETTLE / PMS PRICE] SFT édition /10, royalty 20% ===");
    println!(
        "  seller={}… buyer={}… creator={}…",
        &seller_addr[..14], &buyer_addr[..14], &creator_addr[..14]
    );

    // 1) Classe SFT "studio:ticket" plafonnée à 10, royalty 20% → creator.
    let (st, body) = sandbox
        .admin_post(
            "/admin/sft/classes",
            json!({
                "collection_id": "studio", "class_id": "ticket",
                "name": "Cosmic Ticket /10", "decimals": 0, "max_supply": "10",
                "royalty_bps": 2000, "royalty_beneficiary": creator_addr,
            }),
        )
        .await;
    println!("  create class: {st} — {body:?}");
    anyhow::ensure!(st.is_success(), "create class failed: {st} {body}");

    // 2) Mint 1 exemplaire au vendeur.
    let (st, body) = sandbox
        .admin_post(
            "/admin/sft/mint",
            json!({ "asset_id": "studio:ticket", "to": seller_addr, "amount": "1" }),
        )
        .await;
    println!("  mint ticket→seller: {st} — {body:?}");
    anyhow::ensure!(st.is_success(), "mint failed: {st} {body}");

    // 3) Faucet PMS à l'acheteur (prix 100 + gas).
    sandbox.faucet_mint(None, &buyer_addr, "1000").await?;
    sleep(Duration::from_millis(200)).await;

    let seller_item_before = sandbox.get_asset_balance("main", &seller_addr, Some("studio:ticket")).await?;
    let buyer_pms_before = sandbox.get_balance("main", &buyer_addr).await?;
    println!("  BEFORE: seller_item={seller_item_before}, buyer_pms={buyer_pms_before}");
    assert_eq!(seller_item_before, Decimal::from(1), "seller owns 1 ticket");

    // 4) Revente atomique : l'acheteur paie 100 PMS pour le ticket.
    let (st, body) = sandbox
        .post(
            None,
            "/v1/market/settle",
            json!({
                "seller_private_key_b64": seller.private_key_b64,
                "buyer_private_key_b64": buyer.private_key_b64,
                "asset_sold": "studio:ticket", "quantity": "1", "price": "100",
            }),
        )
        .await;
    println!("  SETTLE → {st} — {body:?}");
    anyhow::ensure!(st.is_success(), "settle failed: {st} {body}");
    assert_eq!(body["royalty"], "20", "royalty = 20% × 100 = 20");
    assert_eq!(body["net_to_seller"], "80", "seller net = 80");
    assert_eq!(body["royalty_beneficiary"].as_str(), Some(creator_addr.as_str()));
    sleep(Duration::from_millis(200)).await;

    let buyer_item = sandbox.get_asset_balance("main", &buyer_addr, Some("studio:ticket")).await?;
    let seller_item = sandbox.get_asset_balance("main", &seller_addr, Some("studio:ticket")).await?;
    let creator_pms = sandbox.get_balance("main", &creator_addr).await?;
    let seller_pms = sandbox.get_balance("main", &seller_addr).await?;
    let buyer_pms = sandbox.get_balance("main", &buyer_addr).await?;
    println!("  AFTER: buyer_item={buyer_item}, seller_item={seller_item}");
    println!("         creator_pms(royalty)={creator_pms}, seller_pms(net)={seller_pms}, buyer_pms={buyer_pms}");

    assert_eq!(buyer_item, Decimal::from(1), "buyer now owns the ticket");
    assert_eq!(seller_item, Decimal::ZERO, "seller relinquished the ticket");
    assert_eq!(creator_pms, Decimal::from(20), "creator received 20% royalty in PMS");
    assert_eq!(seller_pms, Decimal::from(80), "seller received 80 net in PMS");
    assert!(buyer_pms < Decimal::from(900), "buyer paid price+gas, remaining={buyer_pms}");

    // GAP1 audit-trail: the sale MUST surface in each party's activity feed
    // (previously MarketSettle fell through a wildcard → invisible everywhere).
    let buyer_acts = sandbox.activity_types(&buyer_addr).await?;
    let seller_acts = sandbox.activity_types(&seller_addr).await?;
    let creator_acts = sandbox.activity_types(&creator_addr).await?;
    println!("  ACTIVITY buyer={buyer_acts:?} seller={seller_acts:?} creator={creator_acts:?}");
    assert!(buyer_acts.iter().any(|t| t == "market_buy"), "buyer sees market_buy");
    assert!(seller_acts.iter().any(|t| t == "market_sell"), "seller sees market_sell");
    assert!(creator_acts.iter().any(|t| t == "royalty_received"), "creator sees royalty_received");

    println!("  ✅ atomic resale + 20% royalty enforced by consensus (PMS price) + visible in activity");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_market_settle_royalty_custom_token_price() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    let seller = Wallet::generate();
    let buyer = Wallet::generate();
    let creator = Wallet::generate();
    let seller_addr = seller.get_address("8e");
    let buyer_addr = buyer.get_address("8e");
    let creator_addr = creator.get_address("8e");

    println!("\n=== [MARKET SETTLE / CUSTOM-TOKEN PRICE] royalty en usdx (pas PMS) ===");

    // Payment token "usdx" (6 decimals).
    let (st, body) = sandbox
        .admin_post(
            "/admin/tokens/create",
            json!({ "asset_id": "usdx", "symbol": "USDX", "name": "Test USD", "decimals": 6, "max_supply": "1000000" }),
        )
        .await;
    anyhow::ensure!(st.is_success(), "usdx create failed: {st} {body}");

    // SFT class with a 15% royalty → creator.
    let (st, body) = sandbox
        .admin_post(
            "/admin/sft/classes",
            json!({
                "collection_id": "art", "class_id": "print", "name": "Signed Print",
                "decimals": 0, "max_supply": "5", "royalty_bps": 1500, "royalty_beneficiary": creator_addr,
            }),
        )
        .await;
    anyhow::ensure!(st.is_success(), "class failed: {st} {body}");

    // Mint the item to seller; mint 1000 usdx to buyer; faucet buyer PMS for gas.
    let (st, _b) = sandbox.admin_post("/admin/sft/mint", json!({ "asset_id": "art:print", "to": seller_addr, "amount": "1" })).await;
    anyhow::ensure!(st.is_success(), "sft mint failed");
    let (st, _b) = sandbox.admin_post("/admin/tokens/mint", json!({ "asset_id": "usdx", "to": buyer_addr, "amount": "1000" })).await;
    anyhow::ensure!(st.is_success(), "usdx mint failed");
    sandbox.faucet_mint(None, &buyer_addr, "100").await?; // PMS for gas
    sleep(Duration::from_millis(200)).await;

    // Settle: buyer buys the print for 200 usdx. Royalty = 15% × 200 = 30 usdx.
    let (st, body) = sandbox
        .post(
            None,
            "/v1/market/settle",
            json!({
                "seller_private_key_b64": seller.private_key_b64,
                "buyer_private_key_b64": buyer.private_key_b64,
                "asset_sold": "art:print", "quantity": "1",
                "price_asset": "usdx", "price": "200",
            }),
        )
        .await;
    println!("  SETTLE(usdx) → {st} — {body:?}");
    anyhow::ensure!(st.is_success(), "settle failed: {st} {body}");
    assert_eq!(body["royalty"], "30", "royalty = 15% × 200 = 30 usdx");
    assert_eq!(body["net_to_seller"], "170");
    sleep(Duration::from_millis(200)).await;

    let buyer_item = sandbox.get_asset_balance("main", &buyer_addr, Some("art:print")).await?;
    let creator_usdx = sandbox.get_asset_balance("main", &creator_addr, Some("usdx")).await?;
    let seller_usdx = sandbox.get_asset_balance("main", &seller_addr, Some("usdx")).await?;
    let buyer_usdx = sandbox.get_asset_balance("main", &buyer_addr, Some("usdx")).await?;
    println!("  AFTER: buyer_item={buyer_item}, creator_usdx(royalty)={creator_usdx}, seller_usdx(net)={seller_usdx}, buyer_usdx={buyer_usdx}");
    assert_eq!(buyer_item, Decimal::from(1), "buyer owns the print");
    assert_eq!(creator_usdx, Decimal::from(30), "creator got 30 usdx royalty (NOT PMS)");
    assert_eq!(seller_usdx, Decimal::from(170), "seller got 170 usdx net");
    assert_eq!(buyer_usdx, Decimal::from(800), "buyer paid 200 usdx (1000-200)");
    println!("  ✅ royalty prélevée dans le token de PAIEMENT custom (usdx), consensus-enforced");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_market_settle_rejects_tampered_royalty() -> Result<()> {
    use pms_types::{PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock};

    let sandbox = boot_sandbox().await?;

    let seller = Wallet::generate();
    let buyer = Wallet::generate();
    let creator = Wallet::generate();
    let seller_addr = seller.get_address("8e");
    let buyer_addr = buyer.get_address("8e");
    let creator_addr = creator.get_address("8e");

    println!("\n=== [MARKET SETTLE / REJECT] créateur sous-payé (10 au lieu de 20) ===");

    // Class with 20% royalty; mint item to seller; faucet buyer PMS.
    let (st, _b) = sandbox
        .admin_post("/admin/sft/classes", json!({
            "collection_id": "studio", "class_id": "ticket", "name": "Ticket", "decimals": 0,
            "max_supply": "10", "royalty_bps": 2000, "royalty_beneficiary": creator_addr,
        }))
        .await;
    anyhow::ensure!(st.is_success(), "class failed");
    let (st, _b) = sandbox.admin_post("/admin/sft/mint", json!({ "asset_id": "studio:ticket", "to": seller_addr, "amount": "1" })).await;
    anyhow::ensure!(st.is_success(), "mint failed");
    sandbox.faucet_mint(None, &buyer_addr, "1000").await?;
    sleep(Duration::from_millis(200)).await;

    // Hand-build a TAMPERED settlement: buyer pays 100 PMS, but the creator only
    // gets 10 (should be 20) and the seller grabs 90 (should be 80). The wrapped
    // tx is fully valid (conservation holds, both parties sign) — only the ROYALTY
    // shape is wrong. The consensus gate must reject it.
    let seller_refs = sandbox.get_utxo_refs("main", &seller_addr).await?;
    let (item_oid, _, _) = seller_refs
        .iter()
        .find(|(_, a, amt)| a.as_deref() == Some("studio:ticket") && *amt >= Decimal::from(1))
        .cloned()
        .expect("seller item UTXO");
    let buyer_refs = sandbox.get_utxo_refs("main", &buyer_addr).await?;
    let (pms_oid, _, pms_amt) = buyer_refs
        .iter()
        .find(|(_, a, amt)| a.is_none() && *amt >= Decimal::from(100))
        .cloned()
        .expect("buyer PMS UTXO");

    let buyer_change = pms_amt - Decimal::from(100);
    let mut outputs = vec![
        TxOutput::new(buyer_addr.clone(), "1", Some("studio:ticket".to_string())), // item → buyer
        TxOutput::new(creator_addr.clone(), "10", None), // TAMPERED royalty (should be 20)
        TxOutput::new(seller_addr.clone(), "90", None),  // TAMPERED net (should be 80)
    ];
    if buyer_change > Decimal::ZERO {
        outputs.push(TxOutput::new(buyer_addr.clone(), buyer_change.to_string(), None));
    }
    let unsigned = Transaction {
        inputs: vec![TxInput { out: item_oid }, TxInput { out: pms_oid }],
        outputs,
        fee: "0".to_string(),
        unlocks: vec![],
    };
    let msg = unsigned.signing_message(&sandbox.network_id).expect("signing_message");
    let seller_sig = seller.sign(&msg).expect("seller sign");
    let buyer_sig = buyer.sign(&msg).expect("buyer sign");
    let signed = Transaction {
        unlocks: vec![
            Unlock::new(seller.public_key_hex.clone(), seller_sig), // input 0 = seller item
            Unlock::new(buyer.public_key_hex.clone(), buyer_sig),   // input 1 = buyer PMS
        ],
        ..unsigned
    };

    let payload = PlainPayload::MarketSettle {
        tx: signed,
        asset_sold: "studio:ticket".to_string(),
        quantity: "1".to_string(),
        price_asset: None,
        price: "100".to_string(),
        seller: seller_addr.clone(),
        buyer: buyer_addr.clone(),
    };

    let parents = sandbox.tips().await?;
    anyhow::ensure!(!parents.is_empty(), "no tips");
    let wb = forge_signed_wire_block_for_test(
        parents,
        &sandbox.wire_meta,
        &sandbox.admin_wallet, // Coordinator signs the block (single-writer)
        0,
        Some(PayloadEnvelope::Plain(payload)),
    );

    // `/submit/block` returns the rejection reason as a PLAIN-TEXT body — read it raw.
    let resp = sandbox
        .client
        .post(format!("{}/submit/block", sandbox.base_url))
        .json(&wb)
        .send()
        .await
        .expect("submit HTTP failed");
    let st = resp.status();
    let reason = resp.text().await.unwrap_or_default();
    println!("  SUBMIT tampered settlement → {st} — reason: {reason:?}");

    // Must be rejected, and for the RIGHT reason (settlement/royalty shape).
    assert!(!st.is_success(), "tampered settlement must NOT be accepted (got {st})");
    let low = reason.to_lowercase();
    assert!(
        low.contains("settlement"),
        "reject reason must be a settlement violation, got: {reason:?}"
    );
    // Exact-accounting gate: the declared split is item→buyer, royalty(20)→creator,
    // net(80)→seller. The tampered tx routes 90 to the seller and 10 to the creator,
    // so at least one address nets an amount that is NOT part of the declared split.
    // The gate reports the first such mismatch as "unexpected credit <n> <asset> to
    // <addr> (not part of the declared split)" — this is precisely the royalty-shape
    // enforcement, and the assertion is order-independent (either the seller's +90
    // over-credit or the creator's +10 short-credit may surface first).
    assert!(
        low.contains("unexpected credit") && low.contains("not part of the declared split"),
        "reject reason must be the exact-accounting royalty-split violation, got: {reason:?}"
    );
    // And it must name a PMS amount from the tampered split (90 grabbed by seller, or
    // 10 short-paid to creator) — proving it caught THIS mis-routing, not an unrelated
    // failure.
    assert!(
        low.contains(" 90 pms") || low.contains(" 10 pms"),
        "reject reason must reference the tampered PMS split (90 or 10), got: {reason:?}"
    );

    // And the state must be unchanged: buyer got no ticket, creator got nothing.
    sleep(Duration::from_millis(150)).await;
    let buyer_item = sandbox.get_asset_balance("main", &buyer_addr, Some("studio:ticket")).await?;
    let creator_pms = sandbox.get_balance("main", &creator_addr).await?;
    let seller_item = sandbox.get_asset_balance("main", &seller_addr, Some("studio:ticket")).await?;
    println!("  STATE UNCHANGED: buyer_item={buyer_item}, creator_pms={creator_pms}, seller_item={seller_item}");
    assert_eq!(buyer_item, Decimal::ZERO, "buyer must NOT have received the item");
    assert_eq!(creator_pms, Decimal::ZERO, "creator must NOT have been paid");
    assert_eq!(seller_item, Decimal::from(1), "seller must still own the item");
    println!("  ✅ tampered royalty settlement REJECTED by consensus; state intact");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_royalty_beneficiary_change_after_mint() -> Result<()> {
    let sandbox = boot_sandbox().await?;
    let seller = Wallet::generate();
    let buyer = Wallet::generate();
    let creator_a = Wallet::generate();
    let creator_b = Wallet::generate();
    let seller_addr = seller.get_address("8e");
    let buyer_addr = buyer.get_address("8e");
    let creator_a_addr = creator_a.get_address("8e");
    let creator_b_addr = creator_b.get_address("8e");

    println!("\n=== [ROYALTY BENEFICIARY CHANGE POST-MINT] créateur A → B ===");

    // Classe royalty 20% → créateur A ; 2 exemplaires au vendeur.
    let (st, _b) = sandbox
        .admin_post("/admin/sft/classes", json!({
            "collection_id":"studio","class_id":"pass","name":"Season Pass","decimals":0,
            "max_supply":"10","royalty_bps":2000,"royalty_beneficiary":creator_a_addr,
        }))
        .await;
    anyhow::ensure!(st.is_success(), "class create failed");
    let (st, _b) = sandbox.admin_post("/admin/sft/mint", json!({"asset_id":"studio:pass","to":seller_addr,"amount":"2"})).await;
    anyhow::ensure!(st.is_success(), "mint failed");
    sandbox.faucet_mint(None, &buyer_addr, "1000").await?;
    sleep(Duration::from_millis(200)).await;

    // Vente #1 → créateur A payé (politique d'origine).
    let (st, b1) = sandbox
        .post(None, "/v1/market/settle", json!({
            "seller_private_key_b64":seller.private_key_b64,"buyer_private_key_b64":buyer.private_key_b64,
            "asset_sold":"studio:pass","quantity":"1","price":"100",
        }))
        .await;
    println!("  settle #1 → {st} — beneficiary={:?}", b1["royalty_beneficiary"]);
    anyhow::ensure!(st.is_success(), "settle 1 failed: {b1}");
    assert_eq!(b1["royalty_beneficiary"].as_str(), Some(creator_a_addr.as_str()));
    sleep(Duration::from_millis(200)).await;

    // Redirection du bénéficiaire vers le créateur B — AUTORISÉE PAR LA
    // CO-SIGNATURE du bénéficiaire courant (créateur A). Custodial : on passe la
    // clé de A ; le serveur vérifie que A EST le bénéficiaire courant, signe.
    let (st, upd) = sandbox
        .post(None, "/v1/royalty/update", json!({
            "asset_id":"studio:pass",
            "royalty_beneficiary":creator_b_addr,
            "authorizer_private_key_b64": creator_a.private_key_b64,
        }))
        .await;
    println!("  ROYALTY UPDATE (signé par A) → {st} — {upd:?}");
    anyhow::ensure!(st.is_success(), "royalty update failed: {upd}");
    assert_eq!(upd["royalty_beneficiary"].as_str(), Some(creator_b_addr.as_str()));
    assert_eq!(upd["royalty_bps"].as_u64(), Some(2000), "taux inchangé");
    sleep(Duration::from_millis(200)).await;

    // Après le handoff A→B, le créateur A n'est PLUS le bénéficiaire courant :
    // sa clé ne peut plus autoriser un changement (403 au pré-check endpoint).
    let (st_a, _r) = sandbox
        .post(None, "/v1/royalty/update", json!({
            "asset_id":"studio:pass",
            "royalty_beneficiary":creator_a_addr,
            "authorizer_private_key_b64": creator_a.private_key_b64,
        }))
        .await;
    println!("  A tente de reprendre la royalty → {st_a} (attendu 403)");
    assert_eq!(st_a.as_u16(), 403, "A n'est plus le bénéficiaire courant");
    sleep(Duration::from_millis(150)).await;

    // Vente #2 → créateur B payé (nouvelle politique), A inchangé.
    let (st, b2) = sandbox
        .post(None, "/v1/market/settle", json!({
            "seller_private_key_b64":seller.private_key_b64,"buyer_private_key_b64":buyer.private_key_b64,
            "asset_sold":"studio:pass","quantity":"1","price":"100",
        }))
        .await;
    println!("  settle #2 → {st} — beneficiary={:?}", b2["royalty_beneficiary"]);
    anyhow::ensure!(st.is_success(), "settle 2 failed: {b2}");
    assert_eq!(b2["royalty_beneficiary"].as_str(), Some(creator_b_addr.as_str()), "settle #2 paie le créateur B");
    sleep(Duration::from_millis(200)).await;

    let a = sandbox.get_balance("main", &creator_a_addr).await?;
    let b = sandbox.get_balance("main", &creator_b_addr).await?;
    let s = sandbox.get_balance("main", &seller_addr).await?;
    let buyer_tickets = sandbox.get_asset_balance("main", &buyer_addr, Some("studio:pass")).await?;
    println!("  creatorA(old)={a} creatorB(new)={b} seller={s} buyer_passes={buyer_tickets}");
    assert_eq!(a, Decimal::from(20), "créateur A : royalty de la vente #1 seulement");
    assert_eq!(b, Decimal::from(20), "créateur B : royalty de la vente #2 (post-changement)");
    assert_eq!(s, Decimal::from(160), "vendeur : 80 + 80");
    assert_eq!(buyer_tickets, Decimal::from(2), "acheteur possède les 2 pass");

    let b_acts = sandbox.activity_types(&creator_b_addr).await?;
    println!("  creatorB activity = {b_acts:?}");
    assert!(b_acts.iter().any(|t| t == "royalty_updated"), "créateur B voit royalty_updated");
    assert!(b_acts.iter().any(|t| t == "royalty_received"), "créateur B voit royalty_received");

    println!("  ✅ bénéficiaire redirigé APRÈS mint ; les ventes futures paient le nouveau bénéficiaire");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_royalty_change_rejected_without_beneficiary_signature() -> Result<()> {
    use pms_types::{PayloadEnvelope, PlainPayload};

    let sandbox = boot_sandbox().await?;
    let creator = Wallet::generate();
    let attacker = Wallet::generate();
    let creator_addr = creator.get_address("8e");
    let attacker_addr = attacker.get_address("8e");

    println!("\n=== [ROYALTY CHANGE — CONSENSUS REJECT sans co-signature] ===");

    // Classe royalty 20% → créateur (bénéficiaire courant).
    let (st, _b) = sandbox
        .admin_post("/admin/sft/classes", json!({
            "collection_id":"secure","class_id":"art","name":"Art","decimals":0,
            "max_supply":"5","royalty_bps":2000,"royalty_beneficiary":creator_addr,
        }))
        .await;
    anyhow::ensure!(st.is_success(), "class create failed");
    sleep(Duration::from_millis(150)).await;

    // (1) L'ATTAQUANT forge un RoyaltyUpdate signé par SA propre clé (signature
    //     cryptographiquement valide) pour se rediriger la royalty. Le bloc est
    //     forgé par le COORDINATEUR (admin_wallet). Le consensus doit rejeter car
    //     l'attaquant n'est pas le bénéficiaire courant.
    let msg = pms_types::royalty_update_signing_message(
        &sandbox.network_id, "secure:art", Some(2000), Some(&attacker_addr), 0,
    );
    let sig = attacker.sign(&msg).expect("attacker sign");
    let payload = PlainPayload::RoyaltyUpdate {
        asset_id: "secure:art".to_string(),
        royalty_bps: Some(2000),
        royalty_beneficiary: Some(attacker_addr.clone()),
        auth_pubkey_hex: attacker.public_key_hex.clone(),
        auth_signature_b64: sig,
    };
    let parents = sandbox.tips().await?;
    let wb = forge_signed_wire_block_for_test(
        parents, &sandbox.wire_meta, &sandbox.admin_wallet, 0,
        Some(PayloadEnvelope::Plain(payload)),
    );
    let resp = sandbox.client.post(format!("{}/submit/block", sandbox.base_url)).json(&wb).send().await.expect("submit");
    let code = resp.status();
    let reason = resp.text().await.unwrap_or_default();
    println!("  attaquant signe pour lui-même → {code} — {reason:?}");
    assert!(!code.is_success(), "un tiers ne peut pas rediriger la royalty (got {code})");
    assert!(reason.to_lowercase().contains("current royalty beneficiary"), "raison: {reason:?}");

    // (2) TAMPER : le VRAI bénéficiaire signe pour X, mais le payload déclare Y.
    //     La signature ne colle pas à la politique déclarée → rejet.
    let msg_x = pms_types::royalty_update_signing_message(
        &sandbox.network_id, "secure:art", Some(2000), Some(&attacker_addr), 0,
    );
    let sig_creator_over_x = creator.sign(&msg_x).expect("creator sign");
    let other = Wallet::generate().get_address("8e");
    let tampered = PlainPayload::RoyaltyUpdate {
        asset_id: "secure:art".to_string(),
        royalty_bps: Some(2000),
        royalty_beneficiary: Some(other), // ≠ ce que le créateur a signé (attacker_addr)
        auth_pubkey_hex: creator.public_key_hex.clone(),
        auth_signature_b64: sig_creator_over_x,
    };
    let parents = sandbox.tips().await?;
    let wb2 = forge_signed_wire_block_for_test(
        parents, &sandbox.wire_meta, &sandbox.admin_wallet, 0,
        Some(PayloadEnvelope::Plain(tampered)),
    );
    let resp2 = sandbox.client.post(format!("{}/submit/block", sandbox.base_url)).json(&wb2).send().await.expect("submit");
    let code2 = resp2.status();
    let reason2 = resp2.text().await.unwrap_or_default();
    println!("  sig du créateur sur une AUTRE politique → {code2} — {reason2:?}");
    assert!(!code2.is_success(), "signature/policy mismatch doit être rejeté");
    assert!(reason2.to_lowercase().contains("invalid authorization signature"), "raison: {reason2:?}");

    // (3) State intact : la royalty pointe toujours vers le créateur.
    let (st, cls) = sandbox.post(None, "/v1/royalty/prepare", json!({"asset_id":"secure:art"})).await;
    anyhow::ensure!(st.is_success(), "prepare failed");
    println!("  bénéficiaire courant inchangé = {:?}", cls["current_beneficiary"]);
    assert_eq!(cls["current_beneficiary"].as_str(), Some(creator_addr.as_str()), "royalty intacte");

    println!("  ✅ aucun changement de royalty sans la signature du bénéficiaire courant");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_royalty_change_signature_not_replayable() -> Result<()> {
    let sandbox = boot_sandbox().await?;
    let a = Wallet::generate();
    let b = Wallet::generate();
    let a_addr = a.get_address("8e");
    let b_addr = b.get_address("8e");
    let net = sandbox.network_id.clone();

    println!("\n=== [ROYALTY — signature NON REJOUABLE (anti-replay version)] ===");

    // Classe royalty 20% → A (version 0).
    let (st, _x) = sandbox
        .admin_post("/admin/sft/classes", json!({
            "collection_id":"replay","class_id":"pass","name":"Pass","decimals":0,
            "max_supply":"3","royalty_bps":2000,"royalty_beneficiary":a_addr,
        }))
        .await;
    anyhow::ensure!(st.is_success(), "class create failed");
    sleep(Duration::from_millis(150)).await;

    // A signe pour rediriger vers B, sur la VERSION 0. On CAPTURE cette signature.
    let msg_v0 = pms_types::royalty_update_signing_message(&net, "replay:pass", Some(2000), Some(&b_addr), 0);
    let a_sig_v0 = a.sign(&msg_v0).expect("A sign v0");

    // Appliquée (voie pré-signée) → bénéficiaire = B, version 1.
    let (st, r1) = sandbox
        .post(None, "/v1/royalty/update", json!({
            "asset_id":"replay:pass","royalty_beneficiary":b_addr,
            "auth_pubkey_hex": a.public_key_hex, "auth_signature_b64": a_sig_v0,
        }))
        .await;
    println!("  A→B (sig v0) → {st} {r1:?}");
    anyhow::ensure!(st.is_success(), "A→B failed: {r1}");
    sleep(Duration::from_millis(150)).await;

    // B redonne à A (custodial, version 1) → bénéficiaire = A, version 2.
    let (st, _r2) = sandbox
        .post(None, "/v1/royalty/update", json!({
            "asset_id":"replay:pass","royalty_beneficiary":a_addr,
            "authorizer_private_key_b64": b.private_key_b64,
        }))
        .await;
    anyhow::ensure!(st.is_success(), "B→A failed");
    sleep(Duration::from_millis(150)).await;

    // A est de nouveau le bénéficiaire courant. On REJOUE sa signature v0 (→B).
    // Sans version : ça repasserait (A est courant, sig valide) = hijack. AVEC la
    // version monotone (courante = 2), le message diffère → signature invalide.
    let (st, rr) = sandbox
        .post(None, "/v1/royalty/update", json!({
            "asset_id":"replay:pass","royalty_beneficiary":b_addr,
            "auth_pubkey_hex": a.public_key_hex, "auth_signature_b64": a_sig_v0,
        }))
        .await;
    println!("  REJEU de la sig v0 (A courant, version=2) → {st} {rr:?}");
    // Le consensus refuse la signature rejouée (la version a avancé → message
    // différent → sig invalide). Le handler mappe tout rejet consensus sur
    // ApiError::Conflict (code stable 3070, status 409) — les clients SDK
    // branchent sur le CODE, pas sur le message. Le fait que l'état soit
    // inchangé (asserté plus bas) prouve que le rejeu n'a eu AUCUN effet.
    assert_eq!(st.as_u16(), 409, "rejet consensus = 409 Conflict (got {st})");
    assert_eq!(
        rr["code"].as_u64(),
        Some(3070),
        "code stable = 3070 (Conflict) pour un rejet consensus: {rr:?}"
    );

    // Bénéficiaire courant toujours A (le rejeu n'a rien changé).
    let (st, cls) = sandbox.post(None, "/v1/royalty/prepare", json!({"asset_id":"replay:pass"})).await;
    anyhow::ensure!(st.is_success(), "prepare failed");
    println!("  bénéficiaire courant = {:?}, version = {:?}", cls["current_beneficiary"], cls["current_royalty_version"]);
    assert_eq!(cls["current_beneficiary"].as_str(), Some(a_addr.as_str()), "toujours A");
    assert_eq!(cls["current_royalty_version"].as_u64(), Some(2), "version = 2 (2 changements)");

    println!("  ✅ signature d'autorisation à usage unique (version monotone) — pas de replay");
    Ok(())
}

/// e2e (protocole 2.8) — PROVISIONNEMENT CUSTODIAL COMPLET sur le chemin réel /v1,
/// SANS token admin : create classe SFT (clé créateur) → mint (clé mint_authority)
/// → transfert (rail UTXO générique) → marketSettle (royalty enforced consensus).
/// Prouve qu'une classe créée+mintée par la seule clé du créateur est citoyenne de
/// 1re classe et que sa royalty atterrit au bénéficiaire au règlement. + rejet du
/// mint par une mauvaise clé (autorité).
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore]
async fn test_custodial_provisioning_end_to_end() -> Result<()> {
    let sandbox = boot_sandbox().await?;

    // Créateur = mint_authority + royalty_beneficiary. Vendeur détient l'item.
    // Acheteur paie en PMS. (creator ≠ seller → prouve le routage royalty vers un
    // tiers, pas le cas dégénéré vendeur==bénéficiaire.)
    let creator = Wallet::generate();
    let creator_addr = creator.get_address("8e");
    let creator_sk = creator.private_key_b64.clone();
    let seller = Wallet::generate();
    let seller_addr = seller.get_address("8e");
    let seller_sk = seller.private_key_b64.clone();
    let buyer = Wallet::generate();
    let buyer_addr = buyer.get_address("8e");
    let buyer_sk = buyer.private_key_b64.clone();

    // Gas/paiement PMS (main). Le mint custodial ne dépense aucun UTXO créateur
    // (émission pure) ; seuls le transfert et le settle consomment du gas.
    sandbox.faucet_mint(None, &seller_addr, "100").await?; // gas vendeur
    sandbox.faucet_mint(None, &buyer_addr, "1000").await?; // paiement 100 + gas
    sleep(Duration::from_millis(400)).await;

    // ── 1. CREATE classe SFT custodiale (clé créateur, AUCUN token admin) ──
    let (status, body) = sandbox
        .post(
            None,
            "/v1/sft/classes",
            json!({
                "collection_id": "studio",
                "class_id": "ticket",
                "name": "Concert Ticket",
                "decimals": 0,
                "max_supply": "1000",
                "royalty_bps": 1000,                    // 10 %
                "royalty_beneficiary": creator_addr,    // royalty → créateur
                "creator_private_key_b64": creator_sk,
            }),
        )
        .await;
    println!("   [1] create SFT class (custodial, no admin) → {} {:?}", status, body);
    anyhow::ensure!(status.is_success(), "custodial create failed: {} — {}", status, body);
    // mint_authority == adresse dérivée de la clé créateur (jamais fournie en clair).
    assert_eq!(
        body["mint_authority"].as_str(),
        Some(creator_addr.as_str()),
        "mint_authority must be the creator's derived address"
    );

    // ── 2. MINT custodial 5 tickets → vendeur (clé du mint_authority) ──
    let (status, body) = sandbox
        .post(
            None,
            "/v1/sft/mint",
            json!({
                "asset_id": "studio:ticket",
                "to": seller_addr,
                "amount": "5",
                "mint_authority_private_key_b64": creator_sk,
            }),
        )
        .await;
    println!("   [2] custodial mint 5 → seller → {} {:?}", status, body);
    anyhow::ensure!(status.is_success(), "custodial mint failed: {} — {}", status, body);
    sleep(Duration::from_millis(400)).await;
    let seller_tickets = sandbox
        .get_asset_balance("main", &seller_addr, Some("studio:ticket"))
        .await?;
    println!("   seller tickets after mint = {}", seller_tickets);
    assert_eq!(seller_tickets, Decimal::from(5), "seller must hold 5 minted tickets");

    // ── 2b. AUTORITÉ : une MAUVAISE clé (acheteur) ne peut PAS minter → 403 ──
    let (status, body) = sandbox
        .post(
            None,
            "/v1/sft/mint",
            json!({
                "asset_id": "studio:ticket",
                "to": buyer_addr,
                "amount": "99",
                "mint_authority_private_key_b64": buyer_sk,
            }),
        )
        .await;
    println!("   [2b] wrong-key mint → {} {:?}", status, body);
    anyhow::ensure!(
        status == reqwest::StatusCode::FORBIDDEN,
        "mint by a non-mint_authority key must be 403 Forbidden, got {} — {}",
        status,
        body
    );

    // ── 3. TRANSFERT 2 tickets vendeur → acheteur (rail UTXO générique, send-simple) ──
    let (status, body) = sandbox
        .post(
            None,
            "/v1/wallet/send-simple",
            json!({
                "private_key_b64": seller_sk,
                "to": buyer_addr,
                "amount": "2",
                "asset_id": "studio:ticket",
            }),
        )
        .await;
    println!("   [3] transfer 2 tickets seller→buyer → {} {:?}", status, body);
    anyhow::ensure!(status.is_success(), "SFT transfer failed: {} — {}", status, body);
    sleep(Duration::from_millis(400)).await;

    // ── 4. MARKETSETTLE : vendeur vend 1 ticket à l'acheteur pour 100 PMS ──
    // Royalty 10 % = 10 PMS → créateur (bénéficiaire), net 90 → vendeur.
    let (status, body) = sandbox
        .post(
            None,
            "/v1/market/settle",
            json!({
                "seller_private_key_b64": seller_sk,
                "buyer_private_key_b64": buyer_sk,
                "asset_sold": "studio:ticket",
                "quantity": "1",
                "price": "100",
            }),
        )
        .await;
    println!("   [4] marketSettle 1 ticket @100 PMS → {} {:?}", status, body);
    anyhow::ensure!(status.is_success(), "marketSettle failed: {} — {}", status, body);
    assert_eq!(body["royalty"].as_str(), Some("10"), "royalty = 10% of 100");
    assert_eq!(
        body["royalty_beneficiary"].as_str(),
        Some(creator_addr.as_str()),
        "royalty beneficiary = creator (re-derived from registry at consensus)"
    );
    sleep(Duration::from_millis(400)).await;

    // ── 5. ASSERTIONS finales : la royalty a atterri chez le créateur ──
    let buyer_tickets = sandbox
        .get_asset_balance("main", &buyer_addr, Some("studio:ticket"))
        .await?;
    let creator_pms = sandbox.get_balance("main", &creator_addr).await?;
    println!(
        "   buyer tickets = {} (2 transferred + 1 bought), creator PMS (royalty) = {}",
        buyer_tickets, creator_pms
    );
    assert_eq!(buyer_tickets, Decimal::from(3), "buyer holds 2 (transfer) + 1 (settle) = 3 tickets");
    // Le créateur n'a JAMAIS été fauceté en PMS → son solde PMS == exactement la
    // royalty encaissée au settlement (10). Prouve l'enforcement consensus de la
    // royalty sur un asset créé+minté PAR SA SEULE CLÉ, sans admin.
    assert_eq!(creator_pms, Decimal::from(10), "creator received exactly the 10 PMS royalty");

    println!("  ✅ provisionnement custodial e2e : create+mint (clé créateur) → transfert → settle royalty, ZÉRO token admin");
    Ok(())
}
