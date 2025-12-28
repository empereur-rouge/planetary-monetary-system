use std::env;
use std::path::PathBuf;
use anyhow::{Result};
use std::sync::{Arc, Once};
use rustls::crypto::{ring};
use tokio::sync::Mutex;
use tracing::info;
use pms_core::dag::Dag;
use pms_core::CoreAdapter;
use pms_storage::{DagStorage, StoredBlock};
use pms_interface::NetDagAdapter;
use pms_server::{Server};
use tracing_subscriber::{EnvFilter, fmt};
use clap::Parser;
use pms_config::{load_config, load_config_with, ServerConfig};
use pms_storage::rocks_store::store::RocksStore;
use pms_types_block::Block;
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wire::WireMeta;

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
    let node_wallet = Wallet::load_from_node_key_file(
        &settings.secrets.node_identity_key_path,
    )?;
    let node_wallet = Arc::new(node_wallet);

    let cwd = env::current_dir()?;
    eprintln!("[DEBUG] cwd             = {}", cwd.display());
    eprintln!("[DEBUG] rocks.path      = {}", settings.rocks.path);
    eprintln!(
        "[DEBUG] rocks.path abs   = {}",
        cwd.join(PathBuf::from(&settings.rocks.path)).display()
    );

    eprintln!("🎛️  mode={:?} prefix={} db={}",
              settings.network.mode,
              settings.rocks.prefix,
              settings.rocks.path
    );

    // 2) Vérification TLS
    if let Some(tls) = &settings.tls {
        if settings.network.mode.is_prod() {
            // En prod : on vérifie vraiment les fichiers
            use rustls_pemfile::{pkcs8_private_keys, ec_private_keys, certs};
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
        ).await?
    );

    if let Err(e) = store.ensure_schema().await {
        eprintln!("[BOOT] Rocks ensure_schema failed: {e}");
    }

    if let Err(e) = store.bootstrap_once_for_production() {
        eprintln!("[BOOT] bootstrap_once_for_production skipped → {e}");
    }

    eprintln!("✅ Opened RocksDB");

    // 4) DAG
    let ids = store.all_block_ids().await?;
    if ids.is_empty() {
        let g = Block::genesis(compute_block_id);

        let meta = WireMeta {
            network_id: settings.network.network_id.clone(),
            protocol_version: settings.network.protocol_version,
        };

        store.persist_genesis(&g, &meta).await?;
    }

    let dag_loaded = Dag::bootstrap_from_store(&*store).await?;
    let dag = Arc::new(Mutex::new(dag_loaded));

    // 5) Adapter & Server
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());
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
        tls: settings.tls,
        network: settings.network,
        auth: settings.auth,
    };

    print_instructions(&cfg, "p2ps");

    info!("👉 P2P listening on {}", cfg.bind_addr);

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
    eprintln!("   ▶ Bind : {}://{}", p2p_scheme.yellow(), cfg.bind_addr.yellow());

    eprintln!("{}", "Endpoints Health :".bright_blue().bold());
    
    let scheme = if cfg.tls.is_some() { "https" } else { "http" };
    eprintln!("   ▶ Live  : {}", format!("{}://{}/live",  scheme, cfg.api_addr).yellow());
    eprintln!("   ▶ Ready : {}", format!("{}://{}/ready", scheme, cfg.api_addr).yellow());

    eprintln!();
    eprintln!("{}", "Pour lancer le CLI interactif :".bright_magenta().bold());
    eprintln!("   {}", "docker compose run --rm cli sh".cyan());
    eprintln!("   puis : {}", "tools-cli".cyan().bold());
    eprintln!();
}