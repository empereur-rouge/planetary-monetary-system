/// PMS Engine — main entry point.
///
/// Uses jemalloc as the global allocator to prevent glibc malloc fragmentation
/// under high-throughput multi-threaded RocksDB workloads (v0.6.3).
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use anyhow::Result;
use clap::Parser;

use pms_config::{ServerConfig, load_config};
use pms_ledger::LedgerManager;
use pms_server::Server;
use pms_storage::DagStorage;
use pms_wallet::Wallet;
use rustls::crypto::ring;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Once};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};


#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Chemin (ex: config.dev.toml / config.prod.toml)
    #[arg(long)]
    config: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let _ = ring::default_provider().install_default();
    init_logging();

    // Parse CLI args (handles --version via clap)
    let _args = Args::parse();

    eprintln!("PMS v{} starting...", env!("CARGO_PKG_VERSION"));

    // Log file descriptor limit at startup — fd exhaustion causes silent crashes.
    // Read from /proc/self/limits on Linux (Docker containers).
    if let Ok(contents) = std::fs::read_to_string("/proc/self/limits") {
        for line in contents.lines() {
            if line.starts_with("Max open files") {
                eprintln!("[BOOT] {}", line);
                // Parse soft limit (second column after the label)
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(soft_str) = parts.get(3) {
                    if let Ok(soft) = soft_str.parse::<u64>() {
                        if soft < 8192 {
                            eprintln!(
                                "⚠️  WARNING: fd limit ({}) is dangerously low for RocksDB!",
                                soft
                            );
                            eprintln!(
                                "   Set `ulimits: nofile: {{ soft: 65536, hard: 65536 }}` in docker-compose.yml"
                            );
                        }
                    }
                }
                break;
            }
        }
    }

    // 1) Settings
    let settings = load_config()?;

    // 1.a) Node identity key — prefer the encrypted envelope when one is
    //      configured, fall back to the plain hex file otherwise. In the
    //      encrypted case the passphrase must be supplied via the env var
    //      `PMS_COORDINATOR_KEY_PASSPHRASE` (systemd EnvironmentFile,
    //      Docker secret, vault-sourced `source` — never embedded in the
    //      config file itself). Audit finding H-key, v0.7.4.
    let node_wallet = match settings.secrets.node_identity_key_encrypted_path.as_deref() {
        Some(enc_path) if std::path::Path::new(enc_path).exists() => {
            Wallet::check_key_file_permissions(enc_path, settings.secrets.strict_key_permissions)?;
            let passphrase = env::var("PMS_COORDINATOR_KEY_PASSPHRASE").map_err(|_| {
                anyhow::anyhow!(
                    "encrypted coordinator key at {enc_path} requires env var \
                     PMS_COORDINATOR_KEY_PASSPHRASE to be set (empty strings \
                     not accepted)"
                )
            })?;
            if passphrase.is_empty() {
                anyhow::bail!("PMS_COORDINATOR_KEY_PASSPHRASE is set but empty");
            }
            eprintln!("[node-key] loading encrypted key from {enc_path}");
            Wallet::load_from_encrypted_file(enc_path, passphrase)?
        }
        _ => {
            Wallet::check_key_file_permissions(
                &settings.secrets.node_identity_key_path,
                settings.secrets.strict_key_permissions,
            )?;
            eprintln!(
                "[node-key] loading plain-text key from {}",
                settings.secrets.node_identity_key_path
            );
            Wallet::load_from_node_key_file(&settings.secrets.node_identity_key_path)?
        }
    };
    let node_wallet = Arc::new(node_wallet);

    let cwd = env::current_dir()?;
    eprintln!("[DEBUG] cwd             = {}", cwd.display());
    eprintln!("[DEBUG] rocks.path      = {}", settings.rocks.path);
    eprintln!(
        "[DEBUG] rocks.path abs   = {}",
        cwd.join(PathBuf::from(&settings.rocks.path)).display()
    );

    eprintln!(
        "🎛️  mode={:?} prefix={} db={}",
        settings.network.mode, settings.rocks.prefix, settings.rocks.path
    );

    // 2) Vérification TLS
    if let Some(tls) = &settings.tls {
        if settings.network.mode.is_prod() {
            // En prod : on vérifie vraiment les fichiers (using rustls native PEM support)
            use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
            use std::fs;

            let cert_pem = fs::read(&tls.cert_pem)?;
            let certs_count = CertificateDer::pem_slice_iter(&cert_pem).count();
            eprintln!("[TLS DEBUG] certs = {}", certs_count);

            let key_pem = fs::read(&tls.key_pem)?;
            match PrivateKeyDer::from_pem_slice(&key_pem) {
                Ok(_) => eprintln!("[TLS DEBUG] private key = OK"),
                Err(e) => eprintln!("[TLS DEBUG] private key error = {}", e),
            }
        } else {
            eprintln!("[TLS DEBUG] skipping TLS file checks (dev mode)");
        }
    }

    // 3) RocksDB init
    {
        use std::path::Path;
        let db_path = Path::new(&settings.rocks.path);
        if !db_path.exists() {
            eprintln!("[BOOT] RocksDB directory does NOT exist → first initialization");
        } else {
            eprintln!("[BOOT] RocksDB directory exists → opening existing DB");
        }
    }

    {
        let cwd = std::env::current_dir().unwrap();
        eprintln!("[DEBUG] cwd = {}", cwd.display());
        eprintln!(
            "[DEBUG] resolved rocks.path = {} → absolute = {}",
            settings.rocks.path,
            cwd.join(&settings.rocks.path).display()
        );
    }

    // 3b) Multi-Ledger Bootstrap
    eprintln!(
        "[BOOT] Bootstrapping {} ledger(s)...",
        settings.effective_ledgers().len()
    );
    let ledger_mgr = Arc::new(LedgerManager::bootstrap(&settings).await?);

    for instance in ledger_mgr.list_all() {
        let dag_size = instance.dag.len();
        let persisted = instance.store.block_count_estimate().await.unwrap_or(0);
        let dag_ver = instance.store.get_dag_version().await.unwrap_or_else(|_| "?".into());
        let schema_ver = instance.store.get_version().await.unwrap_or(0);
        pms_server::metrics::PMS_BLOCKS_TOTAL
            .with_label_values(&[&instance.id])
            .set(dag_size as i64);
        // Initialize the persisted counter with the actual DB count so the
        // dashboard shows the true total instead of "blocks since restart".
        pms_server::metrics::BLOCKS_PERSISTED
            .with_label_values(&[&instance.id])
            .inc_by(persisted as u64);
        eprintln!(
            "  Ledger '{}' ready (DAG: {}, persisted: {}) [DAG v{}, Schema v{}]",
            instance.id, dag_size, persisted, dag_ver, schema_ver
        );
    }

    // Use default ledger ("main") for P2P Server & backward-compat store
    let default_ledger = ledger_mgr
        .default_ledger()
        .expect("At least one ledger must exist");
    let store = default_ledger.store.clone();
    let adapter = default_ledger.adapter.clone();

    // NOTE: bootstrap_once_for_production() (flush + full compaction) removed from
    // startup — too expensive on large DBs (971K+ blocks). Background maintenance
    // in server.rs already handles periodic flush (10min) and compaction (1h).

    // 5) Server P2P (uses default ledger's adapter)
    let srv = Server::new(
        adapter.clone(),
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
        Some(ledger_mgr.clone()),
    );

    // Capture admin_token before move
    let admin_api_token = settings.auth.admin_api_token.clone();
    // Clone settings for internal API use (before partial move)
    let settings_for_internal = settings.clone();

    // Config runtime
    let cfg = ServerConfig {
        bind_addr: settings.client.as_ref().unwrap().bind_addr.clone(),
        api_addr: settings.client.as_ref().unwrap().api_addr.clone(),
        tls: settings.tls.clone(),
        api_tls_enabled: settings
            .client
            .as_ref()
            .map(|c| c.api_tls_enabled)
            .unwrap_or(true),
        network: settings.network,
        auth: settings.auth,
    };

    print_instructions(&cfg, "p2ps");

    info!("👉 P2P listening on {}", cfg.bind_addr);

    // 6) Connect to Known Peers
    let peers_str = settings.p2p.known_peers.clone();
    let peers: Vec<String> = peers_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if !peers.is_empty() {
        let srv_conn = srv.clone();
        // Capture TlsConfig directly (not ClientConfig)
        let tls_config_base = settings.tls.clone();
        let max_retries = settings.p2p.max_peer_retries;

        eprintln!("🔗 Launching connector for {} known peers...", peers.len());
        tokio::spawn(async move {
            // Petite pause pour laisser les autres nœuds démarrer
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            for p in peers {
                let srv = srv_conn.clone();
                let tls = tls_config_base.clone();
                let addr = p.replace("p2ps://", "").replace("p2p://", "");

                tokio::spawn(async move {
                    let mut attempts = 0u32;
                    let mut delay_secs = 5u64;
                    loop {
                        // Use unified connect_to_peer which handles parsing & DNS
                        let res = srv.clone().connect_to_peer(addr.clone(), tls.clone()).await;

                        match res {
                            Ok(_) => {
                                eprintln!("[P2P Connector] Connected to {}!", addr);
                                break;
                            }
                            Err(e) => {
                                attempts += 1;
                                if attempts >= max_retries {
                                    tracing::error!(
                                        peer = %addr,
                                        attempts,
                                        "Gave up connecting to peer after {} attempts: {}",
                                        max_retries, e
                                    );
                                    break;
                                }
                                eprintln!(
                                    "[P2P Connector] Failed to connect to {}: {} (attempt {}/{}). Retrying in {}s...",
                                    addr, e, attempts, max_retries, delay_secs
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                                // Exponential backoff: 5s → 10s → 20s → 40s → 60s (cap)
                                delay_secs = (delay_secs * 2).min(60);
                            }
                        }
                    }
                });
            }
        });
    }

    // 7) Periodic Global Sync (every 5s) to handle orphans/convergence
    {
        let srv_sync = srv.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                srv_sync.trigger_sync().await;
            }
        });
    }

    // 8) Auto-Launch Dashboard
    {
        let scheme = if cfg.tls.is_some() { "https" } else { "http" };
        let url = format!("{}://{}/dashboard/", scheme, cfg.api_addr);
        eprintln!("🖥️  Launching Dashboard at {}", url);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if webbrowser::open(&url).is_err() {
                eprintln!("⚠️  Failed to open browser automatically.");
            }
        });
    }

    // 9) Launcher Internal API (Engine Mode)
    if let Some(internal_addr) = &settings.client.as_ref().unwrap().internal_api_addr {
        let addr = internal_addr.clone();
        let state = pms_server::api::AppState {
            srv: srv.clone(),
            _cfg: Arc::new(cfg.clone()),
            _ready: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            stats: Arc::new(pms_server::stats::Stats::default()), // Use independent stats for internal API
            store: store.clone(),
            admin_token: admin_api_token.clone(),
            node_wallet: node_wallet.clone(),
            settings: Arc::new(settings_for_internal.clone()),
            allowed_networks: vec![], // Internal API is trusted
            treasury_wallets: pms_config::TreasuryWallets::empty(),
            node_registry: pms_server::node_registry::create_registry(),
            fee_pool: pms_server::fee_pool::create_fee_pool(),
            fee_pool_registry: std::sync::Arc::new(pms_server::fee_pool::FeePoolRegistry::new()),
            api_key_store: pms_server::api_keys::create_api_key_store(
                settings_for_internal.auth.api_keys_file.as_deref(),
            )
            .unwrap_or_else(|e| {
                tracing::error!("❌ Failed to load API keys: {}", e);
                pms_server::api_keys::create_api_key_store(None).expect("empty store must work")
            }),
            ledger_mgr: Some(ledger_mgr.clone()),
            ledger_id: "main".into(),
            effective_fees: std::sync::Arc::new(
                pms_server::api_fn::tx_helpers::resolve_effective_fees(
                    &settings_for_internal.fees,
                    None,
                ),
            ),
            activity_cache: std::sync::Arc::new(pms_server::api_fn::activity::ActivityCache::new(10_000, 30)),
            tps_tracker: std::sync::Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
            contract_event_bus: None, // Internal API doesn't need contract events
            contract_store: store.clone(),
            // Internal API runs alongside the main engine; both share the
            // same node_wallet so we don't need a separate compliance
            // lock or shard wallet derivation here. The fields exist on
            // AppState because the main router uses them — supplying
            // empty defaults keeps the internal-API state initializer
            // valid without disturbing fee-shard routing or compliance
            // gating (which only fire on the main router).
            compliance_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            coord_shard_wallets: std::sync::Arc::new(Vec::new()),
            coord_shard_round_robin: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            // Internal API shares the same read-only flag as the main
            // engine so a `Manual` arm via /admin/read-only/arm gates
            // both routers consistently. Fresh ReadOnlyMode here would
            // be a no-op anyway: the internal API doesn't gate writes
            // (it's the gateway-only path) but we keep the field in
            // sync with the type so the struct literal compiles.
            read_only: std::sync::Arc::new(pms_server::read_only::ReadOnlyMode::new()),
            // Webhooks (Phase 4) — internal API path doesn't expose
            // /admin/webhooks but the field must be present. Empty store
            // is harmless: no subscriptions = no deliveries.
            webhook_store: pms_server::api_fn::webhooks::WebhookStore::new(),
            // Emission budget gate (plan §3.1) — recovered from the shared
            // main store. `store` is cloned (not moved) above, so borrowing
            // it here is fine.
            emission_gate: std::sync::Arc::new(pms_server::emission::EmissionGate::load(&store)),
        };

        eprintln!("🔧 Launching Internal API at {}", addr);
        tokio::spawn(async move {
            if let Err(e) = pms_server::internal_api::serve_internal_api(&addr, state).await {
                eprintln!("❌ Internal API failed: {}", e);
            }
        });
    }

    // 10) Graceful shutdown: race srv.run() against SIGTERM/SIGINT
    let store_for_shutdown = store.clone();
    let cfg = Arc::new(cfg);
    // Clone the Arc<Server> for the shutdown branch (v0.7.29). srv.run()
    // takes `self: Arc<Self>` which consumes the original; without
    // this clone the shutdown branch can't call srv.adapter_arc()
    // to snapshot the persist queue depth at signal time.
    let srv_for_shutdown = srv.clone();

    tokio::select! {
        result = srv.run(cfg, store) => {
            match result {
                Ok(()) => {
                    // P2P listener returned Ok — should never happen (infinite loop).
                    // If it does, something went very wrong.
                    eprintln!("❌ FATAL: P2P listener exited unexpectedly (Ok). Shutting down.");
                    tracing::error!("P2P listener exited unexpectedly (Ok) — this should never happen");
                    std::process::exit(1);
                }
                Err(e) => {
                    // Use eprintln! (unbuffered stderr) to guarantee the message is
                    // visible even under fd exhaustion where tracing may fail to write.
                    eprintln!("❌ FATAL: Server exited with error: {}", e);
                    tracing::error!(error = %e, "Server exited with error");
                    std::process::exit(1);
                }
            }
        }
        _ = shutdown_signal() => {
            // ─────────────────────────────────────────────────────────────
            // Forensic shutdown sequence (v0.7.29) — banking-grade
            // ─────────────────────────────────────────────────────────────
            // The previous code path lost blocks on SIGTERM: the persist
            // channel buffers up to 5 K blocks in memory that the
            // background consumer hadn't flushed to RocksDB yet, but
            // the producer-side HTTP handlers had ALREADY returned
            // `Inserted` to the client. Dropping those tasks at exit
            // = silent data loss. The DAG is reconstructed from RocksDB
            // on restart; missing blocks are gone forever.
            //
            // Three fixes here:
            //
            //   1. Snapshot the persist queue depth at signal time so
            //      operators can correlate clean exits with backlog.
            //   2. Wait up to `DRAIN_DEADLINE` for every ledger's
            //      persist queue to drain to zero before flushing
            //      WALs. Best-effort: caller (Docker, k8s, operator)
            //      sets its own SIGTERM-to-SIGKILL window — typically
            //      10 s or 30 s — so we cap our drain to 25 s and
            //      leave 5 s of headroom for the WAL flush + exit.
            //   3. If the deadline elapses with blocks still queued,
            //      log an ERROR-level "data loss risk" line with the
            //      counts per ledger so the operator KNOWS which
            //      restart corresponds to lost blocks.
            //
            // This is best-effort: we don't gate new writes during the
            // drain (would need to wire AppState here). In practice
            // SIGTERM means clients are also being torn down, so new
            // writes are minimal during the drain window. A future
            // iteration can add `state.read_only.arm(Manual)` here for
            // a hard stop.
            const DRAIN_DEADLINE: std::time::Duration =
                std::time::Duration::from_secs(25);
            const DRAIN_POLL_INTERVAL: std::time::Duration =
                std::time::Duration::from_millis(100);

            // Snapshot queue depths at signal time for forensic logs.
            let mut depths_at_signal: Vec<(String, usize, usize)> = Vec::new();
            if let Some((d, c)) = srv_for_shutdown.adapter_arc().persist_queue_depth() {
                depths_at_signal.push(("main".into(), d, c));
            }
            for instance in ledger_mgr.list_all() {
                if instance.id == "main" {
                    continue;
                }
                if let Some((d, c)) = instance.adapter.persist_queue_depth() {
                    depths_at_signal.push((instance.id.clone(), d, c));
                }
            }
            let total_at_signal: usize =
                depths_at_signal.iter().map(|(_, d, _)| *d).sum();
            tracing::warn!(
                total_pending = total_at_signal,
                ledgers = ?depths_at_signal,
                "🛑 SHUTDOWN signal received — draining persist queues (deadline: 25s)"
            );
            eprintln!(
                "🛑 SHUTDOWN: {} blocks pending in persist queues (per-ledger: {:?}). Draining...",
                total_at_signal, depths_at_signal
            );

            // Poll every 100 ms until all queues are empty or the
            // deadline elapses. The consumer task drains 64-block
            // batches per iteration in ~30-50 ms, so 5 K blocks at
            // worst case = 5000 / 64 × 50 ms ≈ 3.9 s. The 25 s
            // deadline leaves 6× headroom for slow disks.
            let drain_start = std::time::Instant::now();
            loop {
                let mut all_drained = true;
                if let Some((d, _)) = srv_for_shutdown.adapter_arc().persist_queue_depth() {
                    if d > 0 {
                        all_drained = false;
                    }
                }
                if all_drained {
                    for instance in ledger_mgr.list_all() {
                        if instance.id == "main" {
                            continue;
                        }
                        if let Some((d, _)) = instance.adapter.persist_queue_depth() {
                            if d > 0 {
                                all_drained = false;
                                break;
                            }
                        }
                    }
                }
                if all_drained {
                    tracing::info!(
                        drain_ms = drain_start.elapsed().as_millis() as u64,
                        "✅ Persist queues fully drained"
                    );
                    eprintln!(
                        "✅ Persist queues drained in {} ms",
                        drain_start.elapsed().as_millis()
                    );
                    break;
                }
                if drain_start.elapsed() >= DRAIN_DEADLINE {
                    // Deadline hit. Re-snapshot what's still queued
                    // and log it loudly — these blocks ARE in the
                    // DAG RAM (client received Inserted) but will
                    // not survive the restart. This is a data-loss
                    // event and operators must know about it.
                    let mut leftover: Vec<(String, usize)> = Vec::new();
                    if let Some((d, _)) = srv_for_shutdown.adapter_arc().persist_queue_depth() {
                        if d > 0 {
                            leftover.push(("main".into(), d));
                        }
                    }
                    for instance in ledger_mgr.list_all() {
                        if instance.id == "main" {
                            continue;
                        }
                        if let Some((d, _)) = instance.adapter.persist_queue_depth() {
                            if d > 0 {
                                leftover.push((instance.id.clone(), d));
                            }
                        }
                    }
                    let lost: usize = leftover.iter().map(|(_, d)| *d).sum();
                    tracing::error!(
                        target = "shutdown_data_loss",
                        blocks_lost = lost,
                        ledgers = ?leftover,
                        drain_deadline_secs = DRAIN_DEADLINE.as_secs(),
                        "⚠️ DATA LOSS: {} blocks still pending after {}s drain deadline. \
                         These blocks were acknowledged to clients (Inserted) but never \
                         persisted to RocksDB — they will NOT survive the restart.",
                        lost, DRAIN_DEADLINE.as_secs()
                    );
                    eprintln!(
                        "⚠️  DATA LOSS RISK: {} blocks pending after deadline. Per-ledger: {:?}",
                        lost, leftover
                    );
                    break;
                }
                tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
            }

            // Now flush all RocksDB WALs (data we already wrote).
            tracing::info!("Flushing RocksDB WALs...");
            for instance in ledger_mgr.list_all() {
                if let Err(e) = instance.store.flush_wal().await {
                    tracing::error!(ledger = %instance.id, error = %e, "Failed to flush WAL on shutdown");
                } else {
                    tracing::info!(ledger = %instance.id, "WAL flushed");
                }
            }
            // Also flush the default store (same DB, but ensures WAL is synced)
            if let Err(e) = store_for_shutdown.flush_wal().await {
                tracing::error!(error = %e, "Failed to flush main store WAL on shutdown");
            }

            let total_shutdown_ms = drain_start.elapsed().as_millis();
            eprintln!(
                "✅ Graceful shutdown complete in {} ms (drain: {} ms + WAL flush)",
                total_shutdown_ms,
                drain_start.elapsed().as_millis()
            );
            tracing::info!(
                total_shutdown_ms = total_shutdown_ms as u64,
                "Graceful shutdown complete"
            );
        }
    }

    Ok(())
}

/// Wait for SIGTERM (Docker stop) or SIGINT (Ctrl+C).
///
/// **Forensic logging** (v0.7.29): logs at WARN level so operators can
/// correlate clean exits with their causes. Stack trace via
/// `std::backtrace::Backtrace::capture()` would be over-kill for a
/// signal that's normal in production (Docker stop, k8s pod cycle),
/// but we tag it loudly enough that grep-based incident analysis
/// finds the cause without having to parse `docker inspect` exit
/// codes ex-post.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
        let mut sighup =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("failed to register SIGHUP handler");
        tokio::select! {
            _ = ctrl_c => {
                tracing::warn!(
                    target = "shutdown_signal",
                    signal = "SIGINT",
                    "Received SIGINT (Ctrl+C) — operator-initiated"
                );
                eprintln!("📥 Received SIGINT (Ctrl+C)");
            }
            _ = sigterm.recv() => {
                tracing::warn!(
                    target = "shutdown_signal",
                    signal = "SIGTERM",
                    "Received SIGTERM — Docker stop / k8s pod cycle / operator restart / \
                     `upgrade-testnet.sh` are the usual senders"
                );
                eprintln!("📥 Received SIGTERM");
            }
            _ = sighup.recv() => {
                tracing::warn!(
                    target = "shutdown_signal",
                    signal = "SIGHUP",
                    "Received SIGHUP — typically a controlling terminal closed"
                );
                eprintln!("📥 Received SIGHUP");
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        tracing::info!("Received Ctrl+C");
    }
}

fn init_logging() {
    static START: Once = Once::new();
    START.call_once(|| {
        // redirige `log` -> `tracing` (ignore si déjà fait)
        let _ = tracing_log::LogTracer::init();

        let filter = EnvFilter::try_from_env("RUST_LOG")
            .unwrap_or_else(|_| EnvFilter::new("info,pms_stats=info"));
        // n’échoue pas si déjà initialisé ailleurs
        let _ = fmt()
            .with_env_filter(filter)
            .with_target(true)
            .with_ansi(false)
            .json()
            .flatten_event(true)
            .try_init();
    });
}

fn print_instructions(cfg: &ServerConfig, p2p_scheme: &str) {
    use owo_colors::OwoColorize;
    eprintln!();
    eprintln!("{}", "🚀 PMS Node démarré !".bold().green());

    eprintln!("{}", "Endpoints P2P :".bright_blue().bold());
    eprintln!(
        "   ▶ Bind : {}://{}",
        p2p_scheme.yellow(),
        cfg.bind_addr.yellow()
    );

    eprintln!("{}", "Endpoints Health :".bright_blue().bold());

    let scheme = if cfg.tls.is_some() { "https" } else { "http" };
    eprintln!(
        "   ▶ Live  : {}",
        format!("{}://{}/live", scheme, cfg.api_addr).yellow()
    );
    eprintln!(
        "   ▶ Ready : {}",
        format!("{}://{}/ready", scheme, cfg.api_addr).yellow()
    );

    eprintln!();
    eprintln!(
        "{}",
        "Pour lancer le CLI interactif :".bright_magenta().bold()
    );
    eprintln!("   {}", "docker compose run --rm cli sh".cyan());
    eprintln!("   puis : {}", "tools-cli".cyan().bold());
    eprintln!();
}
