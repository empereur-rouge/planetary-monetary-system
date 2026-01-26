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
            // En prod : on vérifie vraiment les fichiers
            use rustls_pemfile::{certs, ec_private_keys, pkcs8_private_keys};
            use std::{fs::File, io::BufReader};

            let mut cr = BufReader::new(File::open(&tls.cert_pem)?);
            let certs_count = certs(&mut cr).count();
            eprintln!("[TLS DEBUG] certs = {}", certs_count);

            let mut kr = BufReader::new(File::open(&tls.key_pem)?);
            let pk8_count = pkcs8_private_keys(&mut kr).count();
            eprintln!("[TLS DEBUG] pkcs8 keys = {}", pk8_count);

            let mut kr2 = BufReader::new(File::open(&tls.key_pem)?);
            let ec_count = ec_private_keys(&mut kr2).count();
            eprintln!("[TLS DEBUG] ec keys = {}", ec_count);
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
    );

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
        let tls_cfg = if let Some(tls) = &settings.tls {
            Some(Arc::new(pms_server::tls::load_client_config(
                &tls.cert_pem,
                &tls.key_pem,
                tls.ca_pem.as_deref(),
            )?))
        } else {
            None
        };

        eprintln!("🔗 Launching connector for {} known peers...", peers.len());
        tokio::spawn(async move {
            // Petite pause pour laisser les autres nœuds démarrer
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            for p in peers {
                let srv = srv_conn.clone();
                let tls = tls_cfg.clone();
                let addr = p.replace("p2ps://", "").replace("p2p://", "");

                tokio::spawn(async move {
                    loop {
                        let res = if let Some(t) = &tls {
                            srv.connect_tls(&addr, t.clone()).await
                        } else {
                            srv.connect(&addr).await
                        };

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
