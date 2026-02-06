use anyhow::Result;
use clap::Parser;

use pms_config::{ServerConfig, load_config};
use pms_core::CoreAdapter;
use pms_core::concurrent_dag::ConcurrentDag;
use pms_interface::NetDagAdapter;
use pms_server::Server;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types_block::Block;
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wire::WireMeta;
use rustls::crypto::ring;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Once};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser, Debug)]
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

    // 1) Settings
    let settings = load_config()?;
    let node_wallet = Wallet::load_from_node_key_file(&settings.secrets.node_identity_key_path)?;
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

    let store = Arc::new(
        RocksStore::new(
            &settings.rocks.path,
            settings.rocks.tip_limit,
            settings.rocks.prefix.clone(),
            settings.rocks.checkpoint_interval_secs,
        )
        .await?,
    );

    if let Err(e) = store.ensure_schema().await {
        eprintln!("[BOOT] Rocks ensure_schema failed: {e}");
    }

    if let Err(e) = store.bootstrap_once_for_production() {
        eprintln!("[BOOT] bootstrap_once_for_production skipped → {e}");
    }

    eprintln!("✅ Opened RocksDB");

    // 4) Concurrent DAG
    let ids = store.all_block_ids().await?;
    if ids.is_empty() {
        let g = Block::genesis(compute_block_id);

        let meta = WireMeta {
            network_id: settings.network.network_id.clone(),
            protocol_version: settings.network.protocol_version,
        };

        store.persist_genesis(&g, &meta).await?;
    }

    // Load DAG from store into concurrent structure (RAM)
    println!("[BOOT] Loading concurrent DAG from RocksDB...");
    let dag = Arc::new(ConcurrentDag::bootstrap_from_store(&*store).await?);
    println!("[BOOT] Loaded {} blocks into DAG", dag.len());

    // 5) Adapter & Server
    let core_adapter = CoreAdapter::new(dag.clone(), store.clone());

    // Bootstrapping UTXO (Sharding Phase 4)
    eprintln!("[BOOT] Bootstrapping Sharded UTXO set...");
    core_adapter.bootstrap_utxos().await?;

    let adapter: Arc<dyn NetDagAdapter> = core_adapter;
    let srv = Server::new(
        adapter.clone(),
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
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

        eprintln!("🔗 Launching connector for {} known peers...", peers.len());
        tokio::spawn(async move {
            // Petite pause pour laisser les autres nœuds démarrer
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            for p in peers {
                let srv = srv_conn.clone();
                let tls = tls_config_base.clone();
                let addr = p.replace("p2ps://", "").replace("p2p://", "");

                tokio::spawn(async move {
                    loop {
                        // Use unified connect_to_peer which handles parsing & DNS
                        let res = srv.clone().connect_to_peer(addr.clone(), tls.clone()).await;

                        match res {
                            Ok(_) => {
                                eprintln!("[P2P Connector] Connected to {}!", addr);
                                break;
                            }
                            Err(e) => {
                                eprintln!(
                                    "[P2P Connector] Failed to connect to {}: {}. Retrying in 5s...",
                                    addr, e
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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
        };

        eprintln!("🔧 Launching Internal API at {}", addr);
        tokio::spawn(async move {
            if let Err(e) = pms_server::internal_api::serve_internal_api(&addr, state).await {
                eprintln!("❌ Internal API failed: {}", e);
            }
        });
    }

    srv.run(Arc::new(cfg), store).await?;

    Ok(())
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
